use crate::{Contribution, FlowError, FlowTotals, Metric, RemovalAttribution};
use common::Timestamp;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowConfig {
    pub event_window: usize,
    pub time_window_ns: u64,
    pub qi_levels: usize,
    pub removal_attribution: RemovalAttribution,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Entry {
    time: Timestamp,
    contribution: Contribution,
    bucket: Option<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Window<const W: usize, const B: usize> {
    entries: Box<[Entry; W]>,
    head: usize,
    len: usize,
    total: FlowTotals,
    buckets: [FlowTotals; B],
}
impl<const W: usize, const B: usize> Window<W, B> {
    fn new() -> Self {
        Self {
            entries: vec![Entry::default(); W]
                .into_boxed_slice()
                .try_into()
                .expect("fixed capacity"),
            head: 0,
            len: 0,
            total: FlowTotals::default(),
            buckets: [FlowTotals::default(); B],
        }
    }
    fn clear(&mut self) {
        self.entries.fill(Entry::default());
        self.head = 0;
        self.len = 0;
        self.total = FlowTotals::default();
        self.buckets = [FlowTotals::default(); B];
    }
    fn pop(&mut self) {
        let e = self.entries[self.head];
        self.total.accumulate(e.contribution.totals(), -1);
        if let Some(b) = e.bucket {
            self.buckets[b].accumulate(e.contribution.totals(), -1);
        }
        self.entries[self.head] = Entry::default();
        self.head = (self.head + 1) % W;
        self.len -= 1;
    }
    fn push(&mut self, entry: Entry) {
        self.entries[(self.head + self.len) % W] = entry;
        self.len += 1;
        self.total.accumulate(entry.contribution.totals(), 1);
        if let Some(b) = entry.bucket {
            self.buckets[b].accumulate(entry.contribution.totals(), 1);
        }
    }
    fn expire(&mut self, now: Timestamp, horizon: u64) {
        while self.len > 0 && now.0 - self.entries[self.head].time.0 >= horizon {
            self.pop();
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowSnapshot<const B: usize> {
    pub event_totals: FlowTotals,
    pub time_totals: FlowTotals,
    pub event_buckets: [FlowTotals; B],
    pub time_buckets: [FlowTotals; B],
    pub event_count: usize,
    pub time_count: usize,
    pub exposure_ns: u64,
    pub attribution: RemovalAttribution,
}
impl<const B: usize> FlowSnapshot<B> {
    pub fn time_rate(self, metric: Metric) -> Result<Option<i128>, FlowError> {
        if self.attribution == RemovalAttribution::Unknown
            && matches!(metric, Metric::CancelEvents | Metric::CancelQty)
        {
            return Ok(None);
        }
        self.time_totals.rate(metric, self.exposure_ns)
    }
    pub fn bucket_time_rate(
        self,
        bucket: usize,
        metric: Metric,
    ) -> Result<Option<i128>, FlowError> {
        let totals = self
            .time_buckets
            .get(bucket)
            .ok_or(FlowError::InvalidObservation)?;
        if self.attribution == RemovalAttribution::Unknown
            && matches!(metric, Metric::CancelEvents | Metric::CancelQty)
        {
            return Ok(None);
        }
        totals.rate(metric, self.exposure_ns)
    }
    /// Unknown attribution is distinct from an observed zero cancellation rate.
    pub fn cancellation_rate(self) -> Result<Option<i128>, FlowError> {
        if self.attribution == RemovalAttribution::Unknown {
            return Ok(None);
        }
        self.time_rate(Metric::CancelEvents)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowEngine<const W: usize, const B: usize> {
    config: FlowConfig,
    events: Window<W, B>,
    time: Window<W, B>,
    started: Timestamp,
    now: Timestamp,
    faulted: bool,
}
impl<const W: usize, const B: usize> FlowEngine<W, B> {
    pub fn validate(config: FlowConfig) -> Result<(), FlowError> {
        if W == 0
            || W > 4096
            || B == 0
            || B > 64
            || config.event_window == 0
            || config.event_window > W
            || config.time_window_ns == 0
            || config.qi_levels == 0
        {
            return Err(FlowError::InvalidConfig);
        }
        Ok(())
    }
    pub fn new(config: FlowConfig, started: Timestamp) -> Result<Self, FlowError> {
        Self::validate(config)?;
        Ok(Self {
            config,
            events: Window::new(),
            time: Window::new(),
            started,
            now: started,
            faulted: false,
        })
    }
    /// Start a new observation epoch using the already allocated buffers.
    pub fn reset(&mut self, now: Timestamp) -> Result<(), FlowError> {
        if now < self.now {
            self.faulted = true;
            return Err(FlowError::ClockRegression);
        }
        self.events.clear();
        self.time.clear();
        self.started = now;
        self.now = now;
        self.faulted = false;
        Ok(())
    }
    pub fn advance_time(&mut self, now: Timestamp) -> Result<(), FlowError> {
        if self.faulted {
            return Err(FlowError::Faulted);
        }
        if now < self.now {
            self.faulted = true;
            return Err(FlowError::ClockRegression);
        }
        self.now = now;
        self.time.expire(now, self.config.time_window_ns);
        Ok(())
    }
    pub fn record(
        &mut self,
        now: Timestamp,
        contribution: Contribution,
        bucket: Option<usize>,
    ) -> Result<(), FlowError> {
        self.advance_time(now)?;
        if contribution
            .attribution
            .is_some_and(|a| a != self.config.removal_attribution)
            || bucket.is_some_and(|b| b >= B)
        {
            self.faulted = true;
            return Err(FlowError::InvalidObservation);
        }
        if self.time.len == W {
            self.faulted = true;
            return Err(FlowError::Capacity);
        }
        if self.events.len == self.config.event_window {
            self.events.pop();
        }
        let e = Entry {
            time: now,
            contribution,
            bucket,
        };
        self.events.push(e);
        self.time.push(e);
        Ok(())
    }
    pub fn snapshot(&self) -> Result<FlowSnapshot<B>, FlowError> {
        if self.faulted {
            return Err(FlowError::Faulted);
        }
        Ok(FlowSnapshot {
            event_totals: self.events.total,
            time_totals: self.time.total,
            event_buckets: self.events.buckets,
            time_buckets: self.time.buckets,
            event_count: self.events.len,
            time_count: self.time.len,
            exposure_ns: (self.now.0 - self.started.0).min(self.config.time_window_ns),
            attribution: self.config.removal_attribution,
        })
    }
}
