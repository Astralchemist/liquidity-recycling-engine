use book::VenueBook;
use common::*;
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
pub fn event(seq: u64, kind: K, side: Side, price: i64, qty: i64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(1),
        instrument: InstrumentId(1),
        sequence: seq,
        exchange_sequence: seq,
        exchange_ts: seq,
        receive_ts: Timestamp(seq),
        event_type: kind,
        side,
        price_ticks: PriceTicks(price),
        qty_units: QtyUnits(qty),
    }
}
pub fn setup() -> (VenueBook<256>, MarketEvent) {
    let mut book = VenueBook::new(VenueId(1), InstrumentId(1));
    book.apply(&event(1, K::SnapshotStart, Side::Buy, 0, 0))
        .unwrap();
    let mut seq = 2;
    for index in 0..128 {
        book.apply(&event(seq, K::Add, Side::Buy, 100_000 - index, 100))
            .unwrap();
        seq += 1;
        book.apply(&event(seq, K::Add, Side::Sell, 100_002 + index, 100))
            .unwrap();
        seq += 1;
    }
    book.apply(&event(seq, K::SnapshotEnd, Side::Buy, 0, 0))
        .unwrap();
    (book, event(seq, K::Modify, Side::Buy, 100_000, 100))
}
pub fn advance(e: &mut MarketEvent) {
    e.sequence += 1;
    e.receive_ts.0 += 1;
    e.qty_units.0 = 100 + (e.sequence % 7) as i64;
    e.price_ticks.0 = 100_000 - (e.sequence % 128) as i64;
}
