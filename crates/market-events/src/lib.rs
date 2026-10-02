//! Canonical normalized events, independent of venue wire encodings.
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MarketEventType {
    Add = 0,
    Modify = 1,
    Cancel = 2,
    Trade = 3,
    SnapshotStart = 4,
    SnapshotEnd = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketEvent {
    pub venue: VenueId,
    pub instrument: InstrumentId,
    /// Contiguous canonical sequence PER venue/instrument, including trades and markers.
    /// Adapters MUST validate native update ranges before emitting these events.
    pub sequence: u64,
    pub exchange_sequence: u64,
    /// Exchange epoch nanoseconds; never compared to local monotonic time.
    pub exchange_ts: u64,
    pub receive_ts: Timestamp,
    pub event_type: MarketEventType,
    /// Trade side is aggressor side. Depth side is resting side.
    pub side: Side,
    pub price_ticks: PriceTicks,
    /// Add/Modify contain absolute quantity. Cancel contains zero quantity.
    pub qty_units: QtyUnits,
}

/// Decision-path timestamps are distinct from immutable feed input and recordings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleTimes {
    pub processed: Option<Timestamp>,
    pub decision: Option<Timestamp>,
    pub order_send: Option<Timestamp>,
    pub ack: Option<Timestamp>,
}

/// A decoder writes into caller-owned storage. No exchange-specific types enter core.
pub trait FeedDecoder {
    type Error;
    fn decode(
        &mut self,
        frame: &[u8],
        receive: Timestamp,
        output: &mut [MarketEvent],
    ) -> Result<usize, Self::Error>;
}

/// Generic adapter contract, not a claim about a particular exchange protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeUpdateRange {
    pub first: u64,
    pub last: u64,
    pub previous_last: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceError {
    SnapshotRequired,
    Gap,
    Duplicate,
    InvalidRange,
    PreviousMismatch,
    Exhausted,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeSequenceTracker {
    last: Option<u64>,
    failed: bool,
}
impl NativeSequenceTracker {
    pub fn reset_from_snapshot(&mut self, last: u64) {
        self.last = Some(last);
        self.failed = false;
    }
    pub fn last(&self) -> Option<u64> {
        self.last
    }
    /// Accept overlapping ranges containing the NEXT expected native update.
    /// A previous pointer, when supplied, must match the prior accepted range end.
    pub fn accept(&mut self, range: NativeUpdateRange) -> Result<(), SequenceError> {
        let result = self.validate(range);
        if result.is_err() {
            self.failed = true;
        } else {
            self.last = Some(range.last);
        }
        result
    }
    fn validate(&self, range: NativeUpdateRange) -> Result<(), SequenceError> {
        if self.failed {
            return Err(SequenceError::SnapshotRequired);
        }
        let last = self.last.ok_or(SequenceError::SnapshotRequired)?;
        if range.first > range.last {
            return Err(SequenceError::InvalidRange);
        }
        let next = last.checked_add(1).ok_or(SequenceError::Exhausted)?;
        if range.last <= last {
            return Err(SequenceError::Duplicate);
        }
        if range.first > next {
            return Err(SequenceError::Gap);
        }
        if range.previous_last.is_some_and(|p| p != last) {
            return Err(SequenceError::PreviousMismatch);
        }
        Ok(())
    }
}
