//! Stable identifiers and local monotonic nanoseconds. Exchange clocks are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VenueId(pub u16);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstrumentId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Reserved capacities are deliberate limits, never requests to grow at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityError {
    Full,
}
