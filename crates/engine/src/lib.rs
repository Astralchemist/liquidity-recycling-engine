//! Orchestration: one synchronous owner wiring the shared research path, the environment
//! classifier, the inventory ledger, a transparent recycling policy, a configurable fill
//! simulator and the kill executor. Direct input and replay call the same `apply`.
//!
//! Fills are SIMULATED (Phase 9): either the Phase 7 strict trade-through rule or an L2 queue
//! model with a stated cancellation assumption, charged by an exact fee schedule. Markouts,
//! funding and an optional objective J(a) gate are measured on the same event path. None of
//! this is evidence of executable edge; Phase 10 replaces simulation with acknowledgements.
#![forbid(unsafe_code)]
pub mod fixtures;
use book::{BookError, BookState};
use common::{Side, Timestamp, VenueId};
use consolidator::{
    ConsolidationError, VenueConfig,
    normalization::{InstrumentMetadata, NormalizationError, Normalizer},
};
use execution::{
    fees::FeeSchedule,
    markout::{MarkoutConfig, MarkoutTracker},
    queue::{CancelModel, QueuedOrder},
};
use fixed_point::{ArithmeticError, Money, PriceTicks, QtyUnits, notional, realized_pnl};
use inventory::{
    Charges, CloseMode, InventoryConfig, InventoryError, InventoryEvent, InventoryEventKind as Cmd,
    InventoryRole, Ledger, Outcome, RevisitEvidence,
};
use liquidity::{LiquidityConfig, research::ResearchError};
use market_events::{MarketEvent, MarketEventType as K};
use orderflow::{
    FlowConfig, Metric, RemovalAttribution,
    research::{FlowResearchEngine, FlowResearchError},
};
use risk::{Denial, KillReason};
use toxicity::{
    Environment, EnvironmentConfig, EnvironmentError, EnvironmentTracker, Observation,
    ToxicityComponents,
};
use voids::{LiquidityVoid, Scope, VoidConfig, VoidState};
pub const VENUE_LOCAL: u8 = 1;
pub const MULTI_VENUE: u8 = 2;
pub const MARKET_WIDE: u8 = 4;
pub fn scope_bit(scope: Scope) -> u8 {
    match scope {
        Scope::VenueLocal => VENUE_LOCAL,
        Scope::MultiVenue => MULTI_VENUE,
        Scope::MarketWide => MARKET_WIDE,
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Scope bits (`VENUE_LOCAL | MULTI_VENUE | MARKET_WIDE`) whose revisits may acquire inventory.
    pub allowed_scopes: u8,
    /// Entry quanta per side, one tick apart starting at the touch.
    pub entry_levels: u32,
    /// A side is not quoted if its worst-case fills would take |net| above this.
    pub entry_max_abs_net: i64,
    /// Take-profit distance from entry for harvest closes.
    pub harvest_ticks: i64,
    /// Rebalance when |net - target| exceeds this many units.
    pub rebalance_threshold_units: i64,
    /// Also rebalance within the threshold once an excess-side child is at least this old.
    pub rebalance_age_ns: u64,
    /// Rebalance closes improve the touch by up to this many ticks, staying strictly inside the
    /// spread (still passive). Zero joins the touch.
    pub rebalance_improve_ticks: i64,
    /// Quote hysteresis: move a resting order only when the desired price moved this far.
    pub reprice_ticks: i64,
    pub evidence_ttl_ns: u64,
    /// `Environment::bit` mask that forces a `RegimeChange` halt while inventory is held.
    pub exit_on_environment: u8,
    /// Re-mark an unchanged venue touch after this long; must be below the ledger stale limit.
    pub mark_refresh_ns: u64,
    /// Issue a ledger `Tick` when no command has run for this long.
    pub assess_interval_ns: u64,
}
/// How a simulated resting order fills against the public print stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillModel {
    /// Phase 7 rule: a print strictly beyond the resting price fills it; size and queue ignored.
    StrictTradeThrough,
    /// L2 queue (`execution::queue`): join behind the displayed size, prints at our price
    /// consume the queue ahead first, unexplained decreases are attributed by the model.
    /// Requires every venue price scale to equal the grid's.
    Queue(CancelModel),
}
/// Charges per one-quantum fill: the proportional schedule PLUS fixed atoms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillConfig {
    pub model: FillModel,
    pub schedule: FeeSchedule,
    pub maker_rebate: Money,
    /// Retail maker fills usually PAY a fee (see docs/providers.md); both may be configured.
    pub maker_fee: Money,
    pub taker_fee: Money,
    pub emergency_slippage_ticks: i64,
}
/// Perpetual funding at a constant configured rate. Boundaries are the multiples of
/// `interval_ns` on the ENGINE clock (session-relative for live recordings, so their phase
/// against the exchange's UTC schedule is arbitrary). Each child held across a boundary is
/// charged `rate_ppm` of its notional at the venue mark midpoint (entry price when unmarked).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FundingConfig {
    /// Positive: longs pay shorts. Payments round up and receipts round down.
    pub rate_ppm: i64,
    /// Zero disables funding.
    pub interval_ns: u64,
}
/// Transparent entry utility (§24) for one quantum, in doubled money atoms:
///
/// `J = wR·R + wS·S + wB·B − wA·A − wI·I − wQ·Q` (weights in permille), where
/// - R = maker rebate minus maker fee at the candidate price;
/// - S = spread captured against the reference midpoint;
/// - B = for a fill that moves net toward target: the avoided taker exit (taker fee plus half
///   the venue spread);
/// - A = adverse selection: the measured mean adverse drift at `adverse_horizon` once
///   `adverse_min_samples` fills exist, else `adverse_prior_x2`;
/// - I = for a fill that moves net away from target: `inventory_risk_x2` per unit of
///   |net − target| after the fill;
/// - Q = `queue_cost_x2` per quantity unit displayed ahead (better prices plus our level).
///
/// Actions per entry slot are hold, place, cancel and replace; the arg-max is taken with
/// "no order" valued at `min_edge_x2`, ties keep the current state, and every action remains
/// subject to the ledger's hard limits. Only entries are gated; closes follow the central rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectiveConfig {
    pub w_rebate: u32,
    pub w_spread: u32,
    pub w_rebalance: u32,
    pub w_adverse: u32,
    pub w_inventory: u32,
    pub w_queue: u32,
    pub adverse_horizon: usize,
    pub adverse_min_samples: u64,
    pub adverse_prior_x2: i64,
    pub inventory_risk_x2: i64,
    pub queue_cost_x2: i64,
    pub min_edge_x2: i128,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub environment: EnvironmentConfig,
    pub policy: PolicyConfig,
    pub fills: FillConfig,
    pub funding: FundingConfig,
    pub markout: MarkoutConfig,
    pub objective: Option<ObjectiveConfig>,
}
/// Pending fills awaiting their markout horizons.
pub const MARKOUT_CAPACITY: usize = 1024;
/// Queue position of an order placed where depth is unknown: behind everything, until the first
/// depth observation clamps it to the displayed size.
const UNKNOWN_AHEAD: i64 = i64::MAX / 4;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Entry { level: u32 },
    Harvest,
    Rebalance,
}
/// A simulated resting order, keyed by its ledger reservation or child id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingOrder {
    pub id: u64,
    pub venue: VenueId,
    pub side: Side,
    pub price: PriceTicks,
    pub kind: OrderKind,
    /// Present exactly when the fill model is `Queue`.
    pub queue: Option<QueuedOrder>,
}
pub const DENIALS: usize = 13;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EngineMetrics {
    /// Canonical events, counting every member of an atomic batch.
    pub market_events: u64,
    /// Atomic native depth batches applied through `apply_depth_batch`.
    pub batches: u64,
    pub commands: u64,
    /// Indexed by `risk::Denial as usize`.
    pub denials: [u64; DENIALS],
    pub maker_fills: u64,
    pub taker_fills: u64,
    pub entries_placed: u64,
    pub entries_cancelled: u64,
    pub reprices: u64,
    pub harvests_placed: u64,
    pub rebalances_placed: u64,
    /// Steps with a revisited zone whose scope is not allowed to trade.
    pub disallowed_scope_steps: u64,
    /// Steps with an allowed revisited zone while the environment was not balanced-active.
    pub unqualified_revisit_steps: u64,
    pub kill_steps: u64,
    /// Kill steps that could not price an emergency exit (no touch and no ledger mark).
    pub unpriced_emergency_steps: u64,
    pub max_abs_divergence_x2: i128,
    /// Market events after which at least one venue midpoint differed from the global midpoint.
    pub divergent_events: u64,
    /// Fill notional and charges split by liquidity role (the ledger only keeps totals).
    pub maker_notional: i128,
    pub taker_notional: i128,
    pub maker_fees: i128,
    pub taker_fees: i128,
    pub rebates: i128,
    /// Queue-model completions by a print at our price versus strictly through it.
    pub queue_fills_at_price: u64,
    pub queue_fills_through: u64,
    /// Prints that filled part of a resting order without completing it.
    pub partial_prints: u64,
    /// Simulated partially filled quantity discarded by cancels (never booked; see contracts).
    pub abandoned_partial_qty: i64,
    pub funding_events: u64,
    pub funding_cost: i128,
    pub objective_evaluations: u64,
    /// Placements declined because J was below the minimum edge.
    pub objective_holds: u64,
    /// Resting entries kept although the rule-based price moved (queue value won).
    pub objective_keeps: u64,
    /// Resting entries cancelled because J fell below the minimum edge.
    pub objective_cancels: u64,
    /// Evaluations impossible without a reference midpoint (treated as hold or cancel).
    pub objective_unpriced: u64,
    /// Quotes not placed because a print since the venue's last depth update traded through
    /// the desired price (the displayed touch is known to be stale).
    pub stale_touch_skips: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    InvalidConfig,
    Invariant,
    Research(FlowResearchError),
    Environment(EnvironmentError),
    Ledger(InventoryError),
}
impl From<ConsolidationError> for EngineError {
    fn from(e: ConsolidationError) -> Self {
        Self::Research(FlowResearchError::Market(e))
    }
}
impl From<NormalizationError> for EngineError {
    fn from(e: NormalizationError) -> Self {
        Self::Research(FlowResearchError::Market(e.into()))
    }
}
impl From<BookError> for EngineError {
    fn from(e: BookError) -> Self {
        Self::Research(FlowResearchError::Book(e))
    }
}
fn book_error(e: FlowResearchError) -> Option<BookError> {
    match e {
        FlowResearchError::Book(b)
        | FlowResearchError::Market(ConsolidationError::Book { error: b, .. })
        | FlowResearchError::Research(ResearchError::Market(ConsolidationError::Book {
            error: b,
            ..
        })) => Some(b),
        _ => None,
    }
}
/// Market data faults map to the closest kill reason; anything unclassified is a corrupt book.
pub fn research_kill_reason(e: FlowResearchError) -> KillReason {
    match book_error(e) {
        Some(BookError::SequenceGap) => KillReason::SequenceGap,
        Some(BookError::ClockRegression) => KillReason::ClockAnomaly,
        Some(BookError::Stale) => KillReason::StaleData,
        Some(BookError::Disconnected) => KillReason::Disconnect,
        _ => match e {
            FlowResearchError::Market(ConsolidationError::ClockRegression)
            | FlowResearchError::Research(ResearchError::Market(
                ConsolidationError::ClockRegression,
            )) => KillReason::ClockAnomaly,
            _ => KillReason::CorruptBook,
        },
    }
}
fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine<
    const V: usize,
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
    const W: usize,
    const L: usize,
    const E: usize,
> {
    /// Boxed so moving the engine copies tens of kilobytes, not the whole research state.
    research: Box<FlowResearchEngine<V, N, G, P, B, Z, W>>,
    environment: EnvironmentTracker<W>,
    ledger: Ledger<V, L, E>,
    orders: [Option<RestingOrder>; L],
    venues: [VenueId; V],
    normalizers: [Normalizer; V],
    config: EngineConfig,
    attribution: RemovalAttribution,
    marks: [Option<(PriceTicks, PriceTicks, Timestamp)>; V],
    research_fault: Option<FlowResearchError>,
    environment_fault: Option<EnvironmentError>,
    metrics: EngineMetrics,
    markouts: Box<MarkoutTracker<MARKOUT_CAPACITY>>,
    funding_epoch: Option<u64>,
    /// Per venue since its last depth change: lowest sell-aggressor and highest buy-aggressor
    /// print (grid ticks). Depth arrives in snapshots while prints stream, so a price traded
    /// through after the last depth update is no longer available to a passive quote.
    swept: [[Option<i64>; 2]; V],
    now: Timestamp,
}
impl<
    const V: usize,
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
    const W: usize,
    const L: usize,
    const E: usize,
> Engine<V, N, G, P, B, Z, W, L, E>
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: InstrumentMetadata,
        venues: [VenueConfig; V],
        liquidity: LiquidityConfig<B>,
        voids: VoidConfig,
        flow: FlowConfig,
        inventory: InventoryConfig<V>,
        config: EngineConfig,
    ) -> Result<Self, EngineError> {
        let p = config.policy;
        let f = config.fills;
        // Zone venue masks index venue configuration order, so the ledger must use the same order.
        if venues.map(|c| c.venue) != inventory.venues
            || !(1..=8).contains(&p.entry_levels)
            || p.entry_max_abs_net < 1
            || p.entry_max_abs_net > inventory.limits.max_net_units
            || p.harvest_ticks < 1
            || p.rebalance_threshold_units < 0
            || p.rebalance_improve_ticks < 0
            || p.rebalance_age_ns == 0
            || p.reprice_ticks < 1
            || p.allowed_scopes == 0
            || p.allowed_scopes > 7
            || p.exit_on_environment > 31
            || p.evidence_ttl_ns == 0
            || p.assess_interval_ns == 0
            || p.mark_refresh_ns == 0
            || p.mark_refresh_ns >= inventory.limits.mark_stale_ns
            || f.maker_rebate.0 < 0
            || f.maker_fee.0 < 0
            || f.taker_fee.0 < 0
            || f.emergency_slippage_ticks < 0
            || !f.schedule.validate()
            || config.funding.rate_ppm.abs() > 100_000
            || MarkoutConfig::new(config.markout.horizons()).is_none_or(|m| m != config.markout)
            || config.objective.is_some_and(|o| {
                [
                    o.w_rebate,
                    o.w_spread,
                    o.w_rebalance,
                    o.w_adverse,
                    o.w_inventory,
                    o.w_queue,
                ]
                .iter()
                .any(|&w| w > 100_000)
                    || o.adverse_horizon >= config.markout.count
                    || o.adverse_prior_x2 < 0
                    || o.inventory_risk_x2 < 0
                    || o.queue_cost_x2 < 0
            })
        {
            return Err(EngineError::InvalidConfig);
        }
        let research = Box::new(
            FlowResearchEngine::new(grid, venues, liquidity, voids, flow)
                .map_err(EngineError::Research)?,
        );
        let normalizers =
            venues.map(|c| Normalizer::new(c.metadata, grid).expect("validated by research"));
        // Queue and objective prices are grid prices looked up in native books, so the venue
        // price map must be the identity.
        if (matches!(f.model, FillModel::Queue(_)) || config.objective.is_some())
            && normalizers
                .iter()
                .any(|n| n.price(PriceTicks(1)) != Ok(PriceTicks(1)))
        {
            return Err(EngineError::InvalidConfig);
        }
        Ok(Self {
            research,
            environment: EnvironmentTracker::new(config.environment)
                .map_err(EngineError::Environment)?,
            ledger: Ledger::new(inventory).map_err(EngineError::Ledger)?,
            orders: [None; L],
            venues: venues.map(|c| c.venue),
            normalizers,
            config,
            attribution: flow.removal_attribution,
            marks: [None; V],
            research_fault: None,
            environment_fault: None,
            metrics: EngineMetrics::default(),
            markouts: Box::new(MarkoutTracker::new(config.markout)),
            funding_epoch: None,
            swept: [[None; 2]; V],
            now: Timestamp(0),
        })
    }
    pub fn research(&self) -> &FlowResearchEngine<V, N, G, P, B, Z, W> {
        &self.research
    }
    pub fn environment(&self) -> &EnvironmentTracker<W> {
        &self.environment
    }
    pub fn ledger(&self) -> &Ledger<V, L, E> {
        &self.ledger
    }
    pub fn orders(&self) -> &[Option<RestingOrder>; L] {
        &self.orders
    }
    pub fn metrics(&self) -> EngineMetrics {
        self.metrics
    }
    pub fn config(&self) -> EngineConfig {
        self.config
    }
    pub fn research_fault(&self) -> Option<FlowResearchError> {
        self.research_fault
    }
    pub fn environment_fault(&self) -> Option<EnvironmentError> {
        self.environment_fault
    }
    /// Venue-to-grid conversions, in venue configuration order.
    pub fn normalizers(&self) -> &[Normalizer; V] {
        &self.normalizers
    }
    /// Markouts of every simulated maker fill against the reference midpoint.
    pub fn markouts(&self) -> &MarkoutTracker<MARKOUT_CAPACITY> {
        &self.markouts
    }
    /// Net maker yield (§25) in ppm of maker notional: harvest account (realized, rebates, fees,
    /// slippage, funding) plus open marked P&L. `None` before any maker fill.
    pub fn net_maker_yield_ppm(&self) -> Result<Option<i128>, EngineError> {
        if self.metrics.maker_notional <= 0 {
            return Ok(None);
        }
        let net = self
            .ledger
            .accounts()
            .harvest()
            .and_then(|h| Ok(h.checked_add(self.ledger.unrealized())?))
            .map_err(EngineError::Ledger)?;
        Ok(Some(net.0 * 1_000_000 / self.metrics.maker_notional))
    }
    /// Charges of one quantum filled at `price`: the proportional schedule plus fixed atoms.
    fn charges(&self, price: PriceTicks, maker: bool) -> Result<Charges, EngineError> {
        let f = self.config.fills;
        let cost = f
            .schedule
            .cost(price, self.quantity(), maker)
            .ok_or(EngineError::Invariant)?;
        let (rebate, fee) = if maker {
            (f.maker_rebate, f.maker_fee)
        } else {
            (Money(0), f.taker_fee)
        };
        let arithmetic = |e: ArithmeticError| EngineError::Ledger(e.into());
        Ok(Charges {
            rebate: rebate.checked_add(cost.rebate).map_err(arithmetic)?,
            fee: fee.checked_add(cost.fee).map_err(arithmetic)?,
            slippage: Money(0),
        })
    }
    fn quantity(&self) -> QtyUnits {
        self.ledger.config().quantum.unit_quantity()
    }
    /// The reference midpoint (doubled ticks) while research is healthy.
    pub fn reference_mid_x2(&self) -> Option<i128> {
        if self.research_fault.is_some() {
            return None;
        }
        self.research.research().reference().ok().and_then(|r| r.0)
    }
    /// Displayed grid quantity at `price` on `side` of venue index `v` (everyone else's size;
    /// our simulated orders are not in the book). `None` while that book is not live.
    pub fn displayed(
        &self,
        v: usize,
        side: Side,
        price: PriceTicks,
    ) -> Result<Option<i64>, EngineError> {
        let book = self
            .research
            .research()
            .market()
            .venue_book(self.venues[v])?;
        if book.state() != BookState::Live {
            return Ok(None);
        }
        let qty = book.level(side, price)?.map_or(QtyUnits(0), |l| l.qty);
        Ok(Some(self.normalizers[v].quantity(qty)?.0))
    }
    /// Displayed grid quantity at prices strictly better than `price` on `side` of venue `v`.
    fn displayed_better(
        &self,
        v: usize,
        side: Side,
        price: PriceTicks,
    ) -> Result<i64, EngineError> {
        let book = self
            .research
            .research()
            .market()
            .venue_book(self.venues[v])?;
        if book.state() != BookState::Live {
            return Ok(0);
        }
        let mut total = 0_i64;
        for l in book.levels(side)? {
            let level = self.normalizers[v].price(l.price)?;
            let better = match side {
                Side::Buy => level.0 > price.0,
                Side::Sell => level.0 < price.0,
            };
            if !better {
                break;
            }
            total = total.saturating_add(self.normalizers[v].quantity(l.qty)?.0);
        }
        Ok(total)
    }
    fn healthy(&self) -> bool {
        self.research_fault.is_none() && self.environment_fault.is_none()
    }
    /// Toxicity components over the current windows, computed on demand (not per event).
    pub fn toxicity(&self) -> Result<ToxicityComponents, EngineError> {
        let (mut added, mut removed, mut cancelled) = (0, 0, 0);
        for venue in self.venues {
            if let Some(s) = self
                .research
                .snapshot(venue)
                .map_err(EngineError::Research)?
            {
                added += s.time_totals.get(Metric::AddedQty);
                removed += s.time_totals.get(Metric::RemovedQty);
                cancelled += s.time_totals.get(Metric::CancelQty);
            }
        }
        let (_, spread) = self
            .research
            .research()
            .reference()
            .map_err(|e| EngineError::Research(FlowResearchError::Research(e)))?;
        ToxicityComponents::new(
            self.environment.totals(),
            spread,
            added,
            removed,
            (self.attribution == RemovalAttribution::AssumeCancellation).then_some(cancelled),
        )
        .map_err(EngineError::Environment)
    }
    /// Fresh, live, normalized touch for venue index `v`; `None` while unusable.
    pub fn touch(&self, v: usize) -> Result<Option<(PriceTicks, PriceTicks)>, EngineError> {
        if self.research_fault.is_some() {
            return Ok(None);
        }
        let market = self.research.research().market();
        let venue = self.venues[v];
        if !market.included(venue)? || !market.fresh_at(venue, self.now)? {
            return Ok(None);
        }
        let book = market.venue_book(venue)?;
        if book.state() != BookState::Live {
            return Ok(None);
        }
        let (Some(bid), Some(ask)) = (book.best(Side::Buy)?, book.best(Side::Sell)?) else {
            return Ok(None);
        };
        let n = self.normalizers[v];
        Ok(Some((n.price(bid.price)?, n.price(ask.price)?)))
    }
    /// Whether a live caller may apply input for `venue` stamped `receive_ts`. A snapshot is
    /// always accepted. Anything else needs a book that is live AND stays live once `apply`
    /// advances the clock to `receive_ts`; otherwise it would fault the whole engine and must be
    /// dropped and the venue resynchronized. After a research fault market input is ignored, so
    /// it is accepted unchanged.
    pub fn accepts(&self, venue: VenueId, receive_ts: Timestamp, snapshot: bool) -> bool {
        snapshot
            || self.research_fault.is_some()
            || self
                .research
                .research()
                .market()
                .live_at(venue, receive_ts)
                .unwrap_or(false)
    }
    /// Index of `venue` in configuration order.
    pub fn venue_position(&self, venue: VenueId) -> Option<usize> {
        self.venues.iter().position(|&v| v == venue)
    }
    /// Whether a passive `side` quote at `price` on `venue` is known stale: a print since that
    /// venue's last depth update traded through it.
    pub fn quote_is_stale(&self, venue: VenueId, side: Side, price: PriceTicks) -> bool {
        self.venue_position(venue)
            .is_some_and(|v| self.swept_through(v, side, price))
    }
    fn swept_through(&self, v: usize, side: Side, price: PriceTicks) -> bool {
        match (side, self.swept[v]) {
            (Side::Buy, [Some(low), _]) => low < price.0,
            (Side::Sell, [_, Some(high)]) => high > price.0,
            _ => false,
        }
    }
    fn venue_index(&self, venue: VenueId) -> Result<usize, EngineError> {
        self.venues
            .iter()
            .position(|&v| v == venue)
            .ok_or(EngineError::Invariant)
    }
    fn zone(&self, id: u64) -> Option<LiquidityVoid> {
        self.research
            .research()
            .voids()
            .zones()
            .iter()
            .flatten()
            .find(|z| z.id == id)
            .copied()
    }
    /// Accepted commands go to the sink; denials are counted and returned, never journaled.
    fn command(
        &mut self,
        kind: Cmd,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<Result<Outcome, Denial>, EngineError> {
        let event = InventoryEvent {
            sequence: self.ledger.last_sequence() + 1,
            timestamp: self.now,
            kind,
        };
        match self.ledger.apply(event) {
            Ok(outcome) => {
                sink(&event);
                self.metrics.commands += 1;
                Ok(Ok(outcome))
            }
            Err(InventoryError::Denied(d)) => {
                self.metrics.denials[d as usize] += 1;
                Ok(Err(d))
            }
            Err(e) => Err(EngineError::Ledger(e)),
        }
    }
    /// Fills, cancels and halts must be accepted; a denial here is an engine invariant failure.
    fn required(
        &mut self,
        kind: Cmd,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<Outcome, EngineError> {
        self.command(kind, sink)?
            .map_err(|d| EngineError::Ledger(InventoryError::Denied(d)))
    }
    /// Stores a newly accepted order, joining the back of the displayed queue under `Queue`.
    fn store(
        &mut self,
        id: u64,
        venue: VenueId,
        side: Side,
        price: PriceTicks,
        kind: OrderKind,
    ) -> Result<(), EngineError> {
        let slot = self
            .orders
            .iter()
            .position(Option::is_none)
            .ok_or(EngineError::Invariant)?;
        let queue = match self.config.fills.model {
            FillModel::StrictTradeThrough => None,
            FillModel::Queue(_) => {
                let shown = self.displayed(self.venue_index(venue)?, side, price)?;
                Some(QueuedOrder::place(
                    side,
                    price.0,
                    self.quantity().0,
                    shown.unwrap_or(UNKNOWN_AHEAD),
                ))
            }
        };
        self.orders[slot] = Some(RestingOrder {
            id,
            venue,
            side,
            price,
            kind,
            queue,
        });
        Ok(())
    }
    pub fn apply(
        &mut self,
        event: &MarketEvent,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        self.metrics.market_events += 1;
        self.now = self.now.max(event.receive_ts);
        let commands = self.metrics.commands;
        if self.research_fault.is_none() {
            let depth = event.event_type != K::Trade;
            if let Some(v) = self.venue_position(event.venue).filter(|_| depth) {
                self.swept[v] = [None; 2];
            }
            let before = self.queue_depths(event.venue, depth)?;
            if let Err(e) = self.research.apply(event) {
                self.latch_research_fault(e, sink)?;
            } else if let Some(before) = before {
                self.update_queues(event.venue, &before)?;
            }
        }
        let print = (event.event_type == K::Trade).then_some((
            event.side,
            event.price_ticks,
            event.qty_units,
        ));
        self.finish(event.venue, print, commands, sink)
    }
    /// One atomic native depth message (see `Consolidator::apply_depth_batch`). The engine
    /// observes, marks, assesses and runs policy ONCE for the whole batch; batches carry no
    /// prints, so no fills occur here.
    pub fn apply_depth_batch(
        &mut self,
        venue: VenueId,
        events: &[MarketEvent],
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let first = events.first().ok_or(EngineError::Invariant)?;
        self.metrics.market_events += events.len() as u64;
        self.metrics.batches += 1;
        self.now = self.now.max(first.receive_ts);
        let commands = self.metrics.commands;
        if self.research_fault.is_none() {
            if let Some(v) = self.venue_position(venue) {
                self.swept[v] = [None; 2];
            }
            let before = self.queue_depths(venue, true)?;
            if let Err(e) = self.research.apply_depth_batch(venue, events) {
                self.latch_research_fault(e, sink)?;
            } else if let Some(before) = before {
                self.update_queues(venue, &before)?;
            }
        }
        self.finish(venue, None, commands, sink)
    }
    /// Displayed size at every queued order's price on `venue`, captured around a depth change;
    /// `None` when nothing needs tracking (no depth change, strict model, or no order there).
    #[allow(clippy::type_complexity)]
    fn queue_depths(
        &self,
        venue: VenueId,
        depth: bool,
    ) -> Result<Option<[Option<i64>; L]>, EngineError> {
        if !depth
            || !self
                .orders
                .iter()
                .any(|o| o.is_some_and(|o| o.venue == venue && o.queue.is_some()))
        {
            return Ok(None);
        }
        let v = self.venue_index(venue)?;
        let mut sizes = [None; L];
        for (slot, size) in sizes.iter_mut().enumerate() {
            if let Some(o) = self.orders[slot].filter(|o| o.venue == venue && o.queue.is_some()) {
                *size = self.displayed(v, o.side, o.price)?;
            }
        }
        Ok(Some(sizes))
    }
    /// Feeds displayed-size changes to the queue model. A level first observed after an
    /// unobservable book only clamps the queue to what is displayed.
    fn update_queues(
        &mut self,
        venue: VenueId,
        before: &[Option<i64>; L],
    ) -> Result<(), EngineError> {
        let FillModel::Queue(model) = self.config.fills.model else {
            return Ok(());
        };
        let v = self.venue_index(venue)?;
        for (slot, old) in before.iter().enumerate() {
            let Some(mut o) = self.orders[slot].filter(|o| o.venue == venue) else {
                continue;
            };
            let Some(mut q) = o.queue else { continue };
            let Some(new) = self.displayed(v, o.side, o.price)? else {
                continue;
            };
            q.on_depth(old.unwrap_or(new), new, model);
            o.queue = Some(q);
            self.orders[slot] = Some(o);
        }
        Ok(())
    }
    fn latch_research_fault(
        &mut self,
        e: FlowResearchError,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        self.research_fault = Some(e);
        self.required(
            Cmd::Halt {
                reason: research_kill_reason(e),
            },
            sink,
        )?;
        Ok(())
    }
    fn finish(
        &mut self,
        venue: VenueId,
        print: Option<(Side, PriceTicks, fixed_point::QtyUnits)>,
        commands: u64,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        if self.healthy() {
            let v = self.venue_index(venue)?;
            self.observe(v, print, sink)?;
        }
        if self.markouts.pending() > 0 {
            let mid = self.reference_mid_x2();
            self.markouts.observe(self.now.0, mid);
        }
        self.funding(sink)?;
        if self.metrics.commands == commands
            && self.now.0 - self.ledger.now().0 >= self.config.policy.assess_interval_ns
        {
            self.required(Cmd::Tick, sink)?;
        }
        if self.ledger.halt_reason().is_none() && self.healthy() {
            self.policy(sink)?;
        }
        if self.ledger.halt_reason().is_some() {
            self.kill(sink)?;
        }
        Ok(())
    }
    fn observe(
        &mut self,
        v: usize,
        print: Option<(Side, PriceTicks, fixed_point::QtyUnits)>,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let (midpoint_x2, spread_ticks) = self
            .research
            .research()
            .reference()
            .map_err(|e| EngineError::Research(FlowResearchError::Research(e)))?;
        let market = self.research.research().market();
        let top = |side| -> Result<i128, ConsolidationError> {
            Ok(market
                .levels(side)?
                .first()
                .map_or(0, |l| l.weighted_quantity_microunits / 1_000_000))
        };
        let common_print = match print {
            Some((side, _, qty)) => Some((side, i128::from(self.normalizers[v].quantity(qty)?.0))),
            None => None,
        };
        let observation = Observation {
            midpoint_x2,
            spread_ticks,
            top_qty: top(Side::Buy)? + top(Side::Sell)?,
            print: common_print,
        };
        let mut divergent = false;
        for venue in self.venues {
            if let Some(d) = market.divergence(venue)? {
                divergent |= d.midpoint_difference_x2 != 0;
                self.metrics.max_abs_divergence_x2 = self
                    .metrics
                    .max_abs_divergence_x2
                    .max(d.midpoint_difference_x2.abs());
            }
        }
        self.metrics.divergent_events += u64::from(divergent);
        if let Err(e) = self.environment.update(self.now, observation) {
            self.environment_fault = Some(e);
            self.required(
                Cmd::Halt {
                    reason: KillReason::UnhandledState,
                },
                sink,
            )?;
            return Ok(());
        }
        self.mark(v, sink)?;
        if let Some((side, price, qty)) = print {
            let price = self.normalizers[v].price(price)?;
            let qty = self.normalizers[v].quantity(qty)?.0;
            let swept = &mut self.swept[v];
            match side {
                Side::Sell => swept[0] = Some(swept[0].map_or(price.0, |p| p.min(price.0))),
                Side::Buy => swept[1] = Some(swept[1].map_or(price.0, |p| p.max(price.0))),
            }
            self.fill(v, price, qty, side, sink)?;
        }
        Ok(())
    }
    /// Charges every child held across each crossed funding boundary (one command per child).
    fn funding(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        let f = self.config.funding;
        if f.interval_ns == 0 {
            return Ok(());
        }
        let epoch = self.now.0 / f.interval_ns;
        let crossed = match self.funding_epoch {
            Some(previous) if epoch > previous => epoch - previous,
            Some(_) => return Ok(()),
            None => {
                self.funding_epoch = Some(epoch);
                return Ok(());
            }
        };
        self.funding_epoch = Some(epoch);
        if f.rate_ppm == 0 {
            return Ok(());
        }
        let q = i128::from(self.quantity().0);
        for i in 0..L {
            let Some(lot) = self.ledger.lots()[i] else {
                continue;
            };
            let price_x2 = match self.ledger.mark(lot.venue).map_err(EngineError::Ledger)? {
                Some(m) => i128::from(m.bid.0) + i128::from(m.ask.0),
                None => 2 * i128::from(lot.entry_price.0),
            };
            let sign = if lot.side == Side::Buy { 1 } else { -1 };
            let numerator = sign * price_x2 * q * i128::from(f.rate_ppm) * i128::from(crossed);
            let d = 2 * 1_000_000;
            // Ceiling: a payment rounds up, a receipt (negative cost) rounds toward zero.
            let cost = numerator.div_euclid(d) + i128::from(numerator.rem_euclid(d) != 0);
            if cost == 0 {
                continue;
            }
            self.required(
                Cmd::Funding {
                    id: lot.id,
                    cost: Money(cost),
                },
                sink,
            )?;
            self.metrics.funding_events += 1;
            self.metrics.funding_cost += cost;
        }
        Ok(())
    }
    fn mark(
        &mut self,
        v: usize,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let Some((bid, ask)) = self.touch(v)? else {
            return Ok(());
        };
        let due = self.marks[v].is_none_or(|(b, a, t)| {
            b != bid || a != ask || self.now.0 - t.0 >= self.config.policy.mark_refresh_ns
        });
        if due {
            self.required(
                Cmd::Mark {
                    venue: self.venues[v],
                    bid,
                    ask,
                },
                sink,
            )?;
            self.marks[v] = Some((bid, ask, self.now));
        }
        Ok(())
    }
    /// A print on venue `v` fills resting orders by the configured model, at the ORDER price as
    /// maker. A child fills only completely; queue progress short of that is tracked, not booked.
    fn fill(
        &mut self,
        v: usize,
        price: PriceTicks,
        qty: i64,
        aggressor: Side,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        for slot in 0..L {
            let Some(mut o) = self.orders[slot] else {
                continue;
            };
            if o.venue != self.venues[v] {
                continue;
            }
            let through = match (aggressor, o.side) {
                (Side::Sell, Side::Buy) => price < o.price,
                (Side::Buy, Side::Sell) => price > o.price,
                _ => false,
            };
            let complete = match o.queue {
                None => through,
                Some(mut q) => {
                    let got = q.on_trade(aggressor, price.0, qty);
                    o.queue = Some(q);
                    self.orders[slot] = Some(o);
                    if q.complete() && got > 0 {
                        if through {
                            self.metrics.queue_fills_through += 1;
                        } else {
                            self.metrics.queue_fills_at_price += 1;
                        }
                    } else if got > 0 {
                        self.metrics.partial_prints += 1;
                    }
                    q.complete() && got > 0
                }
            };
            if !complete {
                continue;
            }
            let charges = self.charges(o.price, true)?;
            self.book_maker(o.side, o.price, charges);
            self.orders[slot] = None;
            let kind = match o.kind {
                OrderKind::Entry { .. } => Cmd::FillOpen {
                    id: o.id,
                    price: o.price,
                    maker: true,
                    charges,
                },
                OrderKind::Harvest | OrderKind::Rebalance => Cmd::FillClose {
                    id: o.id,
                    price: o.price,
                    maker: true,
                    charges,
                },
            };
            self.required(kind, sink)?;
            self.metrics.maker_fills += 1;
        }
        Ok(())
    }
    fn book_maker(&mut self, side: Side, price: PriceTicks, charges: Charges) {
        let q = self.quantity();
        self.metrics.maker_notional += notional(price, q).0;
        self.metrics.maker_fees += charges.fee.0;
        self.metrics.rebates += charges.rebate.0;
        let mid = self.reference_mid_x2();
        self.markouts.record(self.now.0, side, price.0, q.0, mid);
    }
    fn policy(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        let environment = self.environment.state();
        let gross = self.ledger.exposure().gross();
        if gross > 0 && self.config.policy.exit_on_environment & environment.bit() != 0 {
            self.required(
                Cmd::Halt {
                    reason: KillReason::RegimeChange,
                },
                sink,
            )?;
            return Ok(());
        }
        if let Some(episode) = self.ledger.active_episode() {
            if gross > 0
                && self
                    .zone(episode.void_id)
                    .is_some_and(|z| z.state == VoidState::Invalidated)
            {
                self.required(
                    Cmd::Halt {
                        reason: KillReason::StructureInvalidated,
                    },
                    sink,
                )?;
                return Ok(());
            }
        }
        self.entries(environment, sink)?;
        self.rebalance(sink)?;
        self.harvest(sink)
    }
    fn cancel_open(
        &mut self,
        slot: usize,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let o = self.orders[slot].ok_or(EngineError::Invariant)?;
        self.orders[slot] = None;
        self.metrics.abandoned_partial_qty += o.queue.map_or(0, |q| q.filled);
        self.required(Cmd::CancelOpen { id: o.id }, sink)?;
        self.metrics.entries_cancelled += 1;
        Ok(())
    }
    fn cancel_close(
        &mut self,
        slot: usize,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let o = self.orders[slot].ok_or(EngineError::Invariant)?;
        self.orders[slot] = None;
        self.metrics.abandoned_partial_qty += o.queue.map_or(0, |q| q.filled);
        self.required(Cmd::CancelClose { id: o.id }, sink)?;
        Ok(())
    }
    fn cancel_entries(
        &mut self,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        for slot in 0..L {
            if matches!(
                self.orders[slot],
                Some(RestingOrder {
                    kind: OrderKind::Entry { .. },
                    ..
                })
            ) {
                self.cancel_open(slot, sink)?;
            }
        }
        Ok(())
    }
    /// Two-sided entry quotes on the lowest-index venue of a revisited, allowed zone, only
    /// while the environment is balanced-active. Everything else cancels resting entries.
    fn entries(
        &mut self,
        environment: Environment,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        let p = self.config.policy;
        let mut target: Option<LiquidityVoid> = None;
        let mut disallowed = false;
        for z in self.research.research().voids().zones().iter().flatten() {
            if z.state != VoidState::Revisited {
                continue;
            }
            if scope_bit(z.scope) & p.allowed_scopes == 0 {
                disallowed = true;
            } else if target.is_none_or(|t| z.id < t.id) {
                target = Some(*z);
            }
        }
        if disallowed {
            self.metrics.disallowed_scope_steps += 1;
        }
        if target.is_some() && environment != Environment::BalancedActive {
            self.metrics.unqualified_revisit_steps += 1;
        }
        let Some(zone) = target.filter(|_| environment == Environment::BalancedActive) else {
            return self.cancel_entries(sink);
        };
        let v = zone.venue_mask.trailing_zeros() as usize;
        let Some((bid, ask)) = self.touch(v)? else {
            return self.cancel_entries(sink);
        };
        for side in [Side::Buy, Side::Sell] {
            for level in 0..p.entry_levels {
                let desired = PriceTicks(match side {
                    Side::Buy => bid.0 - i64::from(level),
                    Side::Sell => ask.0 + i64::from(level),
                });
                let kind = OrderKind::Entry { level };
                // A stale touch neither places nor moves anything on this side and level.
                if self.swept_through(v, side, desired) {
                    self.metrics.stale_touch_skips += 1;
                    continue;
                }
                // Whether J(a) already approved placing at `desired` (a chosen replace).
                let mut approved = false;
                if let Some(slot) = self
                    .orders
                    .iter()
                    .position(|o| o.is_some_and(|o| o.kind == kind && o.side == side))
                {
                    let o = self.orders[slot].ok_or(EngineError::Invariant)?;
                    let here = o.venue == self.venues[v];
                    let moved = (o.price.0 - desired.0).abs() >= p.reprice_ticks;
                    if let Some(objective) = self.config.objective.filter(|_| here) {
                        // Arg-max over keep, cancel (valued at the minimum edge) and replace.
                        self.metrics.objective_evaluations += 1;
                        let edge = objective.min_edge_x2.saturating_mul(1000);
                        let keep = self.utility(v, side, o.price, Some(o))?;
                        let new = if moved && desired.0 > 0 {
                            self.utility(v, side, desired, None)?
                        } else {
                            None
                        };
                        if keep.is_some_and(|k| k >= edge && new.is_none_or(|n| k >= n)) {
                            self.metrics.objective_keeps += u64::from(moved);
                            continue;
                        }
                        if new.is_none_or(|n| n < edge) {
                            self.metrics.objective_unpriced += u64::from(keep.is_none());
                            self.cancel_open(slot, sink)?;
                            self.metrics.objective_cancels += 1;
                            continue;
                        }
                        approved = true;
                    } else if here && !moved {
                        continue;
                    }
                    self.cancel_open(slot, sink)?;
                    self.metrics.reprices += 1;
                }
                let x = self.ledger.exposure();
                let worst = match side {
                    Side::Buy => x.net() + x.pending_buys + x.closing_buys + 1,
                    Side::Sell => x.net() - x.pending_sells - x.closing_sells - 1,
                };
                if desired.0 <= 0 || worst.abs() > p.entry_max_abs_net {
                    continue;
                }
                if let Some(objective) = self.config.objective.filter(|_| !approved) {
                    self.metrics.objective_evaluations += 1;
                    match self.utility(v, side, desired, None)? {
                        Some(j) if j >= objective.min_edge_x2.saturating_mul(1000) => {}
                        Some(_) => {
                            self.metrics.objective_holds += 1;
                            continue;
                        }
                        None => {
                            self.metrics.objective_unpriced += 1;
                            continue;
                        }
                    }
                }
                let evidence = RevisitEvidence {
                    void_id: zone.id,
                    revisited_at: zone.last_touched_at.unwrap_or(self.now).min(self.now),
                    valid_until: Timestamp(self.now.0.saturating_add(p.evidence_ttl_ns)),
                    qualified: true,
                };
                let result = self.command(
                    Cmd::ReserveOpen {
                        venue: self.venues[v],
                        side,
                        role: InventoryRole::Harvest,
                        evidence,
                    },
                    sink,
                )?;
                if let Ok(outcome) = result {
                    let Outcome::Reserved(id) = outcome else {
                        return Err(EngineError::Invariant);
                    };
                    self.store(id, self.venues[v], side, desired, kind)?;
                    self.metrics.entries_placed += 1;
                }
            }
        }
        Ok(())
    }
    /// Diagnostic J(a) of a NEW entry quantum (see `utility`), in permille of doubled atoms.
    pub fn entry_utility(
        &self,
        venue: VenueId,
        side: Side,
        price: PriceTicks,
    ) -> Result<Option<i128>, EngineError> {
        self.utility(self.venue_index(venue)?, side, price, None)
    }
    /// Diagnostic J(a) of KEEPING a resting order, valued at its tracked queue position.
    pub fn resting_utility(&self, order: &RestingOrder) -> Result<Option<i128>, EngineError> {
        self.utility(
            self.venue_index(order.venue)?,
            order.side,
            order.price,
            Some(*order),
        )
    }
    /// J(a) of one entry quantum on `side` at `price` on venue `v`, in permille of doubled money
    /// atoms (see `ObjectiveConfig`). `resting` supplies the tracked queue position of an
    /// existing order; otherwise the order would join behind the displayed size. `None` without
    /// an objective, a reference midpoint or a live touch.
    fn utility(
        &self,
        v: usize,
        side: Side,
        price: PriceTicks,
        resting: Option<RestingOrder>,
    ) -> Result<Option<i128>, EngineError> {
        let (Some(o), Some(mid), Some((bid, ask))) = (
            self.config.objective,
            self.reference_mid_x2(),
            self.touch(v)?,
        ) else {
            return Ok(None);
        };
        let overflow =
            || EngineError::Ledger(InventoryError::Arithmetic(ArithmeticError::Overflow));
        let mul = |a: i128, b: i128| a.checked_mul(b).ok_or_else(overflow);
        let q = i128::from(self.quantity().0);
        let p2 = 2 * i128::from(price.0);
        let maker = self.charges(price, true)?;
        let rebate = 2 * (maker.rebate.0 - maker.fee.0);
        let spread = mul(
            match side {
                Side::Buy => mid - p2,
                Side::Sell => p2 - mid,
            },
            q,
        )?;
        let target = self.ledger.config().target.0;
        let net = self.ledger.exposure().net();
        let after = net + if side == Side::Buy { 1 } else { -1 };
        let (d0, d1) = ((net - target).abs(), (after - target).abs());
        let rebalance = if d1 < d0 {
            2 * self.charges(price, false)?.fee.0 + mul(i128::from(ask.0 - bid.0), q)?
        } else {
            0
        };
        let inventory = if d1 > d0 {
            mul(mul(i128::from(o.inventory_risk_x2), i128::from(d1))?, q)?
        } else {
            0
        };
        let adverse = mul(
            self.markouts.stats()[o.adverse_horizon]
                .adverse_x2_per_unit(o.adverse_min_samples)
                .unwrap_or(i128::from(o.adverse_prior_x2)),
            q,
        )?;
        let at_level = match resting.and_then(|r| r.queue) {
            Some(queued) => queued.ahead,
            None => self.displayed(v, side, price)?.unwrap_or(UNKNOWN_AHEAD),
        };
        let ahead = at_level.saturating_add(self.displayed_better(v, side, price)?);
        let queue = mul(i128::from(o.queue_cost_x2), i128::from(ahead))?;
        let terms = [
            (i128::from(o.w_rebate), rebate),
            (i128::from(o.w_spread), spread),
            (i128::from(o.w_rebalance), rebalance),
            (-i128::from(o.w_adverse), adverse),
            (-i128::from(o.w_inventory), inventory),
            (-i128::from(o.w_queue), queue),
        ];
        let mut j = 0_i128;
        for (w, term) in terms {
            j = j.checked_add(mul(w, term)?).ok_or_else(overflow)?;
        }
        Ok(Some(j))
    }
    /// Passive rebalance price for a child of `side`: a long's sell improves the ask, a short's
    /// buy improves the bid, by up to `rebalance_improve_ticks` but never reaching the far touch.
    fn rebalance_price(
        &self,
        venue: VenueId,
        side: Side,
    ) -> Result<Option<PriceTicks>, EngineError> {
        let k = self.config.policy.rebalance_improve_ticks;
        let v = self.venue_index(venue)?;
        let price = self.touch(v)?.map(|(bid, ask)| match side {
            Side::Buy => PriceTicks((ask.0 - k).max(bid.0 + 1).min(ask.0)),
            Side::Sell => PriceTicks((bid.0 + k).min(ask.0 - 1).max(bid.0)),
        });
        // The close rests on the opposite side of the child.
        Ok(price.filter(|&p| !self.swept_through(v, opposite(side), p)))
    }
    /// One rebalance close at a time on the excess side, passively near the touch, even at a
    /// loss when the ledger's central rule accepts it. Triggered when |net - target| exceeds the
    /// threshold, or when any excess-side child reaches `rebalance_age_ns`. A mirrored
    /// pre-check avoids cancel/deny churn.
    fn rebalance(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        let p = self.config.policy;
        let target = self.ledger.config().target.0;
        let x = self.ledger.exposure();
        let deviation = x.net() - target;
        let resting = self
            .orders
            .iter()
            .position(|o| o.is_some_and(|o| o.kind == OrderKind::Rebalance));
        let excess = if deviation > 0 { Side::Buy } else { Side::Sell };
        let aged = deviation != 0
            && self
                .ledger
                .lots()
                .iter()
                .flatten()
                .any(|l| l.side == excess && self.now.0 - l.entered_at.0 >= p.rebalance_age_ns);
        if deviation.abs() <= p.rebalance_threshold_units && !aged {
            if let Some(slot) = resting {
                self.cancel_close(slot, sink)?;
            }
            return Ok(());
        }
        if let Some(slot) = resting {
            let o = self.orders[slot].ok_or(EngineError::Invariant)?;
            let lot_side = opposite(o.side);
            match self.rebalance_price(o.venue, lot_side)? {
                Some(price) if (price.0 - o.price.0).abs() >= p.reprice_ticks => {
                    self.cancel_close(slot, sink)?;
                    self.metrics.reprices += 1;
                }
                _ => return Ok(()),
            }
        }
        let mut candidates = [None; L];
        let mut count = 0;
        for lot in self.ledger.lots().iter().flatten() {
            if lot.side != excess {
                continue;
            }
            let harvest = self
                .orders
                .iter()
                .position(|o| o.is_some_and(|o| o.id == lot.id && o.kind == OrderKind::Harvest));
            if lot.pending_close.is_some() && harvest.is_none() {
                continue;
            }
            candidates[count] = Some((*lot, harvest));
            count += 1;
        }
        // Least-losing child first; ties by id for determinism.
        candidates[..count]
            .sort_by_key(|c| c.map(|(l, _)| (std::cmp::Reverse(l.unrealized), l.id)));
        let sign = if excess == Side::Buy { 1 } else { -1 };
        for (lot, harvest) in candidates[..count].iter().flatten().copied() {
            let Some(price) = self.rebalance_price(lot.venue, lot.side)? else {
                continue;
            };
            let rebate = self.charges(price, true)?;
            let q = i128::from(x.net()) + i128::from(x.closing_buys) - i128::from(x.closing_sells)
                + if harvest.is_some() {
                    i128::from(sign)
                } else {
                    0
                };
            let t = i128::from(target);
            let improves = (q - i128::from(sign) - t).abs() < (q - t).abs();
            let quantity = self.ledger.config().quantum.unit_quantity();
            let expected = (|| -> Result<Money, InventoryError> {
                Ok(realized_pnl(lot.side, lot.entry_price, price, quantity)?
                    .checked_add(lot.entry_charges.net()?)?
                    .checked_add(rebate.net()?)?
                    .checked_sub(lot.funding)?)
            })()
            .map_err(EngineError::Ledger)?;
            if !improves && expected.0 <= 0 {
                continue;
            }
            if let Some(slot) = harvest {
                self.cancel_close(slot, sink)?;
            }
            let result = self.command(
                Cmd::ReserveClose {
                    id: lot.id,
                    expected_price: price,
                    expected_charges: rebate,
                    mode: CloseMode::Normal,
                },
                sink,
            )?;
            if result.is_ok() {
                self.store(
                    lot.id,
                    lot.venue,
                    opposite(lot.side),
                    price,
                    OrderKind::Rebalance,
                )?;
                self.metrics.rebalances_placed += 1;
            }
            return Ok(());
        }
        Ok(())
    }
    /// Every child without a pending close gets a take-profit close at entry ± harvest_ticks.
    fn harvest(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        let ticks = self.config.policy.harvest_ticks;
        for i in 0..L {
            let Some(lot) = self.ledger.lots()[i] else {
                continue;
            };
            if lot.pending_close.is_some() {
                continue;
            }
            let price = PriceTicks(match lot.side {
                Side::Buy => lot.entry_price.0 + ticks,
                Side::Sell => lot.entry_price.0 - ticks,
            });
            if price.0 <= 0 {
                continue;
            }
            let rebate = self.charges(price, true)?;
            let result = self.command(
                Cmd::ReserveClose {
                    id: lot.id,
                    expected_price: price,
                    expected_charges: rebate,
                    mode: CloseMode::Normal,
                },
                sink,
            )?;
            if result.is_ok() {
                self.store(
                    lot.id,
                    lot.venue,
                    opposite(lot.side),
                    price,
                    OrderKind::Harvest,
                )?;
                self.metrics.harvests_placed += 1;
            }
        }
        Ok(())
    }
    /// Kill procedure: cancel entries, cancel normal closes, then emergency-flatten one child at
    /// a time from the majority side, filled immediately as taker at the touch minus slippage.
    /// Without a live touch it uses the ledger's last mark. Re-runs every step until flat.
    fn kill(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        self.metrics.kill_steps += 1;
        for slot in 0..L {
            match self.orders[slot].map(|o| o.kind) {
                Some(OrderKind::Entry { .. }) => self.cancel_open(slot, sink)?,
                Some(OrderKind::Harvest | OrderKind::Rebalance) => self.cancel_close(slot, sink)?,
                None => {}
            }
        }
        loop {
            let x = self.ledger.exposure();
            if x.gross() == 0 {
                return Ok(());
            }
            let want = match x.net() {
                n if n > 0 => Some(Side::Buy),
                n if n < 0 => Some(Side::Sell),
                _ => None,
            };
            let Some(lot) = self
                .ledger
                .lots()
                .iter()
                .flatten()
                .find(|l| l.pending_close.is_none() && want.is_none_or(|s| l.side == s))
                .copied()
            else {
                return Ok(());
            };
            let reference = match self.touch(self.venue_index(lot.venue)?)? {
                Some(touch) => Some(touch),
                None => self
                    .ledger
                    .mark(lot.venue)
                    .map_err(EngineError::Ledger)?
                    .map(|m| (m.bid, m.ask)),
            };
            let slip = self.config.fills.emergency_slippage_ticks;
            let price = reference.map(|(bid, ask)| match lot.side {
                Side::Buy => bid.0 - slip,
                Side::Sell => ask.0 + slip,
            });
            let Some(price) = price.filter(|&p| p > 0).map(PriceTicks) else {
                self.metrics.unpriced_emergency_steps += 1;
                return Ok(());
            };
            let fee = self.charges(price, false)?;
            let reserve = Cmd::ReserveClose {
                id: lot.id,
                expected_price: price,
                expected_charges: fee,
                mode: CloseMode::Emergency,
            };
            if self.command(reserve, sink)?.is_err() {
                return Ok(());
            }
            self.required(
                Cmd::FillClose {
                    id: lot.id,
                    price,
                    maker: false,
                    charges: fee,
                },
                sink,
            )?;
            self.metrics.taker_fills += 1;
            self.metrics.taker_notional += notional(price, self.quantity()).0;
            self.metrics.taker_fees += fee.fee.0;
        }
    }
}
