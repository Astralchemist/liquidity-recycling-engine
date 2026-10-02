use super::*;
use fixed_point::{notional, realized_pnl};
use risk::ReservedExposure;
fn inc(x: &mut u64) -> Result<(), InventoryError> {
    *x = x.checked_add(1).ok_or(ArithmeticError::Overflow)?;
    Ok(())
}
fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}
fn debt(value: Money) -> Result<Money, InventoryError> {
    if value.0 < 0 {
        Ok(Money(
            value.0.checked_neg().ok_or(ArithmeticError::Overflow)?,
        ))
    } else {
        Ok(Money(0))
    }
}
/// Bounded append-only history, written only after a command's transaction has succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct History<const L: usize, const E: usize> {
    closed: [Option<ClosedLot>; L],
    closed_cursor: usize,
    closed_evictions: u64,
    episodes: [Option<Episode>; E],
    episode_cursor: usize,
    episode_evictions: u64,
}
impl<const L: usize, const E: usize> History<L, E> {
    /// Both counters are checked before any slot is written, so a commit is all-or-nothing.
    fn commit(
        &mut self,
        closed: Option<ClosedLot>,
        episode: Option<Episode>,
    ) -> Result<(), InventoryError> {
        let mut closed_evictions = self.closed_evictions;
        if closed.is_some() && self.closed[self.closed_cursor].is_some() {
            inc(&mut closed_evictions)?;
        }
        let mut episode_evictions = self.episode_evictions;
        if episode.is_some() && self.episodes[self.episode_cursor].is_some() {
            inc(&mut episode_evictions)?;
        }
        if let Some(c) = closed {
            self.closed[self.closed_cursor] = Some(c);
            self.closed_cursor = (self.closed_cursor + 1) % L;
        }
        if let Some(e) = episode {
            self.episodes[self.episode_cursor] = Some(e);
            self.episode_cursor = (self.episode_cursor + 1) % E;
        }
        self.closed_evictions = closed_evictions;
        self.episode_evictions = episode_evictions;
        Ok(())
    }
}
/// Everything a command may change except history. One copy is the rollback image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State<const V: usize, const L: usize> {
    config: InventoryConfig<V>,
    lots: [Option<Lot>; L],
    reservations: [Option<Reservation>; L],
    active_episode: Option<Episode>,
    marks: [Option<Mark>; V],
    venue_gross: [i64; V],
    venue_pending: [i64; V],
    exposure: Exposure,
    accounts: Accounts,
    cash: Money,
    unrealized: Money,
    liability: Money,
    equity_high_water: Money,
    counts: ExecutionCounts,
    halt: Option<KillReason>,
    adverse_episodes: u32,
    completed_episodes: u64,
    profitable_episodes: u64,
    episode_net_total: Money,
    next_id: u64,
    next_episode_id: u64,
    last_sequence: u64,
    now: Timestamp,
    /// Staged history appends; always empty between commands.
    closed_append: Option<ClosedLot>,
    episode_append: Option<Episode>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ledger<const V: usize, const L: usize, const E: usize> {
    state: State<V, L>,
    history: History<L, E>,
}
impl<const V: usize, const L: usize, const E: usize> Ledger<V, L, E> {
    pub fn new(config: InventoryConfig<V>) -> Result<Self, InventoryError> {
        config
            .limits
            .validate()
            .map_err(|_| InventoryError::InvalidConfig)?;
        if V == 0
            || V > 64
            || L == 0
            || L > 256
            || E == 0
            || E > 128
            || config.limits.max_gross_units > L as i64
            || config.limits.max_open_orders > L
            || i128::from(config.target.0).abs() > i128::from(config.limits.max_net_units)
            || i128::from(config.target.0).abs() > i128::from(config.limits.max_target_deviation)
            || i128::from(config.target.0).abs() > i128::from(config.limits.max_gross_units)
            || i128::from(config.completion_max_gross) + i128::from(config.target_tolerance)
                < i128::from(config.target.0).abs()
            || config.target_tolerance < 0
            || config.completion_max_gross < 0
            || config.completion_max_gross > config.limits.max_gross_units
            || config
                .venues
                .iter()
                .enumerate()
                .any(|(i, v)| config.venues[..i].contains(v))
        {
            return Err(InventoryError::InvalidConfig);
        }
        config
            .quantum
            .quantity(InventoryUnits(config.limits.max_gross_units))?;
        Ok(Self {
            state: State {
                config,
                lots: [None; L],
                reservations: [None; L],
                active_episode: None,
                marks: [None; V],
                venue_gross: [0; V],
                venue_pending: [0; V],
                exposure: Exposure::default(),
                accounts: Accounts::default(),
                cash: Money(0),
                unrealized: Money(0),
                liability: Money(0),
                equity_high_water: Money(0),
                counts: ExecutionCounts::default(),
                halt: None,
                adverse_episodes: 0,
                completed_episodes: 0,
                profitable_episodes: 0,
                episode_net_total: Money(0),
                next_id: 1,
                next_episode_id: 1,
                last_sequence: 0,
                now: Timestamp(0),
                closed_append: None,
                episode_append: None,
            },
            history: History {
                closed: [None; L],
                closed_cursor: 0,
                closed_evictions: 0,
                episodes: [None; E],
                episode_cursor: 0,
                episode_evictions: 0,
            },
        })
    }
    /// Bytes copied per command as the rollback image (history is excluded).
    pub const fn rollback_image_bytes() -> usize {
        size_of::<State<V, L>>()
    }
    pub fn config(&self) -> InventoryConfig<V> {
        self.state.config
    }
    pub fn lots(&self) -> &[Option<Lot>; L] {
        &self.state.lots
    }
    pub fn reservations(&self) -> &[Option<Reservation>; L] {
        &self.state.reservations
    }
    pub fn closed_lots(&self) -> &[Option<ClosedLot>; L] {
        &self.history.closed
    }
    pub fn episodes(&self) -> &[Option<Episode>; E] {
        &self.history.episodes
    }
    pub fn active_episode(&self) -> Option<Episode> {
        self.state.active_episode
    }
    pub fn episode_totals(&self) -> (u64, u64, Money) {
        (
            self.state.completed_episodes,
            self.state.profitable_episodes,
            self.state.episode_net_total,
        )
    }
    pub fn unassigned_pnl(&self) -> Result<Money, InventoryError> {
        Ok(self
            .equity()?
            .checked_sub(self.state.episode_net_total)?
            .checked_sub(self.state.active_episode.map_or(Money(0), |e| e.net_pnl))?)
    }
    pub fn venue_exposure(&self, venue: VenueId) -> Result<(i64, i64), InventoryError> {
        let v = self.state.venue(venue)?;
        Ok((self.state.venue_gross[v], self.state.venue_pending[v]))
    }
    pub fn exposure(&self) -> Exposure {
        self.state.exposure
    }
    pub fn accounts(&self) -> Accounts {
        self.state.accounts
    }
    pub fn cash(&self) -> Money {
        self.state.cash
    }
    pub fn unrealized(&self) -> Money {
        self.state.unrealized
    }
    pub fn liability(&self) -> Money {
        self.state.liability
    }
    pub fn halt_reason(&self) -> Option<KillReason> {
        self.state.halt
    }
    pub fn now(&self) -> Timestamp {
        self.state.now
    }
    pub fn last_sequence(&self) -> u64 {
        self.state.last_sequence
    }
    pub fn counts(&self) -> ExecutionCounts {
        self.state.counts
    }
    pub fn history_evictions(&self) -> (u64, u64) {
        (
            self.history.closed_evictions,
            self.history.episode_evictions,
        )
    }
    pub fn recovery(&self) -> Result<Money, InventoryError> {
        Ok(self
            .state
            .accounts
            .harvest()?
            .checked_sub(self.state.liability)?)
    }
    pub fn equity(&self) -> Result<Money, InventoryError> {
        self.state.equity()
    }
    pub fn drawdown(&self) -> Result<Money, InventoryError> {
        self.state.drawdown()
    }
    pub fn safety_work_remaining(&self) -> bool {
        let s = &self.state;
        s.halt.is_some() && (s.exposure.gross() > 0 || s.exposure.open_orders > 0)
    }
    /// One all-or-nothing transaction: failed arithmetic/approval never partially changes balances.
    /// The command runs in place against a single rollback image; success needs no second copy.
    /// Protocol faults additionally latch a halt; acknowledged fills after a halt remain account-able.
    pub fn apply(&mut self, event: InventoryEvent) -> Result<Outcome, InventoryError> {
        let s = &mut self.state;
        if s.last_sequence.checked_add(1) != Some(event.sequence) {
            s.latch(KillReason::SequenceGap);
            return Err(InventoryError::SequenceGap);
        }
        if event.timestamp < s.now {
            s.latch(KillReason::ClockAnomaly);
            return Err(InventoryError::ClockRegression);
        }
        let rollback = *s;
        let result = s.transact(event).and_then(|outcome| {
            let (closed, episode) = (s.closed_append.take(), s.episode_append.take());
            self.history.commit(closed, episode)?;
            Ok(outcome)
        });
        if let Err(e) = result {
            // Preserve a newly observed safety breach even when an approval is denied.
            let observed = self.state.halt;
            self.state = rollback;
            if let Some(reason) = observed {
                self.state.latch(reason);
            }
            if matches!(e, InventoryError::Arithmetic(_)) {
                self.state.latch(KillReason::UnhandledState);
            }
        }
        result
    }
}
impl<const V: usize, const L: usize> State<V, L> {
    fn equity(&self) -> Result<Money, InventoryError> {
        Ok(self.accounts.harvest()?.checked_add(self.unrealized)?)
    }
    fn drawdown(&self) -> Result<Money, InventoryError> {
        Ok(self.equity_high_water.checked_sub(self.equity()?)?)
    }
    fn venue(&self, v: VenueId) -> Result<usize, InventoryError> {
        self.config
            .venues
            .iter()
            .position(|&x| x == v)
            .ok_or(InventoryError::UnknownVenue)
    }
    fn lot_index(&self, id: u64) -> Result<usize, InventoryError> {
        self.lots
            .iter()
            .position(|l| l.is_some_and(|l| l.id == id))
            .ok_or(InventoryError::UnknownId)
    }
    fn reservation_index(&self, id: u64) -> Result<usize, InventoryError> {
        self.reservations
            .iter()
            .position(|r| r.is_some_and(|r| r.id == id))
            .ok_or(InventoryError::UnknownId)
    }
    fn mark(&self, v: usize) -> Result<Mark, InventoryError> {
        self.marks[v].ok_or(Denial::NoMark.into())
    }
    fn risk_exposure(&self, v: usize) -> ReservedExposure {
        ReservedExposure {
            net: self.exposure.net(),
            gross: self.exposure.gross(),
            pending_buys: self.exposure.pending_buys + self.exposure.closing_buys,
            pending_sells: self.exposure.pending_sells + self.exposure.closing_sells,
            pending_open_units: self.exposure.pending_buys + self.exposure.pending_sells,
            venue_gross_and_pending: self.venue_gross[v] + self.venue_pending[v],
            open_orders: self.exposure.open_orders,
        }
    }
    fn upnl(&self, lot: Lot) -> Result<Money, InventoryError> {
        let m = self.mark(self.venue(lot.venue)?)?;
        Ok(realized_pnl(
            lot.side,
            lot.entry_price,
            if lot.side == Side::Buy { m.bid } else { m.ask },
            self.config.quantum.unit_quantity(),
        )?)
    }
    fn replace_upnl(&mut self, old: Money, new: Money) -> Result<(), InventoryError> {
        self.unrealized = self.unrealized.checked_sub(old)?.checked_add(new)?;
        self.liability = self
            .liability
            .checked_sub(debt(old)?)?
            .checked_add(debt(new)?)?;
        Ok(())
    }
    fn apply_charges(&mut self, c: Charges) -> Result<(), InventoryError> {
        c.validate()?;
        self.accounts.charge(c)?;
        self.cash = self.cash.checked_add(c.net()?)?;
        Ok(())
    }
    fn pending(&mut self, side: Side, closing: bool, delta: i64) {
        let count = match (side, closing) {
            (Side::Buy, false) => &mut self.exposure.pending_buys,
            (Side::Sell, false) => &mut self.exposure.pending_sells,
            (Side::Buy, true) => &mut self.exposure.closing_buys,
            (Side::Sell, true) => &mut self.exposure.closing_sells,
        };
        *count += delta;
        if delta > 0 {
            self.exposure.open_orders += 1;
        } else {
            self.exposure.open_orders -= 1;
        }
    }
    fn latch(&mut self, reason: KillReason) {
        if self.halt.is_none() {
            self.halt = Some(reason);
        }
    }
    /// Refresh risk first so a late approval cannot sneak past an expired mark/episode.
    fn transact(&mut self, event: InventoryEvent) -> Result<Outcome, InventoryError> {
        self.now = event.timestamp;
        self.assess()?;
        let outcome = self.process(event.kind)?;
        self.assess()?;
        self.update_episode()?;
        self.last_sequence = event.sequence;
        Ok(outcome)
    }
    fn process(&mut self, kind: InventoryEventKind) -> Result<Outcome, InventoryError> {
        match kind {
            InventoryEventKind::Mark { venue, bid, ask } => {
                if bid.0 <= 0 || ask < bid {
                    return Err(InventoryError::InvalidInput);
                }
                let v = self.venue(venue)?;
                self.marks[v] = Some(Mark {
                    bid,
                    ask,
                    timestamp: self.now,
                });
                for i in 0..L {
                    if let Some(mut lot) = self.lots[i] {
                        if lot.venue == venue {
                            let new = self.upnl(lot)?;
                            self.replace_upnl(lot.unrealized, new)?;
                            lot.unrealized = new;
                            self.lots[i] = Some(lot);
                        }
                    }
                }
            }
            InventoryEventKind::ReserveOpen {
                venue,
                side,
                role,
                evidence,
            } => {
                if self.halt.is_some() {
                    return Err(Denial::Halted.into());
                }
                if role == InventoryRole::ExitPending {
                    return Err(InventoryError::InvalidInput);
                }
                if !evidence.qualified
                    || evidence.void_id == 0
                    || evidence.revisited_at > self.now
                    || self.now > evidence.valid_until
                {
                    return Err(Denial::NotQualified.into());
                }
                let v = self.venue(venue)?;
                let m = self.mark(v)?;
                if self.now.0 - m.timestamp.0 > self.config.limits.mark_stale_ns {
                    return Err(Denial::NoMark.into());
                }
                self.config.limits.approve_open(
                    self.risk_exposure(v),
                    side,
                    self.config.target.0,
                )?;
                if self.exposure.gross() + self.exposure.pending_buys + self.exposure.pending_sells
                    >= L as i64
                {
                    return Err(Denial::Capacity.into());
                }
                let slot = self
                    .reservations
                    .iter()
                    .position(Option::is_none)
                    .ok_or(Denial::Capacity)?;
                let id = self.next_id;
                inc(&mut self.next_id)?;
                self.reservations[slot] = Some(Reservation {
                    id,
                    venue,
                    side,
                    role,
                    evidence,
                    timestamp: self.now,
                });
                self.venue_pending[v] += 1;
                self.pending(side, false, 1);
                inc(&mut self.counts.orders)?;
                return Ok(Outcome::Reserved(id));
            }
            InventoryEventKind::FillOpen {
                id,
                price,
                maker,
                charges,
            } => {
                if price.0 <= 0 {
                    return Err(InventoryError::InvalidInput);
                }
                charges.validate()?;
                let r = self.reservations[self.reservation_index(id)?]
                    .ok_or(InventoryError::UnknownId)?;
                let slot = self
                    .lots
                    .iter()
                    .position(Option::is_none)
                    .ok_or(Denial::Capacity)?;
                if self.active_episode.is_none() {
                    self.start_episode(r.evidence.void_id)?;
                }
                let mut lot = Lot {
                    id,
                    venue: r.venue,
                    side: r.side,
                    entry_price: price,
                    quantity: self.config.quantum.unit_quantity(),
                    entered_at: self.now,
                    entry_charges: charges,
                    maker_entry: maker,
                    funding: Money(0),
                    unrealized: Money(0),
                    role: r.role,
                    previous_role: r.role,
                    pending_close: None,
                };
                lot.unrealized = self.upnl(lot)?;
                self.replace_upnl(Money(0), lot.unrealized)?;
                let value = notional(price, self.config.quantum.unit_quantity());
                self.cash = if lot.side == Side::Buy {
                    self.cash.checked_sub(value)?
                } else {
                    self.cash.checked_add(value)?
                };
                self.apply_charges(charges)?;
                let v = self.venue(r.venue)?;
                self.venue_pending[v] -= 1;
                self.venue_gross[v] += 1;
                self.pending(r.side, false, -1);
                if r.side == Side::Buy {
                    self.exposure.long += 1;
                } else {
                    self.exposure.short += 1;
                }
                let ri = self.reservation_index(id)?;
                self.reservations[ri] = None;
                self.lots[slot] = Some(lot);
                inc(&mut self.counts.fills)?;
                if maker {
                    inc(&mut self.counts.maker_fills)?;
                }
                return Ok(Outcome::Opened(id));
            }
            InventoryEventKind::CancelOpen { id } => {
                let i = self.reservation_index(id)?;
                let r = self.reservations[i].ok_or(InventoryError::UnknownId)?;
                self.venue_pending[self.venue(r.venue)?] -= 1;
                self.pending(r.side, false, -1);
                self.reservations[i] = None;
            }
            InventoryEventKind::ReserveClose {
                id,
                expected_price,
                expected_charges,
                mode,
            } => {
                if expected_price.0 <= 0 {
                    return Err(InventoryError::InvalidInput);
                }
                expected_charges.validate()?;
                let i = self.lot_index(id)?;
                let mut lot = self.lots[i].ok_or(InventoryError::UnknownId)?;
                if lot.pending_close.is_some() {
                    return Err(InventoryError::AlreadyPending);
                }
                if mode == CloseMode::Emergency && self.halt.is_none() {
                    return Err(Denial::Halted.into());
                }
                if mode == CloseMode::Normal && self.halt.is_some() {
                    return Err(Denial::Halted.into());
                }
                let v = self.venue(lot.venue)?;
                self.config.limits.approve_close(
                    self.risk_exposure(v),
                    opposite(lot.side),
                    self.config.target.0,
                )?;
                if mode == CloseMode::Normal {
                    let gross = realized_pnl(
                        lot.side,
                        lot.entry_price,
                        expected_price,
                        self.config.quantum.unit_quantity(),
                    )?;
                    let net = gross
                        .checked_add(lot.entry_charges.net()?)?
                        .checked_add(expected_charges.net()?)?
                        .checked_sub(lot.funding)?;
                    // Project committed closes so two closes cannot claim the same rebalance.
                    let q = i128::from(self.exposure.net())
                        + i128::from(self.exposure.closing_buys)
                        - i128::from(self.exposure.closing_sells);
                    let next = q + if lot.side == Side::Buy { -1 } else { 1 };
                    let target = i128::from(self.config.target.0);
                    let improves = (next - target).abs() < (q - target).abs();
                    if net.0 <= 0 && !improves {
                        return Err(Denial::NoBenefit.into());
                    }
                }
                lot.previous_role = lot.role;
                lot.role = InventoryRole::ExitPending;
                lot.pending_close = Some(mode);
                self.lots[i] = Some(lot);
                self.pending(opposite(lot.side), true, 1);
                inc(&mut self.counts.orders)?;
            }
            InventoryEventKind::FillClose {
                id,
                price,
                maker,
                charges,
            } => {
                if price.0 <= 0 {
                    return Err(InventoryError::InvalidInput);
                }
                charges.validate()?;
                let i = self.lot_index(id)?;
                let lot = self.lots[i].ok_or(InventoryError::UnknownId)?;
                let mode = lot.pending_close.ok_or(InventoryError::NotPending)?;
                let pnl = realized_pnl(
                    lot.side,
                    lot.entry_price,
                    price,
                    self.config.quantum.unit_quantity(),
                )?;
                let net = pnl
                    .checked_add(lot.entry_charges.net()?)?
                    .checked_add(charges.net()?)?
                    .checked_sub(lot.funding)?;
                self.accounts.realized = self.accounts.realized.checked_add(pnl)?;
                let value = notional(price, self.config.quantum.unit_quantity());
                self.cash = if lot.side == Side::Buy {
                    self.cash.checked_add(value)?
                } else {
                    self.cash.checked_sub(value)?
                };
                self.apply_charges(charges)?;
                self.replace_upnl(lot.unrealized, Money(0))?;
                self.venue_gross[self.venue(lot.venue)?] -= 1;
                if lot.side == Side::Buy {
                    self.exposure.long -= 1;
                } else {
                    self.exposure.short -= 1;
                }
                self.pending(opposite(lot.side), true, -1);
                self.lots[i] = None;
                self.closed_append = Some(ClosedLot {
                    lot,
                    exit_price: price,
                    exited_at: self.now,
                    exit_charges: charges,
                    maker_exit: maker,
                    realized: pnl,
                    net_pnl: net,
                    mode,
                });
                inc(&mut self.counts.fills)?;
                if maker {
                    inc(&mut self.counts.maker_fills)?;
                }
                inc(&mut self.counts.completed_cycles)?;
                if lot.maker_entry && maker {
                    inc(&mut self.counts.maker_to_maker)?;
                }
                if lot.maker_entry && !maker {
                    inc(&mut self.counts.maker_to_taker)?;
                }
                if mode == CloseMode::Emergency && !maker {
                    inc(&mut self.counts.taker_escapes)?;
                }
                return Ok(Outcome::Closed(id));
            }
            InventoryEventKind::CancelClose { id } => {
                let i = self.lot_index(id)?;
                let mut lot = self.lots[i].ok_or(InventoryError::UnknownId)?;
                if lot.pending_close.is_none() {
                    return Err(InventoryError::NotPending);
                }
                self.pending(opposite(lot.side), true, -1);
                lot.pending_close = None;
                lot.role = lot.previous_role;
                self.lots[i] = Some(lot);
            }
            InventoryEventKind::Funding { id, cost } => {
                let i = self.lot_index(id)?;
                let mut lot = self.lots[i].ok_or(InventoryError::UnknownId)?;
                lot.funding = lot.funding.checked_add(cost)?;
                self.accounts.funding = self.accounts.funding.checked_add(cost)?;
                self.cash = self.cash.checked_sub(cost)?;
                self.lots[i] = Some(lot);
            }
            InventoryEventKind::Halt { reason } => self.latch(reason),
            InventoryEventKind::Tick => {}
        }
        Ok(Outcome::Applied)
    }
    fn assess(&mut self) -> Result<(), InventoryError> {
        let equity = self.equity()?;
        self.equity_high_water = self.equity_high_water.max(equity);
        let lim = self.config.limits;
        let reason = if i128::from(self.exposure.net()).abs() > i128::from(lim.max_net_units)
            || self.exposure.gross() > lim.max_gross_units
            || self.venue_gross.iter().any(|&g| g > lim.max_venue_units)
        {
            Some(KillReason::InventoryBreach)
        } else if self.liability >= lim.max_liability || self.drawdown()? >= lim.max_drawdown {
            Some(KillReason::PnlBreach)
        } else if self
            .lots
            .iter()
            .flatten()
            .any(|l| self.now.0 - l.entered_at.0 >= lim.max_position_age_ns)
        {
            Some(KillReason::PositionAge)
        } else if self
            .active_episode
            .is_some_and(|e| self.now.0 - e.started_at.0 >= lim.max_episode_ns)
        {
            Some(KillReason::EpisodeDuration)
        } else if (0..V).any(|v| {
            self.venue_gross[v] + self.venue_pending[v] > 0
                && self.marks[v].is_none_or(|m| self.now.0 - m.timestamp.0 > lim.mark_stale_ns)
        }) {
            Some(KillReason::StaleData)
        } else if self.adverse_episodes >= lim.max_adverse_episodes {
            Some(KillReason::AdverseEpisodes)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.latch(reason);
        }
        Ok(())
    }
    fn start_episode(&mut self, void_id: u64) -> Result<(), InventoryError> {
        let id = self.next_episode_id;
        inc(&mut self.next_episode_id)?;
        // Outstanding orders (opens and closes) are attributed to the episode they fill in.
        let mut baseline_counts = self.counts;
        baseline_counts.orders -= self.exposure.open_orders as u64;
        self.active_episode = Some(Episode {
            id,
            void_id,
            started_at: self.now,
            ended_at: None,
            forced: None,
            baseline_accounts: self.accounts,
            baseline_unrealized: self.unrealized,
            baseline_counts,
            peak_long: self.exposure.long,
            peak_short: self.exposure.short,
            peak_gross: self.exposure.gross(),
            peak_net: self.exposure.net().abs(),
            peak_net_deviation: (i128::from(self.exposure.net()) - i128::from(self.config.target.0))
                .abs() as i64,
            peak_liability: self.liability,
            first_recovery_at: None,
            experienced_deficit: false,
            accounts: Accounts::default(),
            residual_mtm: self.unrealized,
            net_pnl: Money(0),
            counts: ExecutionCounts::default(),
        });
        Ok(())
    }
    fn update_episode(&mut self) -> Result<(), InventoryError> {
        let Some(mut e) = self.active_episode else {
            return Ok(());
        };
        e.peak_long = e.peak_long.max(self.exposure.long);
        e.peak_short = e.peak_short.max(self.exposure.short);
        e.peak_gross = e.peak_gross.max(self.exposure.gross());
        e.peak_net = e.peak_net.max(self.exposure.net().abs());
        e.peak_net_deviation = e
            .peak_net_deviation
            .max((i128::from(self.exposure.net()) - i128::from(self.config.target.0)).abs() as i64);
        e.peak_liability = e.peak_liability.max(self.liability);
        e.accounts = self.accounts.difference(e.baseline_accounts)?;
        e.residual_mtm = self.unrealized;
        e.counts = self.counts.difference(e.baseline_counts);
        e.net_pnl = e
            .accounts
            .harvest()?
            .checked_add(e.residual_mtm)?
            .checked_sub(e.baseline_unrealized)?;
        let recovery = e.accounts.harvest()?.checked_sub(self.liability)?;
        if recovery.0 < 0 {
            e.experienced_deficit = true;
        } else if e.experienced_deficit && e.first_recovery_at.is_none() {
            e.first_recovery_at = Some(self.now);
        }
        let near = (i128::from(self.exposure.net()) - i128::from(self.config.target.0)).abs()
            <= i128::from(self.config.target_tolerance);
        if self.halt.is_some() {
            e.forced = self.halt;
        }
        let complete = if self.halt.is_some() {
            self.exposure.gross() == 0 && self.exposure.open_orders == 0
        } else {
            near && self.exposure.gross() <= self.config.completion_max_gross
                && self.exposure.open_orders == 0
        };
        if complete {
            e.ended_at = Some(self.now);
            inc(&mut self.completed_episodes)?;
            if e.net_pnl.0 > 0 {
                inc(&mut self.profitable_episodes)?;
            }
            self.episode_net_total = self.episode_net_total.checked_add(e.net_pnl)?;
            if e.net_pnl.0 < 0 {
                self.adverse_episodes = self
                    .adverse_episodes
                    .checked_add(1)
                    .ok_or(ArithmeticError::Overflow)?;
            } else {
                self.adverse_episodes = 0;
            }
            self.episode_append = Some(e);
            self.active_episode = None;
            if self.adverse_episodes >= self.config.limits.max_adverse_episodes {
                self.latch(KillReason::AdverseEpisodes);
            }
        } else {
            self.active_episode = Some(e);
        }
        Ok(())
    }
}
