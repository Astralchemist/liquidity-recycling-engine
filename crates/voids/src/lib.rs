//! Bounded, deterministic structural zones. No orders, inventory, or directional forecasts.
use common::Timestamp;
use fixed_point::PriceTicks;
pub const PPM: i128 = 1_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceRegion {
    pub lower: PriceTicks,
    pub upper: PriceTicks,
}
impl PriceRegion {
    pub fn contains(self, price_x2: i128) -> bool {
        price_x2 >= i128::from(self.lower.0) * 2 && price_x2 <= i128::from(self.upper.0) * 2
    }
    fn overlaps(self, other: Self) -> bool {
        self.lower <= other.upper && other.lower <= self.upper
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoidState {
    Forming,
    Persisted,
    Exited,
    Revisited,
    Refilled,
    Expired,
    Invalidated,
}
impl VoidState {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Forming | Self::Persisted | Self::Exited | Self::Revisited
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenetrationBand {
    Touch,
    BelowQuarter,
    Quarter,
    Half,
    ThreeQuarters,
    Full,
}
impl PenetrationBand {
    pub fn from_ppm(value: u32) -> Self {
        match value {
            0 => Self::Touch,
            1..250_000 => Self::BelowQuarter,
            250_000..500_000 => Self::Quarter,
            500_000..750_000 => Self::Half,
            750_000..1_000_000 => Self::ThreeQuarters,
            _ => Self::Full,
        }
    }
    fn index(self) -> usize {
        self as usize
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    VenueLocal,
    MultiVenue,
    MarketWide,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    Below,
    Inside,
    Above,
}
fn location(region: PriceRegion, price: i128) -> Location {
    if price < i128::from(region.lower.0) * 2 {
        Location::Below
    } else if price > i128::from(region.upper.0) * 2 {
        Location::Above
    } else {
        Location::Inside
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoidConfig {
    pub minimum_width_ticks: i64,
    pub minimum_persistence_ns: u64,
    pub maximum_age_ns: u64,
    pub low_depth_ppm: u32,
    pub refill_depth_ppm: u32,
    pub minimum_score_ppm: u32,
}
impl VoidConfig {
    pub fn validate(self) -> Result<(), VoidError> {
        if self.minimum_width_ticks <= 0
            || self.minimum_persistence_ns == 0
            || self.maximum_age_ns <= self.minimum_persistence_ns
            || self.low_depth_ppm >= self.refill_depth_ppm
            || self.refill_depth_ppm > 1_000_000
            || self.minimum_score_ppm > 1_000_000
        {
            Err(VoidError::InvalidConfig)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormationEvidence {
    pub depth: i128,
    pub baseline: i128,
    pub score_ppm: u32,
    pub venue_mask: u64,
    pub eligible_mask: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidityVoid {
    pub id: u64,
    pub region: PriceRegion,
    pub state: VoidState,
    pub scope: Scope,
    pub venue_mask: u64,
    pub created_at: Timestamp,
    pub registered_at: Option<Timestamp>,
    pub last_touched_at: Option<Timestamp>,
    pub first_revisit_at: Option<Timestamp>,
    pub formation_depth: i128,
    pub current_depth: i128,
    pub baseline: i128,
    pub formation_score_ppm: u32,
    pub persistence_ppm: u32,
    pub revisit_count: u32,
    pub max_penetration_ppm: u32,
    pub visit_peak_ppm: u32,
    pub ended_at: Option<Timestamp>,
    pub refilled_before_revisit: bool,
    previous: Location,
    entry_from: Location,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VoidMetrics {
    pub candidates: u64,
    pub candidate_restarts: u64,
    pub registered: u64,
    pub revisited_zones: u64,
    pub revisits: u64,
    pub first_revisit_latency_ns: u128,
    pub refilled: u64,
    pub refilled_before_revisit: u64,
    pub expired: u64,
    pub invalidated: u64,
    pub capacity_rejections: u64,
    pub overlap_rejections: u64,
    pub evicted_terminal: u64,
    pub skipped_crossings: u64,
    /// Each revisit contributes exactly one bin: its largest observed penetration so far.
    pub penetration_distribution: [u64; 6],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoidError {
    InvalidConfig,
    InvalidObservation,
    ClockRegression,
    Overflow,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Considered {
    Created(u64),
    Existing(u64),
    Rejected,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoidEngine<const Z: usize> {
    config: VoidConfig,
    zones: [Option<LiquidityVoid>; Z],
    metrics: VoidMetrics,
    next_id: u64,
    last_time: Option<Timestamp>,
}
fn ratio(depth: i128, baseline: i128) -> Result<u32, VoidError> {
    if depth < 0 || baseline <= 0 {
        return Err(VoidError::InvalidObservation);
    }
    let raw = depth.checked_mul(PPM).ok_or(VoidError::Overflow)? / baseline;
    Ok(raw.min(i128::from(u32::MAX)) as u32)
}
impl<const Z: usize> VoidEngine<Z> {
    pub fn new(config: VoidConfig) -> Result<Self, VoidError> {
        config.validate()?;
        if Z == 0 {
            return Err(VoidError::InvalidConfig);
        }
        Ok(Self {
            config,
            zones: [None; Z],
            metrics: VoidMetrics::default(),
            next_id: 1,
            last_time: None,
        })
    }
    pub fn zones(&self) -> &[Option<LiquidityVoid>; Z] {
        &self.zones
    }
    pub fn metrics(&self) -> VoidMetrics {
        self.metrics
    }
    pub fn config(&self) -> VoidConfig {
        self.config
    }
    fn clock(&mut self, now: Timestamp) -> Result<(), VoidError> {
        if self.last_time.is_some_and(|t| now < t) {
            return Err(VoidError::ClockRegression);
        }
        self.last_time = Some(now);
        Ok(())
    }
    pub fn invalidate_all(&mut self, now: Timestamp) -> Result<(), VoidError> {
        self.clock(now)?;
        for z in self.zones.iter_mut().flatten() {
            if z.state.active() {
                z.state = VoidState::Invalidated;
                z.ended_at = Some(now);
                self.metrics.invalidated += 1;
            }
        }
        Ok(())
    }
    pub fn consider(
        &mut self,
        now: Timestamp,
        region: PriceRegion,
        e: FormationEvidence,
        price_x2: i128,
    ) -> Result<Considered, VoidError> {
        self.clock(now)?;
        if region.lower.0 <= 0
            || region.upper <= region.lower
            || price_x2 <= 0
            || e.venue_mask == 0
            || e.venue_mask & !e.eligible_mask != 0
            || e.score_ppm > 1_000_000
        {
            return Err(VoidError::InvalidObservation);
        }
        if region.upper.0 - region.lower.0 < self.config.minimum_width_ticks
            || e.score_ppm < self.config.minimum_score_ppm
            || ratio(e.depth, e.baseline)? > self.config.low_depth_ppm
        {
            return Ok(Considered::Rejected);
        }
        let scope = if e.venue_mask == e.eligible_mask && e.venue_mask.count_ones() > 1 {
            Scope::MarketWide
        } else if e.venue_mask.count_ones() == 1 {
            Scope::VenueLocal
        } else {
            Scope::MultiVenue
        };
        for z in self.zones.iter_mut().flatten() {
            if !z.state.active() || !z.region.overlaps(region) {
                continue;
            }
            if z.region == region {
                if matches!(z.state, VoidState::Forming | VoidState::Persisted)
                    && z.venue_mask != e.venue_mask
                {
                    z.venue_mask = e.venue_mask;
                    z.scope = scope;
                    z.created_at = now;
                    z.state = VoidState::Forming;
                    z.baseline = e.baseline;
                    z.formation_depth = e.depth;
                    z.current_depth = e.depth;
                    z.formation_score_ppm = e.score_ppm;
                    z.persistence_ppm = 0;
                    z.previous = location(region, price_x2);
                    self.metrics.candidate_restarts += 1;
                }
                return Ok(Considered::Existing(z.id));
            }
            self.metrics.overlap_rejections += 1;
            return Ok(Considered::Rejected);
        }
        let slot = self.zones.iter().position(Option::is_none).or_else(|| {
            self.zones
                .iter()
                .enumerate()
                .filter_map(|(i, z)| z.filter(|z| !z.state.active()).map(|z| (i, z.id)))
                .min_by_key(|(_, id)| *id)
                .map(|(i, _)| i)
        });
        let Some(slot) = slot else {
            self.metrics.capacity_rejections += 1;
            return Ok(Considered::Rejected);
        };
        if self.zones[slot].is_some() {
            self.metrics.evicted_terminal += 1;
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(VoidError::Overflow)?;
        self.zones[slot] = Some(LiquidityVoid {
            id,
            region,
            state: VoidState::Forming,
            scope,
            venue_mask: e.venue_mask,
            created_at: now,
            registered_at: None,
            last_touched_at: None,
            first_revisit_at: None,
            formation_depth: e.depth,
            current_depth: e.depth,
            baseline: e.baseline,
            formation_score_ppm: e.score_ppm,
            persistence_ppm: 0,
            revisit_count: 0,
            max_penetration_ppm: 0,
            visit_peak_ppm: 0,
            ended_at: None,
            refilled_before_revisit: false,
            previous: location(region, price_x2),
            entry_from: Location::Inside,
        });
        self.metrics.candidates += 1;
        Ok(Considered::Created(id))
    }
    /// Every price event, including boundary touches, is evaluated; no second traversal gate.
    /// Coverage/refill/expiry have precedence over revisit. Depth is for the frozen venue mask.
    pub fn observe(
        &mut self,
        slot: usize,
        now: Timestamp,
        price_x2: i128,
        depth: i128,
        covered: bool,
    ) -> Result<(), VoidError> {
        self.clock(now)?;
        if slot >= Z || price_x2 <= 0 || depth < 0 {
            return Err(VoidError::InvalidObservation);
        }
        let Some(z) = self.zones[slot].as_mut() else {
            return Ok(());
        };
        if !z.state.active() {
            return Ok(());
        }
        z.current_depth = depth;
        if !covered {
            z.state = VoidState::Invalidated;
            z.ended_at = Some(now);
            self.metrics.invalidated += 1;
            return Ok(());
        }
        let origin = z.registered_at.unwrap_or(z.created_at);
        if now.0 - origin.0 >= self.config.maximum_age_ns {
            z.state = VoidState::Expired;
            z.ended_at = Some(now);
            self.metrics.expired += 1;
            return Ok(());
        }
        let norm = ratio(depth, z.baseline)?;
        if norm >= self.config.refill_depth_ppm {
            if z.registered_at.is_some() {
                z.state = VoidState::Refilled;
                z.refilled_before_revisit = z.revisit_count == 0;
                self.metrics.refilled += 1;
                if z.refilled_before_revisit {
                    self.metrics.refilled_before_revisit += 1;
                }
            } else {
                z.state = VoidState::Invalidated;
                self.metrics.invalidated += 1;
            }
            z.ended_at = Some(now);
            return Ok(());
        }
        if matches!(z.state, VoidState::Forming | VoidState::Persisted)
            && norm > self.config.low_depth_ppm
        {
            z.state = VoidState::Invalidated;
            z.ended_at = Some(now);
            self.metrics.invalidated += 1;
            return Ok(());
        }
        let here = location(z.region, price_x2);
        if z.state == VoidState::Forming {
            z.persistence_ppm = (u128::from(now.0 - z.created_at.0) * 1_000_000
                / u128::from(self.config.minimum_persistence_ns))
            .min(1_000_000) as u32;
            if z.persistence_ppm == 1_000_000 {
                z.state = VoidState::Persisted;
            }
        }
        if z.state == VoidState::Persisted
            && z.previous == Location::Inside
            && here != Location::Inside
        {
            z.state = VoidState::Exited;
            z.registered_at = Some(now);
            self.metrics.registered += 1;
        } else if z.state == VoidState::Exited && here == Location::Inside {
            z.state = VoidState::Revisited;
            z.entry_from = z.previous;
            z.visit_peak_ppm = 0;
            z.revisit_count = z.revisit_count.checked_add(1).ok_or(VoidError::Overflow)?;
            self.metrics.revisits += 1;
            self.metrics.penetration_distribution[0] += 1;
            if z.first_revisit_at.is_none() {
                z.first_revisit_at = Some(now);
                self.metrics.revisited_zones += 1;
                self.metrics.first_revisit_latency_ns +=
                    u128::from(now.0 - z.registered_at.expect("registered").0);
            }
        } else if z.state == VoidState::Exited && here != z.previous {
            self.metrics.skipped_crossings += 1;
        }
        if z.state == VoidState::Revisited {
            let lower = i128::from(z.region.lower.0) * 2;
            let upper = i128::from(z.region.upper.0) * 2;
            let entered = match z.entry_from {
                Location::Below => price_x2 - lower,
                Location::Above => upper - price_x2,
                Location::Inside => return Err(VoidError::InvalidObservation),
            };
            let penetration = (entered.clamp(0, upper - lower) * PPM / (upper - lower)) as u32;
            if penetration > z.visit_peak_ppm {
                let old = PenetrationBand::from_ppm(z.visit_peak_ppm).index();
                let new = PenetrationBand::from_ppm(penetration).index();
                self.metrics.penetration_distribution[old] -= 1;
                self.metrics.penetration_distribution[new] += 1;
                z.visit_peak_ppm = penetration;
                z.max_penetration_ppm = z.max_penetration_ppm.max(penetration);
            }
            if here == Location::Inside {
                z.last_touched_at = Some(now);
            } else {
                z.state = VoidState::Exited;
            }
        }
        z.previous = here;
        Ok(())
    }
}
