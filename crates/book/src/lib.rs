//! Single-owner bounded L2 book. Sorted arrays; O(log N) lookup, O(N) insertion.
//! Capacity exhaustion invalidates the book; depth is never silently discarded.
#![forbid(unsafe_code)]
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as Kind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Level {
    pub price: PriceTicks,
    pub qty: QtyUnits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookError {
    WrongMarket,
    SequenceGap,
    ClockRegression,
    InvalidPrice,
    InvalidQuantity,
    DuplicateLevel,
    MissingLevel,
    Capacity,
    Crossed,
    SnapshotRequired,
    InvalidSnapshot,
    InvalidBatch,
    Stale,
    Disconnected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookState {
    AwaitingSnapshot,
    BuildingSnapshot,
    Live,
    Invalid(BookError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ladder<const N: usize> {
    levels: [Level; N],
    len: usize,
    side: Side,
}
impl<const N: usize> Ladder<N> {
    fn new(side: Side) -> Self {
        Self {
            levels: [Level::default(); N],
            len: 0,
            side,
        }
    }
    fn view(&self) -> &[Level] {
        &self.levels[..self.len]
    }
    fn locate(&self, price: PriceTicks) -> Result<usize, usize> {
        self.view().binary_search_by(|level| match self.side {
            Side::Buy => price.cmp(&level.price),
            Side::Sell => level.price.cmp(&price),
        })
    }
    fn apply(&mut self, event: &MarketEvent) -> Result<(), BookError> {
        let location = self.locate(event.price_ticks);
        match event.event_type {
            Kind::Add => {
                let index = location.err().ok_or(BookError::DuplicateLevel)?;
                if self.len == N {
                    return Err(BookError::Capacity);
                }
                self.levels.copy_within(index..self.len, index + 1);
                self.levels[index] = Level {
                    price: event.price_ticks,
                    qty: event.qty_units,
                };
                self.len += 1;
            }
            Kind::Modify => {
                self.levels[location.map_err(|_| BookError::MissingLevel)?].qty = event.qty_units
            }
            Kind::Cancel => {
                let index = location.map_err(|_| BookError::MissingLevel)?;
                self.levels.copy_within(index + 1..self.len, index);
                self.len -= 1;
                self.levels[self.len] = Level::default();
            }
            _ => unreachable!("caller dispatches only depth events"),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ladders<const N: usize> {
    bids: Ladder<N>,
    asks: Ladder<N>,
}
impl<const N: usize> Ladders<N> {
    fn new() -> Self {
        Self {
            bids: Ladder::new(Side::Buy),
            asks: Ladder::new(Side::Sell),
        }
    }
    fn crossed(&self) -> bool {
        matches!((self.bids.view().first(), self.asks.view().first()), (Some(b), Some(a)) if b.price >= a.price)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueBook<const N: usize> {
    venue: VenueId,
    instrument: InstrumentId,
    active: Ladders<N>,
    staging: Ladders<N>,
    state: BookState,
    sequence: Option<u64>,
    receive: Option<Timestamp>,
    exchange_ts: u64,
}
impl<const N: usize> VenueBook<N> {
    pub fn new(venue: VenueId, instrument: InstrumentId) -> Self {
        Self {
            venue,
            instrument,
            active: Ladders::new(),
            staging: Ladders::new(),
            state: BookState::AwaitingSnapshot,
            sequence: None,
            receive: None,
            exchange_ts: 0,
        }
    }
    pub fn state(&self) -> BookState {
        self.state
    }
    pub fn venue(&self) -> VenueId {
        self.venue
    }
    pub fn instrument(&self) -> InstrumentId {
        self.instrument
    }
    pub fn sequence(&self) -> Option<u64> {
        self.sequence
    }
    pub fn receive_timestamp(&self) -> Option<Timestamp> {
        self.receive
    }
    pub fn exchange_timestamp(&self) -> u64 {
        self.exchange_ts
    }
    pub fn levels(&self, side: Side) -> Result<&[Level], BookError> {
        if self.state != BookState::Live {
            return Err(BookError::SnapshotRequired);
        }
        Ok(match side {
            Side::Buy => self.active.bids.view(),
            Side::Sell => self.active.asks.view(),
        })
    }
    /// Last committed depth for audit/aggregate withdrawal ONLY. It may be stale or
    /// invalid; trading callers must use `levels`, which checks book validity.
    pub fn last_committed_levels(&self, side: Side) -> &[Level] {
        match side {
            Side::Buy => self.active.bids.view(),
            Side::Sell => self.active.asks.view(),
        }
    }
    pub fn best(&self, side: Side) -> Result<Option<Level>, BookError> {
        Ok(self.levels(side)?.first().copied())
    }
    pub fn level(&self, side: Side, price: PriceTicks) -> Result<Option<Level>, BookError> {
        self.levels(side)?;
        let ladder = match side {
            Side::Buy => &self.active.bids,
            Side::Sell => &self.active.asks,
        };
        Ok(ladder.locate(price).ok().map(|i| ladder.levels[i]))
    }
    /// External session health can withdraw book validity without altering its ladders.
    pub fn invalidate(&mut self, reason: BookError) {
        self.state = BookState::Invalid(reason);
    }

    /// Atomic depth message: bounded scratch copy, no intermediate crossed-book checks.
    /// All members must belong to the same receive timestamp and native update ID.
    pub fn apply_depth_batch(&mut self, events: &[MarketEvent]) -> Result<(), BookError> {
        if events
            .iter()
            .any(|e| e.venue != self.venue || e.instrument != self.instrument)
        {
            return Err(BookError::WrongMarket);
        }
        let result = self.batch_inner(events);
        if let Err(error) = result {
            self.state = BookState::Invalid(error);
        }
        result
    }
    fn batch_inner(&mut self, events: &[MarketEvent]) -> Result<(), BookError> {
        let first = events.first().ok_or(BookError::InvalidBatch)?;
        if self.state != BookState::Live {
            return Err(BookError::SnapshotRequired);
        }
        if events.len() > N.saturating_mul(2) {
            return Err(BookError::InvalidBatch);
        }
        self.staging.clone_from(&self.active);
        let mut sequence = self.sequence;
        let mut receive = self.receive;
        for e in events {
            if e.venue != self.venue || e.instrument != self.instrument {
                return Err(BookError::WrongMarket);
            }
            if e.receive_ts != first.receive_ts
                || e.exchange_sequence != first.exchange_sequence
                || !matches!(e.event_type, Kind::Add | Kind::Modify | Kind::Cancel)
            {
                return Err(BookError::InvalidBatch);
            }
            if sequence.and_then(|s| s.checked_add(1)) != Some(e.sequence) {
                return Err(BookError::SequenceGap);
            }
            if receive.is_some_and(|t| e.receive_ts < t) {
                return Err(BookError::ClockRegression);
            }
            if e.price_ticks.0 <= 0 {
                return Err(BookError::InvalidPrice);
            }
            if (e.event_type == Kind::Cancel && e.qty_units.0 != 0)
                || (e.event_type != Kind::Cancel && e.qty_units.0 <= 0)
            {
                return Err(BookError::InvalidQuantity);
            }
            match e.side {
                Side::Buy => self.staging.bids.apply(e)?,
                Side::Sell => self.staging.asks.apply(e)?,
            }
            sequence = Some(e.sequence);
            receive = Some(e.receive_ts);
        }
        if self.staging.crossed() {
            return Err(BookError::Crossed);
        }
        core::mem::swap(&mut self.active, &mut self.staging);
        self.sequence = sequence;
        self.receive = receive;
        self.exchange_ts = events.last().expect("nonempty batch").exchange_ts;
        Ok(())
    }
    /// Doubled midpoint preserves half-ticks exactly and avoids i64 addition overflow.
    pub fn midpoint_x2(&self) -> Result<Option<i128>, BookError> {
        Ok(match (self.best(Side::Buy)?, self.best(Side::Sell)?) {
            (Some(b), Some(a)) => Some(i128::from(b.price.0) + i128::from(a.price.0)),
            _ => None,
        })
    }
    pub fn spread(&self) -> Result<Option<i64>, BookError> {
        Ok(match (self.best(Side::Buy)?, self.best(Side::Sell)?) {
            (Some(b), Some(a)) => Some(a.price.0 - b.price.0),
            _ => None,
        })
    }
    pub fn depth(&self, side: Side, top_n: usize) -> Result<i128, BookError> {
        Ok(self
            .levels(side)?
            .iter()
            .take(top_n)
            .map(|l| i128::from(l.qty.0))
            .sum())
    }
    pub fn apply(&mut self, event: &MarketEvent) -> Result<(), BookError> {
        // A routing error must not poison an unrelated market's book.
        if event.venue != self.venue || event.instrument != self.instrument {
            return Err(BookError::WrongMarket);
        }
        match self.apply_inner(event) {
            Ok(()) => {
                self.sequence = Some(event.sequence);
                self.receive = Some(event.receive_ts);
                self.exchange_ts = event.exchange_ts;
                Ok(())
            }
            Err(error) => {
                self.state = BookState::Invalid(error);
                Err(error)
            }
        }
    }
    fn apply_inner(&mut self, e: &MarketEvent) -> Result<(), BookError> {
        if self.receive.is_some_and(|last| e.receive_ts < last) {
            return Err(BookError::ClockRegression);
        }
        if e.event_type == Kind::SnapshotStart {
            if e.sequence == 0 || self.sequence.is_some_and(|last| e.sequence <= last) {
                return Err(BookError::SequenceGap);
            }
            Self::validate_marker(e)?;
            self.staging = Ladders::new();
            self.state = BookState::BuildingSnapshot;
            return Ok(());
        }
        if !matches!(self.state, BookState::Live | BookState::BuildingSnapshot) {
            return Err(BookError::SnapshotRequired);
        }
        if self.sequence.and_then(|s| s.checked_add(1)) != Some(e.sequence) {
            return Err(BookError::SequenceGap);
        }
        if e.event_type == Kind::SnapshotEnd {
            Self::validate_marker(e)?;
            if self.state != BookState::BuildingSnapshot {
                return Err(BookError::InvalidSnapshot);
            }
            if self.staging.crossed() {
                return Err(BookError::Crossed);
            }
            core::mem::swap(&mut self.active, &mut self.staging);
            self.state = BookState::Live;
            return Ok(());
        }
        if e.price_ticks.0 <= 0 {
            return Err(BookError::InvalidPrice);
        }
        if (e.event_type == Kind::Cancel && e.qty_units.0 != 0)
            || (e.event_type != Kind::Cancel && e.qty_units.0 <= 0)
        {
            return Err(BookError::InvalidQuantity);
        }
        if self.state == BookState::BuildingSnapshot && e.event_type != Kind::Add {
            return Err(BookError::InvalidSnapshot);
        }
        // Prints do not decrement an L2 book: native depth updates carry that change.
        if e.event_type == Kind::Trade {
            return Ok(());
        }
        let building = self.state == BookState::BuildingSnapshot;
        let target = if building {
            &mut self.staging
        } else {
            &mut self.active
        };
        // Reject crossing before modifying visible data; snapshots validate at commit.
        if !building && e.event_type != Kind::Cancel {
            let crosses = match e.side {
                Side::Buy => target
                    .asks
                    .view()
                    .first()
                    .is_some_and(|a| e.price_ticks >= a.price),
                Side::Sell => target
                    .bids
                    .view()
                    .first()
                    .is_some_and(|b| e.price_ticks <= b.price),
            };
            if crosses {
                return Err(BookError::Crossed);
            }
        }
        match e.side {
            Side::Buy => target.bids.apply(e),
            Side::Sell => target.asks.apply(e),
        }
    }
    fn validate_marker(e: &MarketEvent) -> Result<(), BookError> {
        if e.qty_units.0 != 0 || e.price_ticks.0 != 0 {
            Err(BookError::InvalidSnapshot)
        } else {
            Ok(())
        }
    }
}
