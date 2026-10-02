use book::{BookError, BookState, VenueBook};
use common::*;
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
use recorder::*;
use replay::*;
fn metadata() -> RecordingMetadata {
    RecordingMetadata {
        venue: VenueId(1),
        instrument: InstrumentId(1),
        price_decimals: 2,
        quantity_decimals: 6,
        tick_atoms: 1,
        quantity_atoms: 1,
    }
}
fn e(seq: u64, kind: K, side: Side, price: i64, qty: i64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(1),
        instrument: InstrumentId(1),
        sequence: seq,
        exchange_sequence: seq * 10,
        exchange_ts: seq * 1000,
        receive_ts: Timestamp(seq),
        event_type: kind,
        side,
        price_ticks: PriceTicks(price),
        qty_units: QtyUnits(qty),
    }
}
fn initial() -> Vec<MarketEvent> {
    vec![
        e(1, K::SnapshotStart, Side::Buy, 0, 0),
        e(2, K::Add, Side::Buy, 100, 10),
        e(3, K::Add, Side::Sell, 102, 20),
        e(4, K::SnapshotEnd, Side::Buy, 0, 0),
    ]
}
fn encode(events: &[MarketEvent]) -> Vec<u8> {
    let mut r = Recorder::new(Vec::new(), metadata()).unwrap();
    for event in events {
        r.append(event).unwrap();
    }
    r.finish().unwrap()
}
fn book() -> VenueBook<16> {
    VenueBook::new(VenueId(1), InstrumentId(1))
}
#[test]
fn direct_replay_twice_and_all_pacing_modes_match_after_every_event() {
    let mut events = initial();
    events.extend((5..1005).map(|seq| e(seq, K::Modify, Side::Buy, 100, (seq % 11 + 1) as i64)));
    let bytes = encode(&events);
    let mut direct = book();
    let mut a = book();
    let mut b = book();
    let mut ra = Replay::new(bytes.as_slice(), ReplayMode::Maximum).unwrap();
    let mut rb = Replay::new(bytes.as_slice(), ReplayMode::Maximum).unwrap();
    for event in &events {
        direct.apply(event).unwrap();
        assert_eq!(ra.step(&mut a).unwrap(), Some(*event));
        assert_eq!(rb.step(&mut b).unwrap(), Some(*event));
        assert_eq!(direct, a);
        assert_eq!(a, b);
    }
    assert_eq!(ra.step(&mut a).unwrap(), None);
    for speed in [1, 2, 10, 100] {
        let mut paced = book();
        let mut replay = Replay::new(bytes.as_slice(), ReplayMode::Paced { speed }).unwrap();
        assert_eq!(replay.run(&mut paced).unwrap(), events.len() as u64);
        assert_eq!(paced, direct);
    }
}
#[test]
fn gaps_and_clock_regression_invalidate_shared_book() {
    for (sequence, timestamp, error) in [
        (6, 6, BookError::SequenceGap),
        (5, 3, BookError::ClockRegression),
    ] {
        let mut events = initial();
        let mut bad = e(sequence, K::Modify, Side::Buy, 100, 5);
        bad.receive_ts = Timestamp(timestamp);
        events.push(bad);
        let bytes = encode(&events);
        let mut b = book();
        let mut r = Replay::new(bytes.as_slice(), ReplayMode::Maximum).unwrap();
        assert!(matches!(r.run(&mut b),Err(ReplayError::Book(found)) if found == error));
        assert_eq!(b.state(), BookState::Invalid(error));
        assert!(b.best(Side::Buy).is_err());
    }
}
#[test]
fn incomplete_snapshot_and_zero_speed_rejected() {
    let bytes = encode(&initial()[..3]);
    let mut b = book();
    assert!(matches!(
        Replay::new(bytes.as_slice(), ReplayMode::Paced { speed: 0 }),
        Err(ReplayError::InvalidSpeed)
    ));
    let mut r = Replay::new(bytes.as_slice(), ReplayMode::Maximum).unwrap();
    assert!(matches!(
        r.run(&mut b),
        Err(ReplayError::IncompleteBookState)
    ));
}
