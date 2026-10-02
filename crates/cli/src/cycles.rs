//! Cycle statistics shared by `policy-replay` (the engine's own closed children) and
//! `cycle-control`: the engine's EXIT rules applied to UNGATED entries. Comparing the two on
//! the same session isolates what the void-revisit entry signal adds.
//!
//! Control rules, per venue and direction (one long and one short cycler), mirroring
//! `config/phase9-engine.toml` and the engine's policy:
//! - Idle: immediately quote one child passively at the touch (bid for a long). It is
//!   re-priced when the touch is `reprice_ticks` or more away, never at a price a print has
//!   traded through since the venue's last depth update, and withdrawn while the touch is
//!   unusable.
//! - Filled at p: the harvest close rests at p ± `harvest_ticks`.
//! - After `age_ns`, the close moves to the own-side touch (a long sells at the ask) and is
//!   re-priced like the entry: the engine's rebalance with zero improvement.
//! - Fills follow the proportional queue model; a level beyond the visible window is
//!   unobserved, so an order there joins behind whatever is displayed when it comes into view.
//! - Both fills pay the maker fee schedule. Positions open at the end are marked at the
//!   reference midpoint and reported separately.
use super::study::{Depths, Market};
use common::Side;
use execution::{
    fees::FeeSchedule,
    queue::{CancelModel, QueuedOrder},
};
use fixed_point::{PriceTicks, QtyUnits};
/// One completed child: net P&L in money atoms (fees included), gross per unit in ticks.
#[derive(Debug, Clone, Copy)]
pub(super) struct Cycle {
    pub venue: usize,
    pub side: Side,
    pub entry: i64,
    pub exit: i64,
    pub hold_ns: u64,
    pub net_atoms: i128,
    pub harvest: bool,
}
pub(super) fn report(label: &str, cycles: &[Cycle], open: usize, open_mtm_atoms: i128) {
    let n = cycles.len();
    let net: Vec<f64> = cycles.iter().map(|c| c.net_atoms as f64).collect();
    let mean = net.iter().sum::<f64>() / n.max(1) as f64;
    let sd = if n > 1 {
        (net.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        0.0
    };
    let gross: f64 = cycles
        .iter()
        .map(|c| match c.side {
            Side::Buy => (c.exit - c.entry) as f64,
            Side::Sell => (c.entry - c.exit) as f64,
        })
        .sum::<f64>()
        / n.max(1) as f64;
    let wins = cycles.iter().filter(|c| c.net_atoms > 0).count();
    let harvests = cycles.iter().filter(|c| c.harvest).count();
    let hold = cycles.iter().map(|c| c.hold_ns as f64).sum::<f64>() / n.max(1) as f64 / 1e9;
    println!(
        "{label} cycles={n} harvests={harvests} wins={wins} mean_net_atoms={mean:.1} sd={sd:.1} se={:.1} total_net_atoms={:.0} mean_gross_ticks={gross:.1} mean_hold_s={hold:.1} open_at_end={open} open_mtm_atoms={open_mtm_atoms}",
        sd / (n.max(1) as f64).sqrt(),
        net.iter().sum::<f64>(),
    );
}
#[derive(Debug, Clone, Copy)]
enum Phase {
    Idle,
    Entering(QueuedOrder),
    Holding {
        entry: i64,
        at: u64,
        exit: QueuedOrder,
        aged: bool,
    },
}
#[derive(Debug, Clone, Copy)]
pub(super) struct ControlConfig {
    pub harvest_ticks: i64,
    pub age_ns: u64,
    pub reprice_ticks: i64,
    pub model: CancelModel,
    pub schedule: FeeSchedule,
    pub size: i64,
}
/// Unobserved queue: behind everything until the level is first displayed.
const UNKNOWN_AHEAD: i64 = i64::MAX / 4;
pub(super) struct Control {
    config: ControlConfig,
    /// Per venue: [long cycler, short cycler].
    phases: Vec<[Phase; 2]>,
    swept: Vec<[Option<i64>; 2]>,
    pub cycles: Vec<Cycle>,
}
const DIRECTIONS: [Side; 2] = [Side::Buy, Side::Sell];
fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}
impl Control {
    pub fn new(venues: usize, config: ControlConfig) -> Self {
        Self {
            config,
            phases: vec![[Phase::Idle; 2]; venues],
            swept: vec![[None; 2]; venues],
            cycles: Vec::new(),
        }
    }
    fn resting(&self, v: usize, d: usize) -> Option<QueuedOrder> {
        match self.phases[v][d] {
            Phase::Idle => None,
            Phase::Entering(o) => Some(o),
            Phase::Holding { exit, .. } => Some(exit),
        }
    }
    fn set_resting(&mut self, v: usize, d: usize, o: QueuedOrder) {
        match &mut self.phases[v][d] {
            Phase::Idle => {}
            Phase::Entering(e) => *e = o,
            Phase::Holding { exit, .. } => *exit = o,
        }
    }
    /// Call BEFORE a depth frame on venue `v`.
    pub fn depths(&self, market: &impl Market, v: usize) -> Result<Depths, String> {
        let mut out = [[None; 2]; 4];
        for (d, slot) in out[0].iter_mut().enumerate() {
            if let Some(o) = self.resting(v, d) {
                *slot = market.depth(v, o.side, PriceTicks(o.price))?;
            }
        }
        Ok(out)
    }
    /// Call AFTER a depth frame on venue `v`.
    pub fn after_depth(
        &mut self,
        market: &impl Market,
        v: usize,
        before: &Depths,
    ) -> Result<(), String> {
        self.swept[v] = [None; 2];
        for (d, old) in before[0][..2].iter().enumerate() {
            let Some(mut o) = self.resting(v, d) else {
                continue;
            };
            if let Some(new) = market.depth(v, o.side, PriceTicks(o.price))? {
                o.on_depth(old.unwrap_or(new), new, self.config.model);
                self.set_resting(v, d, o);
            }
        }
        Ok(())
    }
    fn fee(&self, price: i64) -> Result<i128, String> {
        Ok(self
            .config
            .schedule
            .cost(PriceTicks(price), QtyUnits(self.config.size), true)
            .ok_or("non-positive notional")?
            .fee
            .0)
    }
    /// Call AFTER a print on venue `v`.
    pub fn on_trade(
        &mut self,
        market: &impl Market,
        v: usize,
        now: u64,
        aggressor: Side,
        price: PriceTicks,
        qty: QtyUnits,
    ) -> Result<(), String> {
        let (price, qty) = market.grid_print(v, price, qty)?;
        let swept = &mut self.swept[v];
        match aggressor {
            Side::Sell => swept[0] = Some(swept[0].map_or(price, |p| p.min(price))),
            Side::Buy => swept[1] = Some(swept[1].map_or(price, |p| p.max(price))),
        }
        for (d, side) in DIRECTIONS.into_iter().enumerate() {
            let Some(mut o) = self.resting(v, d) else {
                continue;
            };
            let got = o.on_trade(aggressor, price, qty);
            self.set_resting(v, d, o);
            if got == 0 || !o.complete() {
                continue;
            }
            match self.phases[v][d] {
                Phase::Entering(_) => {
                    let target = match side {
                        Side::Buy => o.price + self.config.harvest_ticks,
                        Side::Sell => o.price - self.config.harvest_ticks,
                    };
                    let shown = market.depth(v, opposite(side), PriceTicks(target))?;
                    self.phases[v][d] = Phase::Holding {
                        entry: o.price,
                        at: now,
                        exit: QueuedOrder::place(
                            opposite(side),
                            target,
                            self.config.size,
                            shown.unwrap_or(UNKNOWN_AHEAD),
                        ),
                        aged: false,
                    };
                }
                Phase::Holding {
                    entry, at, aged, ..
                } => {
                    let gross_ticks = match side {
                        Side::Buy => o.price - entry,
                        Side::Sell => entry - o.price,
                    };
                    let net = i128::from(gross_ticks) * i128::from(self.config.size)
                        - self.fee(entry)?
                        - self.fee(o.price)?;
                    self.cycles.push(Cycle {
                        venue: v,
                        side,
                        entry,
                        exit: o.price,
                        hold_ns: now - at,
                        net_atoms: net,
                        harvest: !aged,
                    });
                    self.phases[v][d] = Phase::Idle;
                }
                Phase::Idle => {}
            }
        }
        Ok(())
    }
    fn stale(&self, v: usize, side: Side, price: i64) -> bool {
        match (side, self.swept[v]) {
            (Side::Buy, [Some(low), _]) => low < price,
            (Side::Sell, [_, Some(high)]) => high > price,
            _ => false,
        }
    }
    /// Call after EVERY frame: entry placement, re-pricing and ageing on every venue.
    pub fn after_frame(&mut self, market: &impl Market, now: u64) -> Result<(), String> {
        let c = self.config;
        for v in 0..self.phases.len() {
            let touch = market.venue_touch(v)?;
            for (d, side) in DIRECTIONS.into_iter().enumerate() {
                let Some((bid, ask)) = touch else {
                    if matches!(self.phases[v][d], Phase::Entering(_)) {
                        self.phases[v][d] = Phase::Idle;
                    }
                    continue;
                };
                // The entry quotes the own-side touch; an aged close quotes the other side's.
                let own = |s: Side| if s == Side::Buy { bid.0 } else { ask.0 };
                match self.phases[v][d] {
                    Phase::Idle => {
                        let price = own(side);
                        if !self.stale(v, side, price) {
                            let shown = market.depth(v, side, PriceTicks(price))?;
                            self.phases[v][d] = Phase::Entering(QueuedOrder::place(
                                side,
                                price,
                                c.size,
                                shown.unwrap_or(UNKNOWN_AHEAD),
                            ));
                        }
                    }
                    Phase::Entering(o) => {
                        let price = own(side);
                        if (o.price - price).abs() >= c.reprice_ticks && !self.stale(v, side, price)
                        {
                            let shown = market.depth(v, side, PriceTicks(price))?;
                            self.phases[v][d] = Phase::Entering(QueuedOrder::place(
                                side,
                                price,
                                c.size,
                                shown.unwrap_or(UNKNOWN_AHEAD),
                            ));
                        }
                    }
                    Phase::Holding {
                        entry,
                        at,
                        exit,
                        aged,
                    } => {
                        if now - at < c.age_ns {
                            continue;
                        }
                        let close = opposite(side);
                        let price = own(close);
                        let move_it = !aged || (exit.price - price).abs() >= c.reprice_ticks;
                        if move_it && !self.stale(v, close, price) {
                            let shown = market.depth(v, close, PriceTicks(price))?;
                            self.phases[v][d] = Phase::Holding {
                                entry,
                                at,
                                exit: QueuedOrder::place(
                                    close,
                                    price,
                                    c.size,
                                    shown.unwrap_or(UNKNOWN_AHEAD),
                                ),
                                aged: true,
                            };
                        }
                    }
                }
            }
        }
        Ok(())
    }
    /// Positions still held, marked at the reference midpoint net of both fees.
    pub fn open(&self, market: &impl Market) -> Result<(usize, i128), String> {
        let Some(mid) = market.mid_x2() else {
            return Ok((0, 0));
        };
        let (mut n, mut mtm) = (0, 0_i128);
        for phases in &self.phases {
            for (d, p) in phases.iter().enumerate() {
                if let Phase::Holding { entry, .. } = *p {
                    let e2 = 2 * i128::from(entry);
                    let gross_x2 = if d == 0 { mid - e2 } else { e2 - mid };
                    let mark = i64::try_from(mid.div_euclid(2)).map_err(|e| e.to_string())?;
                    mtm += gross_x2 * i128::from(self.config.size) / 2
                        - self.fee(entry)?
                        - self.fee(mark)?;
                    n += 1;
                }
            }
        }
        Ok((n, mtm))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use engine::fixtures;
    use market_events::MarketEventType as K;
    use simulation::{Scenario, scenarios::generate};
    /// The control completes cycles on a synthetic path, books exactly the exit rules and
    /// fees, and never holds more than one child per venue and direction.
    #[test]
    fn control_cycles_follow_the_exit_rules_and_fees() {
        let mut engine = Box::new(fixtures::build().unwrap());
        let config = ControlConfig {
            harvest_ticks: 2,
            age_ns: 200_000_000,
            reprice_ticks: 2,
            model: CancelModel::Proportional,
            schedule: FeeSchedule {
                maker_fee_ppm: 1_000,
                maker_rebate_ppm: 0,
                taker_fee_ppm: 0,
            },
            size: 10,
        };
        let mut control = Control::new(3, config);
        for e in &generate(Scenario::RevisitOscillation) {
            let v = (e.venue.0 - 1) as usize;
            let before = control.depths(&*engine, v).unwrap();
            engine.apply(e, &mut |_| {}).unwrap();
            if e.event_type == K::Trade {
                control
                    .on_trade(
                        &*engine,
                        v,
                        e.receive_ts.0,
                        e.side,
                        e.price_ticks,
                        e.qty_units,
                    )
                    .unwrap();
            } else {
                control.after_depth(&*engine, v, &before).unwrap();
            }
            control.after_frame(&*engine, e.receive_ts.0).unwrap();
        }
        assert!(control.cycles.len() > 5);
        let fee = |p: i64| (i128::from(p) * 10 * 1_000 + 999_999) / 1_000_000;
        for c in &control.cycles {
            let gross = match c.side {
                Side::Buy => c.exit - c.entry,
                Side::Sell => c.entry - c.exit,
            };
            if c.harvest {
                assert_eq!(gross, config.harvest_ticks);
            }
            assert_eq!(
                c.net_atoms,
                i128::from(gross) * 10 - fee(c.entry) - fee(c.exit)
            );
        }
        assert!(control.cycles.iter().any(|c| c.harvest));
    }
}
