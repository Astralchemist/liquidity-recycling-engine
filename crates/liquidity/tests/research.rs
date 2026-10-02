use common::*;
use consolidator::{VenueConfig, normalization::*};
use fixed_point::*;
use liquidity::{research::*, *};
use recorder::{Recorder, RecordingMetadata};
use replay::merged::MergedReplay;
use simulation::structures::{StructureScenario, generate};
use voids::*;
fn metadata() -> InstrumentMetadata {
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
            decimals: 0,
        },
        quantity: UnitScale {
            atoms: 1,
            decimals: 0,
        },
    }
}
fn configs() -> [VenueConfig; 3] {
    std::array::from_fn(|v| VenueConfig {
        venue: VenueId(v as u16 + 1),
        metadata: metadata(),
        weight_ppm: [1_000_000, 750_000, 500_000][v],
        stale_after_ns: 1_000_000_000,
    })
}
fn lc() -> LiquidityConfig<3> {
    LiquidityConfig {
        lower_price: PriceTicks(80),
        cell_width: 4,
        bucket_edges_ppm: [10_000, 100_000, 500_000],
        sample_interval_ns: 1_000_000,
        warmup_samples: 3,
        baseline_alpha_ppm: 10_000,
        minimum_baseline_units: 10,
        minimum_covered_venues: 3,
        assume_contiguous_l2_coverage: true,
        pool_depth_ratio_ppm: 2_000_000,
        pool_persistence_ns: 3_000_000,
        pool_minimum_score_ppm: 1_100_000,
        pool_weights: [600_000, 200_000, 200_000],
        price_reference: PriceReference::LastTrade,
    }
}
fn vc() -> VoidConfig {
    VoidConfig {
        minimum_width_ticks: 3,
        minimum_persistence_ns: 3_000_000,
        maximum_age_ns: 1_000_000_000,
        low_depth_ppm: 250_000,
        refill_depth_ppm: 800_000,
        minimum_score_ppm: 750_000,
        coverage_loss: voids::CoverageLoss::Invalidate,
    }
}
type Engine = ResearchEngine<3, 16, 48, 64, 3, 16>;
fn engine() -> Engine {
    Engine::new(metadata(), configs(), lc(), vc()).unwrap()
}
fn events(s: StructureScenario) -> Vec<market_events::MarketEvent> {
    let mut events = Vec::new();
    generate(configs().map(|c| c.venue), [InstrumentId(1); 3], s, |e| {
        events.push(e);
        Ok::<_, ()>(())
    })
    .unwrap();
    events
}
#[test]
fn synthetic_global_and_local_voids_activate_on_touch() {
    for scenario in [StructureScenario::Revisit, StructureScenario::LocalVoid] {
        let mut e = engine();
        let mut touch_seen = false;
        for event in events(scenario) {
            e.apply(&event).unwrap();
            if event.event_type == market_events::MarketEventType::Trade
                && event.price_ticks == PriceTicks(103)
            {
                assert_eq!(e.voids().metrics().revisits, 1);
                assert_eq!(
                    e.voids()
                        .zones()
                        .iter()
                        .flatten()
                        .find(|z| z.revisit_count > 0)
                        .unwrap()
                        .max_penetration_ppm,
                    0
                );
                touch_seen = true;
            }
        }
        assert!(touch_seen);
        assert_eq!(e.voids().metrics().registered, 1);
        assert_eq!(e.voids().metrics().revisits, 1);
        assert!(e.liquidity().metrics().pool_activations > 0);
        let z = e
            .voids()
            .zones()
            .iter()
            .flatten()
            .find(|z| z.registered_at.is_some())
            .unwrap();
        assert_eq!(z.state, VoidState::Refilled);
        assert_eq!(z.max_penetration_ppm, 333_333);
        assert_eq!(
            z.scope,
            if scenario == StructureScenario::LocalVoid {
                Scope::VenueLocal
            } else {
                Scope::MarketWide
            }
        );
    }
}
#[test]
fn refill_before_touch_and_one_way_continuation_do_not_create_revisits() {
    for s in [
        StructureScenario::RefillBeforeRevisit,
        StructureScenario::Continuation,
    ] {
        let mut e = engine();
        for event in events(s) {
            e.apply(&event).unwrap();
        }
        assert_eq!(e.voids().metrics().registered, 1);
        assert_eq!(e.voids().metrics().revisits, 0);
        if s == StructureScenario::RefillBeforeRevisit {
            assert_eq!(e.voids().metrics().refilled_before_revisit, 1);
        }
    }
}
#[test]
fn missing_coverage_does_not_become_empty_liquidity() {
    let mut config = lc();
    config.assume_contiguous_l2_coverage = false;
    let mut e = Engine::new(metadata(), configs(), config, vc()).unwrap();
    for event in events(StructureScenario::Revisit) {
        e.apply(&event).unwrap();
    }
    assert_eq!(e.voids().metrics().candidates, 0);
    assert_eq!(e.liquidity().metrics().pool_activations, 0);
    assert!(e.liquidity().metrics().observations_without_coverage > 0);
}
#[test]
fn stale_sources_invalidate_history_and_faults_stop_research() {
    let mut e = engine();
    for event in events(StructureScenario::Continuation) {
        e.apply(&event).unwrap();
    }
    e.advance_time(Timestamp(2_000_000_000)).unwrap();
    assert!(
        e.voids()
            .zones()
            .iter()
            .flatten()
            .filter(|z| z.registered_at.is_some())
            .all(|z| z.state == VoidState::Invalidated)
    );
    assert!(e.advance_time(Timestamp(1)).is_err());
    assert!(e.faulted());
    assert_eq!(
        e.advance_time(Timestamp(3_000_000_000)),
        Err(ResearchError::Faulted)
    );
}
#[test]
fn merged_binary_replay_matches_complete_research_state_after_every_event() {
    let source = events(StructureScenario::Revisit);
    let mut direct = engine();
    let mut writers: Vec<_> = configs()
        .into_iter()
        .map(|c| {
            Recorder::new(
                Vec::new(),
                RecordingMetadata {
                    venue: c.venue,
                    instrument: InstrumentId(1),
                    price_decimals: 0,
                    quantity_decimals: 0,
                    tick_atoms: 1,
                    quantity_atoms: 1,
                },
            )
            .unwrap()
        })
        .collect();
    for event in &source {
        writers[event.venue.0 as usize - 1].append(event).unwrap();
    }
    let data: Vec<_> = writers.into_iter().map(|w| w.finish().unwrap()).collect();
    let mut r = MergedReplay::new(
        [data[2].as_slice(), data[0].as_slice(), data[1].as_slice()],
        true,
    )
    .unwrap();
    let mut replayed = engine();
    r.validate(replayed.market()).unwrap();
    for expected in &source {
        let actual = r.next_event().unwrap().unwrap();
        assert_eq!(*expected, actual);
        direct.apply(expected).unwrap();
        replayed.apply(&actual).unwrap();
        assert_eq!(direct, replayed);
    }
    assert!(r.next_event().unwrap().is_none());
}
