use common::*;
use consolidator::{normalization::*, *};
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
use std::collections::BTreeMap;
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
fn configs() -> [VenueConfig; 3] {
    std::array::from_fn(|i| VenueConfig {
        venue: VenueId(i as u16 + 1),
        metadata: InstrumentMetadata {
            instrument: InstrumentId(i as u32 + 1),
            price: UnitScale {
                atoms: [1, 5, 10][i],
                decimals: 2,
            },
            quantity: UnitScale {
                atoms: [1, 10, 100][i],
                decimals: 6,
            },
            ..grid()
        },
        weight_ppm: [1_000_000, 750_000, 500_000][i],
        stale_after_ns: 1_000_000,
    })
}
fn event(v: usize, seq: u64, ts: u64, k: K, side: Side, p: i64, q: i64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(v as u16 + 1),
        instrument: InstrumentId(v as u32 + 1),
        sequence: seq,
        exchange_sequence: seq,
        exchange_ts: ts,
        receive_ts: Timestamp(ts),
        event_type: k,
        side,
        price_ticks: PriceTicks(p),
        qty_units: QtyUnits(q),
    }
}
fn initialized(cs: [VenueConfig; 3]) -> Consolidator<3, 32, 96> {
    let mut e = Consolidator::new(grid(), cs).unwrap();
    for i in 0..3 {
        let scale = [1, 5, 10][i];
        let time = i as u64 * 4;
        for x in [
            event(i, 1, time + 1, K::SnapshotStart, Side::Buy, 0, 0),
            event(i, 2, time + 2, K::Add, Side::Buy, 9900 / scale, 100),
            event(i, 3, time + 3, K::Add, Side::Sell, 10100 / scale, 100),
            event(i, 4, time + 4, K::SnapshotEnd, Side::Buy, 0, 0),
        ] {
            e.apply(&x).unwrap();
        }
    }
    e
}
#[test]
fn exact_normalization_and_settlement_conserve_value() {
    for config in configs() {
        let n = Normalizer::new(config.metadata, grid()).unwrap();
        for price in 1..=100 {
            for qty in 1..=100 {
                let common = n.notional(PriceTicks(price), QtyUnits(qty)).unwrap();
                assert_eq!(
                    common,
                    Money(
                        i128::from(price)
                            * i128::from(qty)
                            * i128::from(config.metadata.price.atoms)
                            * i128::from(config.metadata.quantity.atoms)
                    )
                );
                assert_eq!(settlement_money(common, grid(), 8).unwrap(), common);
            }
        }
    }
    let mut bad = grid();
    bad.market.kind = ContractKind::Linear;
    assert_eq!(
        Normalizer::new(bad, grid()),
        Err(NormalizationError::IncompatibleMarket)
    );
    bad = grid();
    bad.market.equivalence_group = 9;
    assert!(Normalizer::new(bad, grid()).is_err());
    bad = grid();
    bad.price.atoms = 0;
    assert!(Normalizer::new(bad, grid()).is_err());
    let coarse = InstrumentMetadata {
        price: UnitScale {
            atoms: 5,
            decimals: 2,
        },
        ..grid()
    };
    let n = Normalizer::new(grid(), coarse).unwrap();
    assert!(n.price(PriceTicks(1)).is_err());
    assert_eq!(n.price(PriceTicks(10)).unwrap(), PriceTicks(2));
    let huge = InstrumentMetadata {
        price: UnitScale {
            atoms: i64::MAX,
            decimals: 0,
        },
        ..grid()
    };
    assert!(
        Normalizer::new(huge, grid())
            .unwrap()
            .price(PriceTicks(i64::MAX))
            .is_err()
    );
    assert!(settlement_money(Money(1), grid(), 6).is_err());
    assert_eq!(settlement_money(Money(-100), grid(), 6).unwrap(), Money(-1));
}
#[test]
fn weighted_depth_and_divergence_are_explicit() {
    let mut e = initialized(configs());
    assert_eq!(e.depth(Side::Buy).unwrap(), 5_850_000_000);
    assert_eq!(e.midpoint_x2().unwrap(), Some(20000));
    assert_eq!(
        e.divergence(VenueId(2)).unwrap().unwrap().basis_points,
        BasisPoints(0)
    );
    // A local uncrossed book can cross a different venue's quote.
    e.apply(&event(0, 5, 13, K::Add, Side::Buy, 10090, 10))
        .unwrap();
    e.apply(&event(1, 5, 14, K::Add, Side::Sell, 2000, 10))
        .unwrap();
    assert_eq!(e.market_state().unwrap(), MarketState::Crossed);
    assert_eq!(e.spread().unwrap(), Some(-90));
    assert!(
        e.divergence(VenueId(1))
            .unwrap()
            .unwrap()
            .midpoint_difference_x2
            > 0
    );
}
#[test]
fn staleness_disconnect_and_gap_withdraw_one_venue() {
    let mut cs = configs();
    for c in &mut cs {
        c.stale_after_ns = 10;
    }
    let mut e = initialized(cs);
    e.advance_time(Timestamp(14)).unwrap();
    assert!(e.included(VenueId(1)).unwrap());
    e.advance_time(Timestamp(15)).unwrap();
    assert!(!e.included(VenueId(1)).unwrap());
    assert_eq!(e.depth(Side::Buy).unwrap(), 5_750_000_000);
    assert!(
        e.apply(&event(0, 5, 15, K::Trade, Side::Buy, 9900, 10))
            .is_err()
    );
    e.disconnect(VenueId(3), Timestamp(16)).unwrap();
    assert_eq!(e.depth(Side::Buy).unwrap(), 750_000_000);
    assert!(matches!(
        e.apply(&event(1, 7, 17, K::Modify, Side::Buy, 1980, 10)),
        Err(ConsolidationError::Book { .. })
    ));
    assert_eq!(e.market_state().unwrap(), MarketState::Empty);
    assert_eq!(e.fault(), None);
    for x in [
        event(0, 10, 18, K::SnapshotStart, Side::Buy, 0, 0),
        event(0, 11, 19, K::Add, Side::Buy, 9900, 2),
        event(0, 12, 20, K::SnapshotEnd, Side::Buy, 0, 0),
    ] {
        e.apply(&x).unwrap();
    }
    assert_eq!(e.depth(Side::Buy).unwrap(), 2_000_000);
}
#[test]
fn clock_fault_and_bad_scales_hide_all_aggregate_queries() {
    let mut e = initialized(configs());
    assert_eq!(
        e.advance_time(Timestamp(11)),
        Err(ConsolidationError::ClockRegression)
    );
    assert_eq!(e.levels(Side::Buy), Err(ConsolidationError::Faulted));
    let mut cs = configs();
    cs[0].metadata.quantity.atoms = i64::MAX;
    let mut e = Consolidator::<3, 32, 96>::new(grid(), cs).unwrap();
    e.apply(&event(0, 1, 1, K::SnapshotStart, Side::Buy, 0, 0))
        .unwrap();
    assert!(
        e.apply(&event(0, 2, 2, K::Add, Side::Buy, 9900, 2))
            .is_err()
    );
    assert!(e.levels(Side::Buy).is_err());
    let mut cs = configs();
    cs[1].venue = cs[0].venue;
    assert!(matches!(
        Consolidator::<3, 32, 96>::new(grid(), cs),
        Err(ConsolidationError::DuplicateVenue)
    ));
    assert!(Consolidator::<3, 32, 95>::new(grid(), configs()).is_err());
}
#[test]
fn atomic_message_replaces_only_one_venue_contribution() {
    let mut e = initialized(configs());
    let mut batch = [
        event(0, 5, 13, K::Add, Side::Buy, 10110, 1),
        event(0, 6, 13, K::Cancel, Side::Sell, 10100, 0),
        event(0, 7, 13, K::Add, Side::Sell, 10120, 3),
    ];
    for x in &mut batch {
        x.exchange_sequence = 5;
    }
    e.apply_depth_batch(VenueId(1), &batch).unwrap();
    assert_eq!(e.depth(Side::Buy).unwrap(), 5_851_000_000);
    assert_eq!(e.depth(Side::Sell).unwrap(), 5_753_000_000);
    let mut bad = event(0, 8, 14, K::Modify, Side::Buy, 10110, 0);
    bad.exchange_sequence = 6;
    assert!(e.apply_depth_batch(VenueId(1), &[bad]).is_err());
    assert_eq!(e.depth(Side::Buy).unwrap(), 5_750_000_000);
}
#[test]
fn generated_incremental_updates_match_independent_reference() {
    for seed in 1_u64..=8 {
        let mut engine = initialized(configs());
        let mut twin = engine.clone();
        let mut rng = seed;
        let mut seq = [4; 3];
        let mut models: [BTreeMap<(u8, i64), i64>; 3] =
            std::array::from_fn(|_| BTreeMap::from([((0, 9900), 100), ((1, 10100), 100)]));
        for time in 13..5013 {
            rng = rng
                .wrapping_mul(6364136223846793005_u64)
                .wrapping_add(1442695040888963407);
            let i = (rng >> 32) as usize % 3;
            let sell = (rng >> 36) & 1 != 0;
            let side = if sell { Side::Sell } else { Side::Buy };
            let tag = u8::from(sell);
            let price = if sell {
                10100 + ((rng >> 40) % 20) as i64 * 10
            } else {
                9900 - ((rng >> 40) % 20) as i64 * 10
            };
            let key = (tag, price);
            let exists = models[i].contains_key(&key);
            let cancel = exists && rng % 3 == 0;
            let kind = if cancel {
                K::Cancel
            } else if exists {
                K::Modify
            } else {
                K::Add
            };
            let qty = if cancel { 0 } else { (rng % 1000 + 1) as i64 };
            if cancel {
                models[i].remove(&key);
            } else {
                models[i].insert(key, qty);
            }
            seq[i] += 1;
            let ev = event(i, seq[i], time, kind, side, price / [1, 5, 10][i], qty);
            engine.apply(&ev).unwrap();
            twin.apply(&ev).unwrap();
            assert_eq!(engine, twin);
            for (s, tag) in [(Side::Buy, 0), (Side::Sell, 1)] {
                let mut expected = BTreeMap::<i64, i128>::new();
                for (v, model) in models.iter().enumerate() {
                    for (&(t, p), &q) in model {
                        if t == tag {
                            *expected.entry(p).or_default() +=
                                i128::from(q) * [1, 10, 100][v] * [1_000_000, 750_000, 500_000][v];
                        }
                    }
                }
                let mut expected: Vec<_> = expected
                    .into_iter()
                    .map(|(p, q)| ConsolidatedLevel {
                        price: PriceTicks(p),
                        weighted_quantity_microunits: q,
                    })
                    .collect();
                if s == Side::Buy {
                    expected.reverse();
                }
                assert_eq!(
                    engine.depth(s).unwrap(),
                    expected
                        .iter()
                        .map(|l| l.weighted_quantity_microunits)
                        .sum::<i128>()
                );
                assert_eq!(engine.levels(s).unwrap(), expected);
            }
        }
    }
}

#[test]
fn replacement_snapshots_withdraw_old_depth_and_zero_weight_has_no_global_quote() {
    let mut cs = configs();
    cs[2].weight_ppm = 0;
    let mut e = initialized(cs);
    assert_eq!(e.depth(Side::Buy).unwrap(), 850_000_000);
    e.apply(&event(2, 5, 13, K::Add, Side::Buy, 1009, 1))
        .unwrap();
    assert_eq!(e.midpoint_x2().unwrap(), Some(20000));
    let before = e.clone();
    let mut wrong = event(0, 5, 14, K::Modify, Side::Buy, 9900, 10);
    wrong.instrument = InstrumentId(99);
    assert_eq!(e.apply(&wrong), Err(ConsolidationError::WrongMarket));
    assert_eq!(e, before);
    e.apply(&event(0, 5, 14, K::SnapshotStart, Side::Buy, 0, 0))
        .unwrap();
    assert_eq!(e.depth(Side::Buy).unwrap(), 750_000_000);
    e.apply(&event(0, 6, 15, K::Add, Side::Buy, 9800, 2))
        .unwrap();
    assert_eq!(e.depth(Side::Buy).unwrap(), 750_000_000);
    e.apply(&event(0, 7, 16, K::SnapshotEnd, Side::Buy, 0, 0))
        .unwrap();
    assert_eq!(e.depth(Side::Buy).unwrap(), 752_000_000);
}

#[test]
fn disagreement_uses_normalized_unweighted_depth() {
    let e = initialized(configs());
    let d = e
        .depth_disagreement(Side::Buy, PriceTicks(9900))
        .unwrap()
        .unwrap();
    // Quantities 100, 1000, 10000. n*sum(q²)-sum(q)² = 179820000.
    assert_eq!(d.venues, 3);
    assert_eq!(d.variance_numerator, 179_820_000);
    assert_eq!(d.variance_denominator, 9);
    assert_eq!(d.sigma_units_floor, 4469);
    assert_eq!(
        e.depth_disagreement(Side::Buy, PriceTicks(1))
            .unwrap()
            .unwrap()
            .sigma_units_floor,
        0
    );
}
