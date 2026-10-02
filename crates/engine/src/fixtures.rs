//! Synthetic research configuration shared by tests, benchmarks and the CLI TOML files.
//! These values are test data for the coarse synthetic grid, not venue calibrations.
use super::*;
use common::InstrumentId;
use consolidator::normalization::{ContractKind, MarketIdentity, UnitScale};
use fixed_point::{InventoryUnits, Quantum};
use liquidity::PriceReference;
use risk::InventoryLimits;
use toxicity::Environment;
/// 3 venues, 128 levels per book side, 384 consolidated levels, 64-tick corridor, 6 buckets,
/// 16 zone slots, 128-entry windows, 32 inventory slots, 8 completed episodes.
pub type SyntheticEngine = Engine<3, 128, 384, 64, 6, 16, 128, 32, 8>;
pub fn market() -> (InstrumentMetadata, [VenueConfig; 3]) {
    let market = MarketIdentity {
        base_asset: 1,
        quote_asset: 2,
        settlement_asset: 2,
        kind: ContractKind::Spot,
        equivalence_group: 1,
    };
    let unit = UnitScale {
        atoms: 1,
        decimals: 0,
    };
    let metadata = |instrument| InstrumentMetadata {
        instrument: InstrumentId(instrument),
        market,
        price: unit,
        quantity: unit,
    };
    let venue = |id, instrument, weight_ppm| VenueConfig {
        venue: VenueId(id),
        metadata: metadata(instrument),
        weight_ppm,
        stale_after_ns: 1_000_000_000,
    };
    (
        metadata(1),
        [
            venue(1, 101, 1_000_000),
            venue(2, 202, 750_000),
            venue(3, 303, 500_000),
        ],
    )
}
pub fn structures() -> (LiquidityConfig<6>, VoidConfig) {
    (
        LiquidityConfig {
            lower_price: PriceTicks(80),
            cell_width: 4,
            bucket_edges_ppm: [10_000, 25_000, 50_000, 100_000, 250_000, 500_000],
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
            price_reference: PriceReference::Midpoint,
        },
        VoidConfig {
            minimum_width_ticks: 3,
            minimum_persistence_ns: 3_000_000,
            maximum_age_ns: 30_000_000_000,
            low_depth_ppm: 250_000,
            refill_depth_ppm: 800_000,
            minimum_score_ppm: 750_000,
            coverage_loss: voids::CoverageLoss::Invalidate,
        },
    )
}
pub fn flow() -> FlowConfig {
    FlowConfig {
        event_window: 16,
        time_window_ns: 100_000_000,
        qi_levels: 2,
        removal_attribution: RemovalAttribution::Unknown,
    }
}
pub fn inventory() -> InventoryConfig<3> {
    InventoryConfig {
        venues: [VenueId(1), VenueId(2), VenueId(3)],
        quantum: Quantum::new(QtyUnits(100), 1, 10).expect("exact quantum"),
        target: InventoryUnits(0),
        target_tolerance: 0,
        completion_max_gross: 0,
        limits: InventoryLimits {
            max_net_units: 4,
            max_gross_units: 8,
            max_venue_units: 6,
            max_target_deviation: 4,
            max_open_orders: 8,
            max_liability: Money(300),
            max_drawdown: Money(1_000),
            max_position_age_ns: 20_000_000_000,
            max_episode_ns: 30_000_000_000,
            max_adverse_episodes: 3,
            mark_stale_ns: 1_000_000_000,
        },
    }
}
pub fn engine() -> EngineConfig {
    EngineConfig {
        environment: EnvironmentConfig {
            window_ns: 120_000_000,
            shock_spread_ticks: 4,
            shock_min_top_qty: 1,
            chaotic_spread_changes: 40,
            trend_min_move_ticks: 4,
            trend_efficiency_ppm: 700_000,
            trend_min_aggressive_qty: 400,
            trend_aggression_ppm: 900_000,
            active_min_moves: 2,
            active_min_prints: 2,
        },
        policy: PolicyConfig {
            allowed_scopes: MULTI_VENUE | MARKET_WIDE,
            entry_levels: 1,
            entry_max_abs_net: 2,
            harvest_ticks: 1,
            rebalance_threshold_units: 1,
            rebalance_age_ns: 600_000_000,
            rebalance_improve_ticks: 0,
            reprice_ticks: 2,
            evidence_ttl_ns: 50_000_000,
            exit_on_environment: Environment::Chaotic.bit(),
            entry_environments: Environment::BalancedActive.bit(),
            mark_refresh_ns: 100_000_000,
            assess_interval_ns: 10_000_000,
        },
        fills: FillConfig {
            model: FillModel::StrictTradeThrough,
            schedule: FeeSchedule::default(),
            maker_rebate: Money(1),
            maker_fee: Money(0),
            taker_fee: Money(3),
            emergency_slippage_ticks: 1,
        },
        funding: FundingConfig::default(),
        markout: MarkoutConfig::spec(),
        objective: None,
    }
}
pub fn build() -> Result<SyntheticEngine, EngineError> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    SyntheticEngine::new(
        grid,
        venues,
        liquidity,
        voids,
        flow(),
        inventory(),
        engine(),
    )
}
