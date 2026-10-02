use common::*;
use consolidator::{normalization::*, *};
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
use recorder::*;
use replay::merged::*;
use std::io::Cursor;
fn grid() -> InstrumentMetadata {
    InstrumentMetadata {
        instrument: InstrumentId(1),
        market: MarketIdentity {
            base_asset: 1,
            quote_asset: 2,
            settlement_asset: 2,
            kind: ContractKind::Spot,
            equivalence_group: 1,
        },
        price: UnitScale {
            atoms: 1,
            decimals: 2,
        },
        quantity: UnitScale {
            atoms: 1,
            decimals: 6,
        },
    }
}
fn engine() -> Consolidator<3, 8, 24> {
    Consolidator::new(
        grid(),
        std::array::from_fn(|i| VenueConfig {
            venue: VenueId(i as u16 + 1),
            metadata: grid(),
            weight_ppm: 1_000_000,
            stale_after_ns: 100,
        }),
    )
    .unwrap()
}
fn events(venue: u16) -> Vec<MarketEvent> {
    (1..=24)
        .map(|s| {
            let (kind, side, p, q) = match s {
                1 => (K::SnapshotStart, Side::Buy, 0, 0),
                2 => (K::Add, Side::Buy, 99, 100),
                3 => (K::Add, Side::Sell, 101, 100),
                4 => (K::SnapshotEnd, Side::Buy, 0, 0),
                _ => (K::Modify, Side::Buy, 99, s as i64),
            };
            MarketEvent {
                venue: VenueId(venue),
                instrument: InstrumentId(1),
                sequence: s,
                exchange_sequence: s,
                exchange_ts: 1000 - s,
                receive_ts: Timestamp(s),
                event_type: kind,
                side,
                price_ticks: PriceTicks(p),
                qty_units: QtyUnits(q),
            }
        })
        .collect()
}
fn bytes(venue: u16) -> Vec<u8> {
    let mut r = Recorder::new(
        Vec::new(),
        RecordingMetadata {
            venue: VenueId(venue),
            instrument: InstrumentId(1),
            price_decimals: 2,
            quantity_decimals: 6,
            tick_atoms: 1,
            quantity_atoms: 1,
        },
    )
    .unwrap();
    for e in events(venue) {
        r.append(&e).unwrap();
    }
    r.finish().unwrap()
}
#[test]
fn merge_order_is_independent_of_source_order_and_exchange_clocks() {
    let mut expected: Vec<_> = (1..=3).flat_map(events).collect();
    expected.sort_by_key(|e| (e.receive_ts.0, e.venue.0, e.sequence));
    let mut direct = engine();
    for e in &expected {
        direct.apply(e).unwrap();
    }
    for order in [[1, 2, 3], [3, 1, 2], [2, 3, 1]] {
        let mut merged = MergedReplay::new(order.map(|v| Cursor::new(bytes(v))), true).unwrap();
        let mut replayed = engine();
        assert_eq!(merged.run(&mut replayed).unwrap(), 72);
        assert_eq!(direct, replayed);
        let mut merged = MergedReplay::new(order.map(|v| Cursor::new(bytes(v))), true).unwrap();
        for e in &expected {
            assert_eq!(merged.next_event().unwrap(), Some(*e));
        }
        assert!(merged.next_event().unwrap().is_none());
    }
}
#[test]
fn clock_attestation_metadata_and_duplicate_sources_are_required() {
    assert!(matches!(
        MergedReplay::new([Cursor::new(bytes(1))], false),
        Err(MergeError::ClockDomainRequired)
    ));
    assert!(matches!(
        MergedReplay::new([Cursor::new(bytes(1)), Cursor::new(bytes(1))], true),
        Err(MergeError::DuplicateVenue)
    ));
    let mut config = engine().venue_config(VenueId(1)).unwrap();
    config.metadata.price.atoms = 5;
    let mismatch = Consolidator::<3, 8, 24>::new(
        grid(),
        [
            config,
            engine().venue_config(VenueId(2)).unwrap(),
            engine().venue_config(VenueId(3)).unwrap(),
        ],
    )
    .unwrap();
    let merged = MergedReplay::new([1, 2, 3].map(|v| Cursor::new(bytes(v))), true).unwrap();
    assert!(matches!(
        merged.validate(&mismatch),
        Err(MergeError::MetadataMismatch)
    ));
}
#[test]
fn unfinished_snapshot_cannot_finish_successfully() {
    let mut one = bytes(1);
    one.truncate(HEADER_SIZE + RECORD_SIZE * 3);
    let mut r = MergedReplay::new(
        [
            Cursor::new(one),
            Cursor::new(bytes(2)),
            Cursor::new(bytes(3)),
        ],
        true,
    )
    .unwrap();
    assert!(matches!(
        r.run(&mut engine()),
        Err(MergeError::IncompleteSnapshot)
    ));
}
