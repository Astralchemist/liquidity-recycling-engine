use book::*;
use common::*;
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
use std::collections::BTreeMap;
fn e(seq: u64, kind: K, side: Side, price: i64, qty: i64) -> MarketEvent {
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
fn ready<const N: usize>() -> VenueBook<N> {
    let mut book = VenueBook::new(VenueId(1), InstrumentId(1));
    for event in [
        e(1, K::SnapshotStart, Side::Buy, 0, 0),
        e(2, K::Add, Side::Buy, 99, 5),
        e(3, K::Add, Side::Sell, 102, 7),
        e(4, K::SnapshotEnd, Side::Buy, 0, 0),
    ] {
        book.apply(&event).unwrap();
    }
    book
}
#[test]
fn snapshot_visibility_and_queries() {
    let mut b = ready::<4>();
    assert_eq!(b.midpoint_x2(), Ok(Some(201)));
    assert_eq!(b.spread(), Ok(Some(3)));
    assert_eq!(b.depth(Side::Buy, 10), Ok(5));
    b.apply(&e(5, K::SnapshotStart, Side::Buy, 0, 0)).unwrap();
    assert!(b.best(Side::Buy).is_err());
    b.apply(&e(6, K::Add, Side::Buy, 98, 8)).unwrap();
    b.apply(&e(7, K::SnapshotEnd, Side::Buy, 0, 0)).unwrap();
    assert_eq!(b.best(Side::Sell), Ok(None));
    assert_eq!(b.midpoint_x2(), Ok(None));
    assert_eq!(b.best(Side::Buy).unwrap().unwrap().price, PriceTicks(98));
}
#[test]
fn fail_closed_and_recover() {
    let mut b = ready::<4>();
    assert_eq!(
        b.apply(&e(6, K::Modify, Side::Buy, 99, 3)),
        Err(BookError::SequenceGap)
    );
    assert!(b.best(Side::Buy).is_err());
    assert_eq!(
        b.apply(&e(5, K::Modify, Side::Buy, 99, 3)),
        Err(BookError::SnapshotRequired)
    );
    b.apply(&e(10, K::SnapshotStart, Side::Buy, 0, 0)).unwrap();
    b.apply(&e(11, K::SnapshotEnd, Side::Buy, 0, 0)).unwrap();
    assert_eq!(b.state(), BookState::Live);
    assert_eq!(b.best(Side::Buy), Ok(None));
}
#[test]
fn invalid_updates_are_rejected() {
    for (kind, side, price, qty, error) in [
        (K::Add, Side::Buy, 99, 1, BookError::DuplicateLevel),
        (K::Modify, Side::Buy, 98, 1, BookError::MissingLevel),
        (K::Cancel, Side::Buy, 98, 0, BookError::MissingLevel),
        (K::Modify, Side::Buy, 99, 0, BookError::InvalidQuantity),
        (K::Cancel, Side::Buy, 99, 1, BookError::InvalidQuantity),
        (K::Add, Side::Buy, 0, 1, BookError::InvalidPrice),
        (K::Add, Side::Buy, 102, 1, BookError::Crossed),
        (K::Add, Side::Sell, 99, 1, BookError::Crossed),
        (K::SnapshotEnd, Side::Buy, 0, 0, BookError::InvalidSnapshot),
    ] {
        let mut b = ready::<4>();
        assert_eq!(b.apply(&e(5, kind, side, price, qty)), Err(error));
        assert_eq!(b.state(), BookState::Invalid(error));
        assert!(b.levels(Side::Buy).is_err());
    }
}
#[test]
fn capacity_clock_routing_and_trade_semantics() {
    let mut b = ready::<1>();
    assert_eq!(
        b.apply(&e(5, K::Add, Side::Buy, 98, 1)),
        Err(BookError::Capacity)
    );
    let mut b = ready::<2>();
    let before = b.clone();
    let mut wrong = e(5, K::Cancel, Side::Buy, 99, 0);
    wrong.venue = VenueId(9);
    assert_eq!(b.apply(&wrong), Err(BookError::WrongMarket));
    assert_eq!(b, before);
    b.apply(&e(5, K::Trade, Side::Sell, 99, 5)).unwrap();
    assert_eq!(b.depth(Side::Buy, 1), Ok(5));
    let mut regressed = e(6, K::Modify, Side::Buy, 99, 1);
    regressed.receive_ts = Timestamp(4);
    assert_eq!(b.apply(&regressed), Err(BookError::ClockRegression));
}
#[test]
fn crossed_snapshot_and_sequence_overflow() {
    let mut b = VenueBook::<2>::new(VenueId(1), InstrumentId(1));
    b.apply(&e(1, K::SnapshotStart, Side::Buy, 0, 0)).unwrap();
    b.apply(&e(2, K::Add, Side::Buy, 102, 1)).unwrap();
    b.apply(&e(3, K::Add, Side::Sell, 101, 1)).unwrap();
    assert_eq!(
        b.apply(&e(4, K::SnapshotEnd, Side::Buy, 0, 0)),
        Err(BookError::Crossed)
    );
    b.apply(&e(u64::MAX, K::SnapshotStart, Side::Buy, 0, 0))
        .unwrap();
    let mut end = e(0, K::SnapshotEnd, Side::Buy, 0, 0);
    end.receive_ts = Timestamp(u64::MAX);
    assert_eq!(b.apply(&end), Err(BookError::SequenceGap));
}
#[test]
fn generated_stream_matches_reference_and_is_deterministic() {
    // Fixed seeds, 200,000 mutations; failures reproduce without a randomness service.
    for seed in 1..=20_u64 {
        let mut rng = seed;
        let mut a = ready::<64>();
        let mut b = a.clone();
        let mut bids = BTreeMap::from([(99, 5)]);
        let mut asks = BTreeMap::from([(102, 7)]);
        for seq in 5..10_005 {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let side = if rng & (1 << 32) == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let offset = ((rng >> 33) % 50) as i64;
            let price = match side {
                Side::Buy => 99 - offset,
                Side::Sell => 102 + offset,
            };
            let model = match side {
                Side::Buy => &mut bids,
                Side::Sell => &mut asks,
            };
            let exists = model.contains_key(&price);
            let cancel = exists && rng % 3 == 0;
            let qty = if cancel {
                0
            } else {
                ((rng >> 17) % 10_000 + 1) as i64
            };
            let kind = if cancel {
                K::Cancel
            } else if exists {
                K::Modify
            } else {
                K::Add
            };
            if cancel {
                model.remove(&price);
            } else {
                model.insert(price, qty);
            }
            let event = e(seq, kind, side, price, qty);
            a.apply(&event).unwrap();
            b.apply(&event).unwrap();
            assert_eq!(a, b);
            let expected_bids: Vec<_> = bids
                .iter()
                .rev()
                .map(|(&p, &q)| Level {
                    price: PriceTicks(p),
                    qty: QtyUnits(q),
                })
                .collect();
            let expected_asks: Vec<_> = asks
                .iter()
                .map(|(&p, &q)| Level {
                    price: PriceTicks(p),
                    qty: QtyUnits(q),
                })
                .collect();
            assert_eq!(a.levels(Side::Buy).unwrap(), expected_bids);
            assert_eq!(a.levels(Side::Sell).unwrap(), expected_asks);
            assert!(
                a.levels(Side::Buy)
                    .unwrap()
                    .windows(2)
                    .all(|w| w[0].price > w[1].price)
            );
            assert!(
                a.levels(Side::Sell)
                    .unwrap()
                    .windows(2)
                    .all(|w| w[0].price < w[1].price)
            );
        }
    }
}

#[test]
fn atomic_depth_batches_allow_intermediate_crossing_and_fail_closed() {
    let mut b = ready::<4>();
    // Raise best bid above old ask, then move ask: final book 103/104 is valid.
    let mut events = [
        e(5, K::Add, Side::Buy, 103, 2),
        e(6, K::Cancel, Side::Sell, 102, 0),
        e(7, K::Add, Side::Sell, 104, 3),
    ];
    for event in &mut events {
        event.receive_ts = Timestamp(5);
        event.exchange_sequence = 50;
    }
    b.apply_depth_batch(&events).unwrap();
    assert_eq!(b.spread(), Ok(Some(1)));
    assert_eq!(b.sequence(), Some(7));
    let committed = b.last_committed_levels(Side::Buy).to_vec();
    let mut bad = [
        e(8, K::Modify, Side::Buy, 103, 99),
        e(9, K::Add, Side::Buy, 104, 1),
    ];
    for event in &mut bad {
        event.receive_ts = Timestamp(8);
        event.exchange_sequence = 60;
    }
    assert_eq!(b.apply_depth_batch(&bad), Err(BookError::Crossed));
    assert_eq!(b.last_committed_levels(Side::Buy), committed);
    assert_eq!(b.sequence(), Some(7));
    assert!(b.best(Side::Buy).is_err());
}
