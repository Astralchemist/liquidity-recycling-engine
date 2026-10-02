//! Reserved okx adapter boundary. Network connectivity is NOT implemented.
//! Native sequence validation and atomic depth-batch normalization precede decoding.
pub use market_events::FeedDecoder;
pub const VENUE_NAME: &str = "okx";
