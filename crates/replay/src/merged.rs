//! Bounded k-way merge within one explicitly shared local monotonic clock domain.
use common::{Side, VenueId};
use consolidator::{ConsolidationError, Consolidator};
use market_events::MarketEvent;
use recorder::{RecordingMetadata, RecordingReader};
use std::io::{self, Read};
#[derive(Debug)]
pub enum MergeError {
    Io(io::Error),
    DuplicateVenue,
    MetadataMismatch,
    ClockDomainRequired,
    Consolidation(ConsolidationError),
    IncompleteSnapshot,
}
impl From<io::Error> for MergeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<ConsolidationError> for MergeError {
    fn from(e: ConsolidationError) -> Self {
        Self::Consolidation(e)
    }
}
pub struct MergedReplay<R, const V: usize> {
    readers: [RecordingReader<R>; V],
    heads: [Option<MarketEvent>; V],
    refill: Option<usize>,
    failed: bool,
}
impl<R: Read, const V: usize> MergedReplay<R, V> {
    /// v1 has no clock-domain metadata. Explicit attestation is mandatory.
    pub fn new(inputs: [R; V], shared_clock_domain: bool) -> Result<Self, MergeError> {
        if !shared_clock_domain || V == 0 {
            return Err(MergeError::ClockDomainRequired);
        }
        // Initialization only: no allocations while merging or applying events.
        let readers: Vec<_> = inputs
            .into_iter()
            .map(RecordingReader::new)
            .collect::<io::Result<_>>()?;
        let mut readers: [RecordingReader<R>; V] = readers
            .try_into()
            .map_err(|_| MergeError::MetadataMismatch)?;
        for i in 0..V {
            if readers[..i]
                .iter()
                .any(|r| r.metadata.venue == readers[i].metadata.venue)
            {
                return Err(MergeError::DuplicateVenue);
            }
        }
        let mut heads = [None; V];
        for i in 0..V {
            heads[i] = readers[i].next_event()?;
        }
        Ok(Self {
            readers,
            heads,
            refill: None,
            failed: false,
        })
    }
    pub fn metadata(&self) -> [RecordingMetadata; V] {
        std::array::from_fn(|i| self.readers[i].metadata)
    }
    fn next_inner(&mut self) -> Result<Option<MarketEvent>, MergeError> {
        if let Some(i) = self.refill.take() {
            self.heads[i] = self.readers[i].next_event()?;
        }
        let i = (0..V)
            .filter(|&i| self.heads[i].is_some())
            .min_by_key(|&i| {
                let e = self.heads[i].expect("filtered");
                (e.receive_ts.0, e.venue.0, e.sequence)
            });
        let Some(i) = i else { return Ok(None) };
        self.refill = Some(i);
        Ok(self.heads[i].take())
    }
    pub fn next_event(&mut self) -> Result<Option<MarketEvent>, MergeError> {
        if self.failed {
            return Err(MergeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "merged replay poisoned",
            )));
        }
        let result = self.next_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn validate<const N: usize, const G: usize>(
        &self,
        engine: &Consolidator<V, N, G>,
    ) -> Result<(), MergeError> {
        for r in &self.readers {
            let m = r.metadata;
            let config = engine.venue_config(m.venue)?;
            let native = config.metadata;
            if m.instrument != native.instrument
                || m.price_decimals != native.price.decimals
                || m.tick_atoms != native.price.atoms
                || m.quantity_decimals != native.quantity.decimals
                || m.quantity_atoms != native.quantity.atoms
            {
                return Err(MergeError::MetadataMismatch);
            }
        }
        Ok(())
    }
    /// Maximum-throughput merge. Call next_event + engine.apply for caller-controlled stepping.
    pub fn run<const N: usize, const G: usize>(
        &mut self,
        engine: &mut Consolidator<V, N, G>,
    ) -> Result<u64, MergeError> {
        self.validate(engine)?;
        let mut count = 0;
        while let Some(e) = self.next_event()? {
            engine.apply(&e)?;
            count += 1;
        }
        for m in self.metadata() {
            let state = engine.venue_book(VenueId(m.venue.0))?.state();
            if matches!(
                state,
                book::BookState::AwaitingSnapshot | book::BookState::BuildingSnapshot
            ) {
                return Err(MergeError::IncompleteSnapshot);
            }
        }
        engine.levels(Side::Buy)?;
        Ok(count)
    }
}
/// One merged step: a single event, or an atomic batch to apply with `apply_depth_batch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergedFrame<'a> {
    Event(MarketEvent),
    Batch(&'a [MarketEvent]),
}
/// Frame-aware k-way merge for version 1 and 2 recordings. Order is
/// `(receive_ts, venue, first canonical sequence)`; batch boundaries are preserved.
/// One `MAX_BATCH` buffer per reader is allocated at construction, never while merging.
pub struct MergedFrames<R, const V: usize> {
    readers: [RecordingReader<R>; V],
    heads: [Option<recorder::Frame>; V],
    buffers: [Box<[MarketEvent]>; V],
    refill: Option<usize>,
    failed: bool,
}
impl<R: Read, const V: usize> MergedFrames<R, V> {
    pub fn new(inputs: [R; V], shared_clock_domain: bool) -> Result<Self, MergeError> {
        if !shared_clock_domain || V == 0 {
            return Err(MergeError::ClockDomainRequired);
        }
        let readers: Vec<_> = inputs
            .into_iter()
            .map(RecordingReader::new)
            .collect::<io::Result<_>>()?;
        let mut readers: [RecordingReader<R>; V] = readers
            .try_into()
            .map_err(|_| MergeError::MetadataMismatch)?;
        for i in 0..V {
            if readers[..i]
                .iter()
                .any(|r| r.metadata.venue == readers[i].metadata.venue)
            {
                return Err(MergeError::DuplicateVenue);
            }
        }
        let blank = MarketEvent {
            venue: VenueId(0),
            instrument: common::InstrumentId(0),
            sequence: 0,
            exchange_sequence: 0,
            exchange_ts: 0,
            receive_ts: common::Timestamp(0),
            event_type: market_events::MarketEventType::Add,
            side: Side::Buy,
            price_ticks: fixed_point::PriceTicks(0),
            qty_units: fixed_point::QtyUnits(0),
        };
        let mut buffers: [Box<[MarketEvent]>; V] =
            std::array::from_fn(|_| vec![blank; recorder::MAX_BATCH].into_boxed_slice());
        let mut heads = [None; V];
        for i in 0..V {
            heads[i] = readers[i].next_frame(&mut buffers[i])?;
        }
        Ok(Self {
            readers,
            heads,
            buffers,
            refill: None,
            failed: false,
        })
    }
    pub fn metadata(&self) -> [RecordingMetadata; V] {
        std::array::from_fn(|i| self.readers[i].metadata)
    }
    fn key(&self, i: usize) -> Option<(u64, u16, u64)> {
        let first = match self.heads[i]? {
            recorder::Frame::Event(e) => e,
            recorder::Frame::Batch(_) => self.buffers[i][0],
        };
        Some((first.receive_ts.0, first.venue.0, first.sequence))
    }
    pub fn next_frame(&mut self) -> Result<Option<MergedFrame<'_>>, MergeError> {
        if self.failed {
            return Err(MergeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "merged replay poisoned",
            )));
        }
        if let Some(i) = self.refill.take() {
            match self.readers[i].next_frame(&mut self.buffers[i]) {
                Ok(frame) => self.heads[i] = frame,
                Err(e) => {
                    self.failed = true;
                    return Err(e.into());
                }
            }
        }
        let Some(i) = (0..V)
            .filter_map(|i| self.key(i).map(|k| (k, i)))
            .min()
            .map(|(_, i)| i)
        else {
            return Ok(None);
        };
        self.refill = Some(i);
        Ok(Some(match self.heads[i].take().expect("keyed head") {
            recorder::Frame::Event(e) => MergedFrame::Event(e),
            recorder::Frame::Batch(n) => MergedFrame::Batch(&self.buffers[i][..n]),
        }))
    }
    pub fn validate<const N: usize, const G: usize>(
        &self,
        engine: &Consolidator<V, N, G>,
    ) -> Result<(), MergeError> {
        for r in &self.readers {
            let m = r.metadata;
            let native = engine.venue_config(m.venue)?.metadata;
            if m.instrument != native.instrument
                || m.price_decimals != native.price.decimals
                || m.tick_atoms != native.price.atoms
                || m.quantity_decimals != native.quantity.decimals
                || m.quantity_atoms != native.quantity.atoms
            {
                return Err(MergeError::MetadataMismatch);
            }
        }
        Ok(())
    }
}
