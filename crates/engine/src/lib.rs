//! Phase 7 orchestration: one synchronous owner wiring the shared research path, the
//! environment classifier, the inventory ledger, a transparent recycling policy, an explicit
//! rule-based fill model and the kill executor. Direct input and replay call the same `apply`.
//!
//! Fills are SIMULATED by one stated rule (strict trade-through). This is not a queue model
//! and not evidence of executable edge; Phase 9 replaces it with a configurable simulator and
//! Phase 10 with exchange acknowledgements.
#![forbid(unsafe_code)]
pub mod fixtures;
use book::{BookError, BookState};
use common::{Side, Timestamp, VenueId};
use consolidator::{
    ConsolidationError, VenueConfig,
    normalization::{InstrumentMetadata, NormalizationError, Normalizer},
};
use fixed_point::{Money, PriceTicks, realized_pnl};
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
/// Synthetic fee model: fixed atoms per one-quantum fill. Realistic schedules are Phase 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillConfig {
    pub maker_rebate: Money,
    /// Retail maker fills usually PAY a fee (see docs/providers.md); both may be configured.
    pub maker_fee: Money,
    pub taker_fee: Money,
    pub emergency_slippage_ticks: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub environment: EnvironmentConfig,
    pub policy: PolicyConfig,
    pub fills: FillConfig,
}
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
        {
            return Err(EngineError::InvalidConfig);
        }
        let research = Box::new(
            FlowResearchEngine::new(grid, venues, liquidity, voids, flow)
                .map_err(EngineError::Research)?,
        );
        Ok(Self {
            research,
            environment: EnvironmentTracker::new(config.environment)
                .map_err(EngineError::Environment)?,
            ledger: Ledger::new(inventory).map_err(EngineError::Ledger)?,
            orders: [None; L],
            venues: venues.map(|c| c.venue),
            normalizers: venues
                .map(|c| Normalizer::new(c.metadata, grid).expect("validated by research")),
            config,
            attribution: flow.removal_attribution,
            marks: [None; V],
            research_fault: None,
            environment_fault: None,
            metrics: EngineMetrics::default(),
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
    fn maker_charges(&self) -> Charges {
        Charges {
            rebate: self.config.fills.maker_rebate,
            fee: self.config.fills.maker_fee,
            slippage: Money(0),
        }
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
    fn touch(&self, v: usize) -> Result<Option<(PriceTicks, PriceTicks)>, EngineError> {
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
    fn store(&mut self, order: RestingOrder) -> Result<(), EngineError> {
        let slot = self
            .orders
            .iter()
            .position(Option::is_none)
            .ok_or(EngineError::Invariant)?;
        self.orders[slot] = Some(order);
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
            if let Err(e) = self.research.apply(event) {
                self.latch_research_fault(e, sink)?;
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
            if let Err(e) = self.research.apply_depth_batch(venue, events) {
                self.latch_research_fault(e, sink)?;
            }
        }
        self.finish(venue, None, commands, sink)
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
        if let Some((side, price, _)) = print {
            let price = self.normalizers[v].price(price)?;
            self.fill(v, price, side, sink)?;
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
    /// Strict trade-through: a print strictly beyond a resting price fills it at that price as
    /// maker. Print size and queue position are ignored; Phase 9 models them.
    fn fill(
        &mut self,
        v: usize,
        price: PriceTicks,
        aggressor: Side,
        sink: &mut impl FnMut(&InventoryEvent),
    ) -> Result<(), EngineError> {
        for slot in 0..L {
            let Some(o) = self.orders[slot] else { continue };
            let through = match (aggressor, o.side) {
                (Side::Sell, Side::Buy) => price < o.price,
                (Side::Buy, Side::Sell) => price > o.price,
                _ => false,
            };
            if o.venue != self.venues[v] || !through {
                continue;
            }
            let charges = self.maker_charges();
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
                if let Some(slot) = self
                    .orders
                    .iter()
                    .position(|o| o.is_some_and(|o| o.kind == kind && o.side == side))
                {
                    let o = self.orders[slot].ok_or(EngineError::Invariant)?;
                    if o.venue == self.venues[v] && (o.price.0 - desired.0).abs() < p.reprice_ticks
                    {
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
                    self.store(RestingOrder {
                        id,
                        venue: self.venues[v],
                        side,
                        price: desired,
                        kind,
                    })?;
                    self.metrics.entries_placed += 1;
                }
            }
        }
        Ok(())
    }
    /// Passive rebalance price for a child of `side`: a long's sell improves the ask, a short's
    /// buy improves the bid, by up to `rebalance_improve_ticks` but never reaching the far touch.
    fn rebalance_price(
        &self,
        venue: VenueId,
        side: Side,
    ) -> Result<Option<PriceTicks>, EngineError> {
        let k = self.config.policy.rebalance_improve_ticks;
        Ok(self
            .touch(self.venue_index(venue)?)?
            .map(|(bid, ask)| match side {
                Side::Buy => PriceTicks((ask.0 - k).max(bid.0 + 1).min(ask.0)),
                Side::Sell => PriceTicks((bid.0 + k).min(ask.0 - 1).max(bid.0)),
            }))
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
            let rebate = self.maker_charges();
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
                self.store(RestingOrder {
                    id: lot.id,
                    venue: lot.venue,
                    side: opposite(lot.side),
                    price,
                    kind: OrderKind::Rebalance,
                })?;
                self.metrics.rebalances_placed += 1;
            }
            return Ok(());
        }
        Ok(())
    }
    /// Every child without a pending close gets a take-profit close at entry ± harvest_ticks.
    fn harvest(&mut self, sink: &mut impl FnMut(&InventoryEvent)) -> Result<(), EngineError> {
        let ticks = self.config.policy.harvest_ticks;
        let rebate = self.maker_charges();
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
                self.store(RestingOrder {
                    id: lot.id,
                    venue: lot.venue,
                    side: opposite(lot.side),
                    price,
                    kind: OrderKind::Harvest,
                })?;
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
            let fee = Charges {
                fee: self.config.fills.taker_fee,
                ..Charges::default()
            };
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
        }
    }
}
