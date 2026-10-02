use common::*;
use consolidator::{VenueConfig, normalization::*};
use fixed_point::*;
use liquidity::{research::*, *};
use market_events::{MarketEvent, MarketEventType as K};
use simulation::structures::{StructureScenario, generate};
use voids::*;
pub fn metadata() -> InstrumentMetadata {
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
pub fn configs() -> [VenueConfig; 3] {
    std::array::from_fn(|v| VenueConfig {
        venue: VenueId(v as u16 + 1),
        metadata: metadata(),
        weight_ppm: [1_000_000, 750_000, 500_000][v],
        stale_after_ns: u64::MAX,
    })
}
pub fn lc() -> LiquidityConfig<3> {
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
pub fn vc() -> VoidConfig {
    VoidConfig {
        minimum_width_ticks: 3,
        minimum_persistence_ns: 3_000_000,
        maximum_age_ns: u64::MAX,
        low_depth_ppm: 250_000,
        refill_depth_ppm: 800_000,
        minimum_score_ppm: 750_000,
        coverage_loss: voids::CoverageLoss::Invalidate,
    }
}
pub type Engine = ResearchEngine<3, 128, 384, 64, 3, 16>;
pub fn engine() -> Engine {
    Engine::new(metadata(), configs(), lc(), vc()).unwrap()
}

pub fn setup() -> (Engine, u64, [u64; 3]) {
    let mut engine = engine();
    let mut now = 0;
    let mut sequences = [0; 3];
    generate(
        configs().map(|c| c.venue),
        [InstrumentId(1); 3],
        StructureScenario::Continuation,
        |event| {
            now = event.receive_ts.0;
            sequences[event.venue.0 as usize - 1] = event.sequence;
            engine.apply(&event)
        },
    )
    .unwrap();
    assert_eq!(engine.voids().metrics().registered, 1);
    (engine, now, sequences)
}
pub fn advance(now: &mut u64, sequences: &mut [u64; 3]) -> MarketEvent {
    *now += 1;
    let v = *now as usize % 3;
    sequences[v] += 1;
    MarketEvent {
        venue: VenueId(v as u16 + 1),
        instrument: InstrumentId(1),
        sequence: sequences[v],
        exchange_sequence: sequences[v],
        exchange_ts: *now,
        receive_ts: Timestamp(*now),
        event_type: K::Modify,
        side: Side::Buy,
        price_ticks: PriceTicks(96),
        qty_units: QtyUnits(100 + (*now % 19) as i64),
    }
}
