//! Incremental fixed-grid liquidity research. All scores are explicit integer ratios.
#![forbid(unsafe_code)]
pub mod grid;
pub mod research;
use common::{Side, Timestamp};
use fixed_point::PriceTicks;
use grid::PriceGrid;
pub use voids::PriceRegion;
pub const PPM: i128 = 1_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityError {
    InvalidConfig,
    InvalidObservation,
    Overflow,
    ClockRegression,
    Faulted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceReference {
    /// Consolidated touch midpoint; research suspends unless the consolidated market is open.
    Midpoint,
    LastTrade,
    /// Weight-averaged midpoint of valid venues (`Consolidator::composite_midpoint_x2`).
    /// Research continues while the consolidated touch is crossed across venues; it suspends
    /// only when no venue has a valid two-sided book.
    Composite,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidityConfig<const B: usize> {
    pub lower_price: PriceTicks,
    pub cell_width: usize,
    pub bucket_edges_ppm: [u32; B],
    pub sample_interval_ns: u64,
    pub warmup_samples: u32,
    pub baseline_alpha_ppm: u32,
    pub minimum_baseline_units: i64,
    pub minimum_covered_venues: u32,
    pub assume_contiguous_l2_coverage: bool,
    pub pool_depth_ratio_ppm: u32,
    pub pool_persistence_ns: u64,
    pub pool_minimum_score_ppm: u32,
    pub pool_weights: [u32; 3],
    pub price_reference: PriceReference,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreComponents {
    pub depth_norm_ppm: u32,
    pub persistence_ppm: u32,
    pub stability_ppm: u32,
}
/// Weighted arithmetic mean of independently visible terms; weights need not sum to one.
pub fn pool_score(c: ScoreComponents, weights: [u32; 3]) -> Result<u32, LiquidityError> {
    let denominator: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    if denominator == 0 {
        return Err(LiquidityError::InvalidConfig);
    }
    let numerator = u128::from(c.depth_norm_ppm) * u128::from(weights[0])
        + u128::from(c.persistence_ppm) * u128::from(weights[1])
        + u128::from(c.stability_ppm) * u128::from(weights[2]);
    u32::try_from(numerator / denominator).map_err(|_| LiquidityError::Overflow)
}
/// Future flow components must be supplied explicitly when their weight is nonzero.
pub fn void_score(
    depth_norm_ppm: u32,
    executed_norm_ppm: Option<u32>,
    cancellation_ppm: Option<u32>,
    weights: [u32; 3],
) -> Result<u32, LiquidityError> {
    let execution = if weights[1] == 0 {
        0
    } else {
        executed_norm_ppm.ok_or(LiquidityError::InvalidObservation)?
    };
    let cancellation = if weights[2] == 0 {
        0
    } else {
        cancellation_ppm.ok_or(LiquidityError::InvalidObservation)?
    };
    pool_score(
        ScoreComponents {
            depth_norm_ppm: 1_000_000 - depth_norm_ppm.min(1_000_000),
            persistence_ppm: 1_000_000 - execution.min(1_000_000),
            stability_ppm: cancellation.min(1_000_000),
        },
        weights,
    )
}
fn norm(depth: i128, baseline: i128) -> u32 {
    ((depth * PPM / baseline.max(1)).min(i128::from(u32::MAX))) as u32
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Baseline {
    value: i128,
    samples: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    Bid,
    Ask,
    TwoSided,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolObservation {
    pub kind: PoolKind,
    pub venue_mask: u64,
    pub score_ppm: u32,
    pub components: ScoreComponents,
    pub persistent_ns: u64,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CellObservation {
    pub bid_depth: i128,
    pub ask_depth: i128,
    pub baseline: i128,
    pub covered_mask: u64,
    pub depleted_mask: u64,
    pub pool: Option<PoolObservation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell<const V: usize> {
    baselines: [Baseline; V],
    previous_depth: i128,
    pool_since: Option<Timestamp>,
    pool_active: bool,
    observation: CellObservation,
}
impl<const V: usize> Cell<V> {
    fn new() -> Self {
        Self {
            baselines: [Baseline::default(); V],
            previous_depth: 0,
            pool_since: None,
            pool_active: false,
            observation: CellObservation::default(),
        }
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LiquidityMetrics {
    pub samples: u64,
    pub pool_activations: u64,
    pub pool_observed_ns: u128,
    pub structural_resets: u64,
    pub observations_without_coverage: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketObservation {
    pub bid_depth: i128,
    pub ask_depth: i128,
    pub complete_corridor: bool,
    pub displayed_levels: u32,
    pub average_displayed_level_age_ns: Option<u64>,
    pub order_count: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityEngine<const V: usize, const P: usize, const B: usize> {
    pub(crate) grid: PriceGrid<V, P>,
    config: LiquidityConfig<B>,
    weights: [u32; V],
    cells: [Cell<V>; P],
    last_sample: Option<Timestamp>,
    last_time: Option<Timestamp>,
    metrics: LiquidityMetrics,
}
impl<const V: usize, const P: usize, const B: usize> LiquidityEngine<V, P, B> {
    pub fn new(config: LiquidityConfig<B>, weights: [u32; V]) -> Result<Self, LiquidityError> {
        if B == 0
            || config.bucket_edges_ppm[0] == 0
            || config.bucket_edges_ppm.windows(2).any(|w| w[0] >= w[1])
            || config.bucket_edges_ppm[B - 1] > 1_000_000
            || config.sample_interval_ns == 0
            || config.warmup_samples < 2
            || config.baseline_alpha_ppm == 0
            || config.baseline_alpha_ppm > 1_000_000
            || config.minimum_baseline_units <= 0
            || config.minimum_covered_venues == 0
            || config.minimum_covered_venues > V as u32
            || config.minimum_covered_venues > weights.iter().filter(|&&w| w > 0).count() as u32
            || config.pool_depth_ratio_ppm <= 1_000_000
            || config.pool_persistence_ns == 0
            || config.pool_weights == [0; 3]
        {
            return Err(LiquidityError::InvalidConfig);
        }
        Ok(Self {
            grid: PriceGrid::new(config.lower_price, config.cell_width, weights)?,
            config,
            weights,
            cells: [Cell::new(); P],
            last_sample: None,
            last_time: None,
            metrics: LiquidityMetrics::default(),
        })
    }
    pub fn config(&self) -> LiquidityConfig<B> {
        self.config
    }
    pub fn update_level(
        &mut self,
        venue: usize,
        side: Side,
        price: PriceTicks,
        qty: i64,
        now: Timestamp,
    ) -> Result<bool, LiquidityError> {
        self.grid.set(venue, side, price, qty, now)
    }
    pub fn metrics(&self) -> LiquidityMetrics {
        self.metrics
    }
    pub fn suspend(&mut self) {
        self.cells = [Cell::new(); P];
        self.last_sample = None;
    }
    pub fn cell(&self, i: usize) -> Option<CellObservation> {
        if i < self.grid.cell_count() {
            Some(self.cells[i].observation)
        } else {
            None
        }
    }
    pub fn cell_count(&self) -> usize {
        self.grid.cell_count()
    }
    pub fn region(&self, i: usize) -> PriceRegion {
        self.grid.region(i)
    }
    pub fn reset(&mut self) -> Result<(), LiquidityError> {
        self.grid = PriceGrid::new(
            self.config.lower_price,
            self.config.cell_width,
            self.weights,
        )?;
        self.cells = [Cell::new(); P];
        self.last_sample = None;
        self.metrics.structural_resets += 1;
        Ok(())
    }
    pub fn due(&self, now: Timestamp) -> bool {
        self.last_sample
            .is_none_or(|t| now.0.saturating_sub(t.0) >= self.config.sample_interval_ns)
    }
    pub fn sample(
        &mut self,
        now: Timestamp,
        coverage: &[u64],
        low_ppm: u32,
    ) -> Result<(), LiquidityError> {
        if self.last_time.is_some_and(|t| now < t) {
            return Err(LiquidityError::ClockRegression);
        }
        self.last_time = Some(now);
        let allowed =
            self.weights.iter().enumerate().fold(
                0_u64,
                |mask, (i, &w)| if w > 0 { mask | (1 << i) } else { mask },
            );
        if coverage.len() != self.cell_count()
            || low_ppm > 1_000_000
            || coverage.iter().any(|&mask| mask & !allowed != 0)
        {
            return Err(LiquidityError::InvalidObservation);
        }
        if !self.due(now) {
            return Ok(());
        }
        let dt = self.last_sample.map_or(0, |t| now.0 - t.0);
        for (i, &mask) in coverage.iter().enumerate() {
            let region = self.grid.region(i);
            let bid = self.grid.depth(
                Side::Buy,
                i128::from(region.lower.0),
                i128::from(region.upper.0),
            );
            let ask = self.grid.depth(
                Side::Sell,
                i128::from(region.lower.0),
                i128::from(region.upper.0),
            );
            let cell = &mut self.cells[i];
            // Persistence describes one continuous observation cohort.
            if mask != cell.observation.covered_mask {
                cell.pool_since = None;
                cell.pool_active = false;
                cell.previous_depth = 0;
            }
            let (mut baseline, mut depleted, mut pool_mask, mut mature_depth) =
                (0_i128, 0_u64, 0_u64, 0_i128);
            for v in 0..V {
                let reference = &mut cell.baselines[v];
                if mask & (1 << v) == 0 {
                    *reference = Baseline::default();
                    continue;
                }
                let depth = self.grid.local_depth(v, i);
                let mature = reference.samples >= self.config.warmup_samples
                    && reference.value >= i128::from(self.config.minimum_baseline_units);
                if mature {
                    mature_depth += depth * i128::from(self.weights[v]);
                    baseline += reference.value * i128::from(self.weights[v]);
                    let ratio = norm(depth, reference.value);
                    if ratio <= low_ppm {
                        depleted |= 1 << v;
                    }
                    if ratio >= self.config.pool_depth_ratio_ppm {
                        pool_mask |= 1 << v;
                    }
                }
                // Freeze established reference in thin regions; otherwise an event-sampled EWMA.
                if !mature || norm(depth, reference.value) > low_ppm {
                    if reference.samples == 0 {
                        reference.value = depth;
                    } else {
                        reference.value += (depth - reference.value)
                            * i128::from(self.config.baseline_alpha_ppm)
                            / PPM;
                    }
                }
                reference.samples = reference.samples.saturating_add(1);
            }
            let covered = mask.count_ones() >= self.config.minimum_covered_venues;
            let total = mature_depth;
            let ratio = norm(total, baseline);
            let stability = 1_000_000
                - ((total - cell.previous_depth).abs() * PPM
                    / total.max(cell.previous_depth).max(1))
                .min(PPM) as u32;
            if !covered {
                self.metrics.observations_without_coverage += 1;
                depleted = 0;
            }
            let pool = if covered
                && baseline > 0
                && ratio >= self.config.pool_depth_ratio_ppm
                && pool_mask != 0
            {
                let start = *cell.pool_since.get_or_insert(now);
                let elapsed = now.0 - start.0;
                let components = ScoreComponents {
                    depth_norm_ppm: ratio,
                    persistence_ppm: (u128::from(elapsed) * 1_000_000
                        / u128::from(self.config.pool_persistence_ns))
                    .min(1_000_000) as u32,
                    stability_ppm: stability,
                };
                let score = pool_score(components, self.config.pool_weights)?;
                if elapsed >= self.config.pool_persistence_ns
                    && score >= self.config.pool_minimum_score_ppm
                {
                    Some(PoolObservation {
                        kind: if bid > 0 && ask > 0 {
                            PoolKind::TwoSided
                        } else if bid > 0 {
                            PoolKind::Bid
                        } else {
                            PoolKind::Ask
                        },
                        venue_mask: pool_mask,
                        score_ppm: score,
                        components,
                        persistent_ns: elapsed,
                    })
                } else {
                    None
                }
            } else {
                cell.pool_since = None;
                None
            };
            if pool.is_some() && !cell.pool_active {
                self.metrics.pool_activations += 1;
            }
            if cell.pool_active && pool.is_some() {
                self.metrics.pool_observed_ns += u128::from(dt);
            }
            cell.pool_active = pool.is_some();
            cell.previous_depth = total;
            cell.observation = CellObservation {
                bid_depth: bid,
                ask_depth: ask,
                baseline,
                covered_mask: mask,
                depleted_mask: depleted,
                pool,
            };
        }
        self.metrics.samples += 1;
        self.last_sample = Some(now);
        Ok(())
    }
    pub fn evidence(&self, cell: usize, mask: u64) -> (i128, i128) {
        let (mut depth, mut baseline) = (0, 0);
        for v in 0..V {
            if mask & (1 << v) != 0 {
                depth += self.grid.local_depth(v, cell) * i128::from(self.weights[v]);
                baseline += self.cells[cell].baselines[v].value * i128::from(self.weights[v]);
            }
        }
        (depth, baseline)
    }
    pub fn buckets(
        &self,
        midpoint_x2: i128,
        now: Timestamp,
    ) -> Result<[BucketObservation; B], LiquidityError> {
        if midpoint_x2 <= 0
            || midpoint_x2 > 2 * i128::from(i64::MAX)
            || self.last_time.is_some_and(|t| now < t)
        {
            return Err(LiquidityError::InvalidObservation);
        }
        // Bounds are derived by integer inequalities, preserving half-tick midpoints.
        let mut inner = 0_i128;
        let mut result = [BucketObservation {
            bid_depth: 0,
            ask_depth: 0,
            complete_corridor: false,
            displayed_levels: 0,
            average_displayed_level_age_ns: None,
            order_count: None,
        }; B];
        for (i, bucket) in result.iter_mut().enumerate() {
            let outer = i128::from(self.config.bucket_edges_ppm[i]);
            let bl = (midpoint_x2
                .checked_mul(PPM - outer)
                .ok_or(LiquidityError::Overflow)?)
            .div_euclid(2 * PPM)
                + 1;
            let bu = (midpoint_x2
                .checked_mul(PPM - inner)
                .ok_or(LiquidityError::Overflow)?)
            .div_euclid(2 * PPM);
            let al = (midpoint_x2
                .checked_mul(PPM + inner)
                .ok_or(LiquidityError::Overflow)?
                + 2 * PPM
                - 1)
            .div_euclid(2 * PPM);
            let au = (midpoint_x2
                .checked_mul(PPM + outer)
                .ok_or(LiquidityError::Overflow)?
                + 2 * PPM
                - 1)
            .div_euclid(2 * PPM)
                - 1;
            let (nb, ab) = self.grid.age(Side::Buy, bl, bu, now)?;
            let (na, aa) = self.grid.age(Side::Sell, al, au, now)?;
            let n = nb + na;
            *bucket = BucketObservation {
                bid_depth: self.grid.depth(Side::Buy, bl, bu),
                ask_depth: self.grid.depth(Side::Sell, al, au),
                complete_corridor: bl >= i128::from(self.grid.lower())
                    && au <= i128::from(self.grid.upper()),
                displayed_levels: n,
                average_displayed_level_age_ns: if n == 0 {
                    None
                } else {
                    Some(((ab + aa) / u128::from(n)) as u64)
                },
                order_count: None,
            };
            inner = outer;
        }
        Ok(result)
    }
}
