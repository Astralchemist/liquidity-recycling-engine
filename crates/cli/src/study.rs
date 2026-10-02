//! Fill study (Phase 9): shadow maker probes replayed over a session. Each venue carries one
//! probe per side and per fill model, always one child at that venue's touch. Probes never
//! reach the ledger or the policy; they measure how the fill models behave on real queues.
//!
//! Rules, identical for every model:
//! - A side without a probe, or whose probe is `requote_ticks` or more BEHIND the touch,
//!   DECIDES to place one child at the touch. A touch that moved THROUGH a probe leaves it
//!   resting: it is then the best price and the next print through it fills it.
//! - A decision takes effect `latency_ns` later, at the first frame at or after that time.
//!   Until then the old probe stays live and can still fill (cancel latency). On arrival the
//!   order is post-only: it is rejected if it would cross the touch or lands where a print
//!   has traded through; otherwise it joins behind the size displayed at that moment.
//! - No decision is made at a price that a print since that venue's last depth update has
//!   traded THROUGH: depth arrives every 20 to 100 ms while prints stream continuously, so
//!   such a touch is known to be stale. Without this rule a sweep refills the same stale
//!   price on every print.
//! - An unusable venue touch (stale, withdrawn, rebuilding) withdraws that venue's probes
//!   at once and drops pending decisions.
//!
//! A CONTROL samples the reference midpoint every 100 ms of event time as a fictitious buy at
//! the midpoint (rounded down to a tick). Its drift is the unconditional midpoint change, and
//! its markout standard deviation (within half a tick) is the volatility each probe drift
//! should be read against.
use common::Side;
use engine::{Engine, FillModel};
use execution::{
    markout::{MarkoutConfig, MarkoutTracker},
    queue::{CancelModel, QueuedOrder},
};
use fixed_point::{PriceTicks, QtyUnits};
pub(super) const MODELS: [FillModel; 4] = [
    FillModel::StrictTradeThrough,
    FillModel::Queue(CancelModel::Pessimistic),
    FillModel::Queue(CancelModel::Proportional),
    FillModel::Queue(CancelModel::Optimistic),
];
const PENDING: usize = 4096;
const UNKNOWN_AHEAD: i64 = i64::MAX / 4;
const SIDES: [Side; 2] = [Side::Buy, Side::Sell];
/// What the study needs from the engine's market state, in grid units.
pub(super) trait Market {
    fn depth(&self, v: usize, side: Side, price: PriceTicks) -> Result<Option<i64>, String>;
    fn venue_touch(&self, v: usize) -> Result<Option<(PriceTicks, PriceTicks)>, String>;
    fn mid_x2(&self) -> Option<i128>;
    fn grid_print(&self, v: usize, price: PriceTicks, qty: QtyUnits) -> Result<(i64, i64), String>;
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
> Market for Engine<V, N, G, P, B, Z, W, L, E>
{
    fn depth(&self, v: usize, side: Side, price: PriceTicks) -> Result<Option<i64>, String> {
        self.displayed(v, side, price).map_err(|e| format!("{e:?}"))
    }
    fn venue_touch(&self, v: usize) -> Result<Option<(PriceTicks, PriceTicks)>, String> {
        self.touch(v).map_err(|e| format!("{e:?}"))
    }
    fn mid_x2(&self) -> Option<i128> {
        self.reference_mid_x2()
    }
    fn grid_print(&self, v: usize, price: PriceTicks, qty: QtyUnits) -> Result<(i64, i64), String> {
        let n = self.normalizers()[v];
        let price = n.price(price).map_err(|e| format!("{e:?}"))?;
        let qty = n.quantity(qty).map_err(|e| format!("{e:?}"))?;
        Ok((price.0, qty.0))
    }
}
#[derive(Debug, Clone, Copy)]
struct Probe {
    order: QueuedOrder,
    placed_at: u64,
}
/// A placement decided at `decided_at`, live from `decided_at + latency`.
#[derive(Debug, Clone, Copy)]
struct Decision {
    price: i64,
    decided_at: u64,
}
#[derive(Debug, Default, Clone, Copy)]
struct Slot {
    live: Option<Probe>,
    pending: Option<Decision>,
}
/// Results for one venue and one fill model (both sides).
pub(super) struct Cell {
    pub model: FillModel,
    pub placed: u64,
    pub fills_at_price: u64,
    pub fills_through: u64,
    pub buy_fills: u64,
    /// Fills of a live probe while its replacement was in flight.
    pub fills_while_replacing: u64,
    pub partial_prints: u64,
    pub requotes: u64,
    pub suspensions: u64,
    /// Decisions deferred because a print had traded through the touch price.
    pub stale_blocks: u64,
    /// Arrivals rejected as post-only: crossing the touch, or behind a sweep.
    pub rejects: u64,
    /// Arrivals beyond the visible window (unknown queue; excluded from `ahead_at_entry`).
    pub unobserved_entries: u64,
    pub abandoned_partial_qty: i64,
    pub waits_ns: Vec<u64>,
    pub ahead_at_entry: i128,
    pub fill_price_sum: i128,
    pub markouts: Box<MarkoutTracker<PENDING>>,
    slots: [Slot; 2],
}
impl Cell {
    pub fn fills(&self) -> u64 {
        self.fills_at_price + self.fills_through
    }
}
const CONTROL_EVERY_NS: u64 = 100_000_000;
pub(super) struct Study {
    pub cells: Vec<[Cell; 4]>,
    pub control: Box<MarkoutTracker<PENDING>>,
    control_next: u64,
    /// Per venue since its last depth update: lowest sell-aggressor and highest buy-aggressor
    /// print price (grid ticks).
    swept: Vec<[Option<i64>; 2]>,
    requote_ticks: i64,
    latency_ns: u64,
    size: i64,
}
/// Displayed sizes at every live probe price on one venue, captured before a depth frame.
pub(super) type Depths = [[Option<i64>; 2]; 4];
fn swept_through(side: Side, swept: Option<i64>, price: i64) -> bool {
    match (side, swept) {
        (Side::Buy, Some(low)) => low < price,
        (Side::Sell, Some(high)) => high > price,
        (_, None) => false,
    }
}
impl Study {
    pub fn new(
        venues: usize,
        requote_ticks: i64,
        latency_ns: u64,
        size: i64,
        horizons: MarkoutConfig,
    ) -> Self {
        assert!(requote_ticks >= 1 && size >= 1);
        let cell = |model| Cell {
            model,
            placed: 0,
            fills_at_price: 0,
            fills_through: 0,
            buy_fills: 0,
            fills_while_replacing: 0,
            partial_prints: 0,
            requotes: 0,
            suspensions: 0,
            stale_blocks: 0,
            rejects: 0,
            unobserved_entries: 0,
            abandoned_partial_qty: 0,
            waits_ns: Vec::new(),
            ahead_at_entry: 0,
            fill_price_sum: 0,
            markouts: Box::new(MarkoutTracker::new(horizons)),
            slots: [Slot::default(); 2],
        };
        Self {
            cells: (0..venues).map(|_| MODELS.map(cell)).collect(),
            control: Box::new(MarkoutTracker::new(horizons)),
            control_next: 0,
            swept: vec![[None; 2]; venues],
            requote_ticks,
            latency_ns,
            size,
        }
    }
    /// Call BEFORE a depth frame on venue `v` is applied.
    pub fn depths(&self, market: &impl Market, v: usize) -> Result<Depths, String> {
        let mut out = [[None; 2]; 4];
        for (m, cell) in self.cells[v].iter().enumerate() {
            for (s, slot) in cell.slots.iter().enumerate() {
                if let Some(p) = slot.live {
                    out[m][s] = market.depth(v, p.order.side, PriceTicks(p.order.price))?;
                }
            }
        }
        Ok(out)
    }
    /// Call AFTER a depth frame on venue `v` was applied.
    pub fn after_depth(
        &mut self,
        market: &impl Market,
        v: usize,
        before: &Depths,
    ) -> Result<(), String> {
        self.swept[v] = [None; 2];
        for (m, cell) in self.cells[v].iter_mut().enumerate() {
            let FillModel::Queue(model) = cell.model else {
                continue;
            };
            for (s, slot) in cell.slots.iter_mut().enumerate() {
                let Some(p) = slot.live.as_mut() else {
                    continue;
                };
                if let Some(new) = market.depth(v, p.order.side, PriceTicks(p.order.price))? {
                    p.order.on_depth(before[m][s].unwrap_or(new), new, model);
                }
            }
        }
        Ok(())
    }
    /// Call AFTER a print on venue `v` was applied.
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
        let mid = market.mid_x2();
        let swept = &mut self.swept[v];
        match aggressor {
            Side::Sell => swept[0] = Some(swept[0].map_or(price, |p| p.min(price))),
            Side::Buy => swept[1] = Some(swept[1].map_or(price, |p| p.max(price))),
        }
        for cell in self.cells[v].iter_mut() {
            for slot in cell.slots.iter_mut() {
                let Some(p) = slot.live.as_mut() else {
                    continue;
                };
                let through = match p.order.side {
                    Side::Buy => aggressor == Side::Sell && price < p.order.price,
                    Side::Sell => aggressor == Side::Buy && price > p.order.price,
                };
                let complete = match cell.model {
                    FillModel::StrictTradeThrough => through,
                    FillModel::Queue(_) => {
                        let got = p.order.on_trade(aggressor, price, qty);
                        if got > 0 && !p.order.complete() {
                            cell.partial_prints += 1;
                        }
                        got > 0 && p.order.complete()
                    }
                };
                if !complete {
                    continue;
                }
                if through {
                    cell.fills_through += 1;
                } else {
                    cell.fills_at_price += 1;
                }
                cell.buy_fills += u64::from(p.order.side == Side::Buy);
                cell.fills_while_replacing += u64::from(slot.pending.is_some());
                cell.waits_ns.push(now - p.placed_at);
                cell.fill_price_sum += i128::from(p.order.price);
                cell.markouts
                    .record(now, p.order.side, p.order.price, self.size, mid);
                slot.live = None;
            }
        }
        Ok(())
    }
    /// A due decision arrives: post-only checks, then the probe replaces any live one.
    #[allow(clippy::too_many_arguments)]
    fn arrive(
        cell: &mut Cell,
        s: usize,
        market: &impl Market,
        v: usize,
        touch: (PriceTicks, PriceTicks),
        swept: Option<i64>,
        latency_ns: u64,
        size: i64,
        now: u64,
    ) -> Result<(), String> {
        let Some(d) = cell.slots[s].pending else {
            return Ok(());
        };
        if now < d.decided_at.saturating_add(latency_ns) {
            return Ok(());
        }
        cell.slots[s].pending = None;
        let side = SIDES[s];
        let (bid, ask) = touch;
        let crosses = match side {
            Side::Buy => d.price >= ask.0,
            Side::Sell => d.price <= bid.0,
        };
        if crosses || swept_through(side, swept, d.price) {
            cell.rejects += 1;
            return Ok(());
        }
        if let Some(old) = cell.slots[s].live {
            cell.abandoned_partial_qty += old.order.filled;
        }
        // A price beyond the visible window (the market moved during the latency) is
        // unobserved: the probe joins behind whatever is displayed when the level comes into view.
        let shown = market.depth(v, side, PriceTicks(d.price))?;
        cell.placed += 1;
        cell.ahead_at_entry += i128::from(shown.unwrap_or(0));
        cell.unobserved_entries += u64::from(shown.is_none());
        cell.slots[s].live = Some(Probe {
            order: QueuedOrder::place(side, d.price, size, shown.unwrap_or(UNKNOWN_AHEAD)),
            placed_at: now,
        });
        Ok(())
    }
    /// Call after EVERY frame: arrivals and re-quote decisions on every venue, then markouts.
    pub fn after_frame(&mut self, market: &impl Market, now: u64) -> Result<(), String> {
        let (latency, size, requote) = (self.latency_ns, self.size, self.requote_ticks);
        for v in 0..self.cells.len() {
            let touch = market.venue_touch(v)?;
            let swept = self.swept[v];
            for cell in self.cells[v].iter_mut() {
                for (s, side) in SIDES.into_iter().enumerate() {
                    let Some((bid, ask)) = touch else {
                        cell.slots[s].pending = None;
                        if let Some(p) = cell.slots[s].live.take() {
                            cell.suspensions += 1;
                            cell.abandoned_partial_qty += p.order.filled;
                        }
                        continue;
                    };
                    let t = (bid, ask);
                    Self::arrive(cell, s, market, v, t, swept[s], latency, size, now)?;
                    if cell.slots[s].pending.is_some() {
                        continue;
                    }
                    let desired = if side == Side::Buy { bid.0 } else { ask.0 };
                    let need = cell.slots[s].live.is_none_or(|p| {
                        let behind = match side {
                            Side::Buy => desired - p.order.price,
                            Side::Sell => p.order.price - desired,
                        };
                        behind >= requote
                    });
                    if !need {
                        continue;
                    }
                    if swept_through(side, swept[s], desired) {
                        cell.stale_blocks += 1;
                        continue;
                    }
                    cell.requotes += u64::from(cell.slots[s].live.is_some());
                    cell.slots[s].pending = Some(Decision {
                        price: desired,
                        decided_at: now,
                    });
                    // Zero latency: the decision arrives in the same frame.
                    Self::arrive(cell, s, market, v, t, swept[s], latency, size, now)?;
                }
            }
        }
        let mid = market.mid_x2();
        for cell in self.cells.iter_mut().flatten() {
            cell.markouts.observe(now, mid);
        }
        self.control.observe(now, mid);
        if let Some(m) = mid.filter(|_| now >= self.control_next) {
            let price = i64::try_from(m.div_euclid(2)).map_err(|e| e.to_string())?;
            self.control.record(now, Side::Buy, price, 1, Some(m));
            self.control_next = now - now % CONTROL_EVERY_NS + CONTROL_EVERY_NS;
        }
        Ok(())
    }
}
pub(super) fn quantile(sorted: &[u64], q: f64) -> Option<u64> {
    (!sorted.is_empty()).then(|| sorted[((sorted.len() - 1) as f64 * q).round() as usize])
}
/// Prints one block per venue and model. `units_per_base` converts grid quantity to the base
/// asset; `maker_fee_ppm` is the fee each fill must overcome, in ppm of the fill price.
pub(super) fn report(
    study: &mut Study,
    names: &[&str],
    seconds: f64,
    units_per_base: f64,
    maker_fee_ppm: u32,
) {
    for (v, cells) in study.cells.iter_mut().enumerate() {
        for cell in cells.iter_mut() {
            cell.waits_ns.sort_unstable();
            let fills = cell.fills();
            let ms = |q| {
                quantile(&cell.waits_ns, q).map_or("-".into(), |n| format!("{:.1}", n as f64 / 1e6))
            };
            let mean_price = (fills > 0).then(|| cell.fill_price_sum as f64 / fills as f64);
            // Fee per unit expressed in ticks: price x ppm.
            let fee_ticks = mean_price.map(|p| p * f64::from(maker_fee_ppm) / 1e6);
            let ratio = |a: f64, b: f64| {
                if b > 0.0 {
                    format!("{:.4}", a / b)
                } else {
                    "-".into()
                }
            };
            println!(
                "study venue={} model={:?} placed={} fills={} buys={} at_price={} through={} while_replacing={} fills_per_min={:.2} fill_ratio={} partial_prints={} requotes={} rejects={} unobserved_entries={} stale_blocks={} suspensions={} abandoned_partial_qty={} wait_ms p50={} p90={} mean_ahead_at_entry_base={} fee_ticks_per_unit={}",
                names[v],
                cell.model,
                cell.placed,
                fills,
                cell.buy_fills,
                cell.fills_at_price,
                cell.fills_through,
                cell.fills_while_replacing,
                fills as f64 / (seconds / 60.0),
                ratio(fills as f64, cell.placed as f64),
                cell.partial_prints,
                cell.requotes,
                cell.rejects,
                cell.unobserved_entries,
                cell.stale_blocks,
                cell.suspensions,
                cell.abandoned_partial_qty,
                ms(0.5),
                ms(0.9),
                ratio(
                    cell.ahead_at_entry as f64,
                    cell.placed as f64 * units_per_base
                ),
                super::scenarios::ticks(fee_ticks),
            );
            let label = format!("study_markout venue={} model={:?}", names[v], cell.model);
            super::scenarios::markout_lines(&label, &cell.markouts);
        }
    }
    let c = &study.control;
    for (h, s) in c.stats().iter().enumerate() {
        println!(
            "study_control every_ms=100 horizon_ms={} samples={} mean_drift_ticks={} stdev_ticks={}",
            c.config().horizons_ns[h] as f64 / 1e6,
            s.samples,
            super::scenarios::ticks(s.mean_drift_ticks()),
            super::scenarios::ticks(s.stdev_ticks(c.per_fill_sum_x2(h)))
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use engine::fixtures;
    use market_events::MarketEventType as K;
    use simulation::{Scenario, scenarios::generate};
    fn drive(latency_ns: u64) -> Study {
        let mut engine = Box::new(fixtures::build().unwrap());
        let mut study = Study::new(3, 1, latency_ns, 10, MarkoutConfig::spec());
        for e in &generate(Scenario::RevisitOscillation) {
            let v = (e.venue.0 - 1) as usize;
            let fills_before: Vec<u64> = study.cells[v].iter().map(Cell::fills).collect();
            let before = study.depths(&*engine, v).unwrap();
            engine.apply(e, &mut |_| {}).unwrap();
            if e.event_type == K::Trade {
                study
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
                study.after_depth(&*engine, v, &before).unwrap();
                // Depth changes never fill.
                let after: Vec<u64> = study.cells[v].iter().map(Cell::fills).collect();
                assert_eq!(after, fills_before);
            }
            study.after_frame(&*engine, e.receive_ts.0).unwrap();
            for (v, cells) in study.cells.iter().enumerate() {
                let touch = engine.touch(v).unwrap();
                for cell in cells {
                    for (s, slot) in cell.slots.iter().enumerate() {
                        let Some((bid, ask)) = touch else {
                            assert!(slot.live.is_none() && slot.pending.is_none());
                            continue;
                        };
                        if let Some(d) = slot.pending {
                            assert!(latency_ns > 0 && e.receive_ts.0 < d.decided_at + latency_ns);
                        }
                        let Some(p) = slot.live else { continue };
                        assert!(!p.order.complete());
                        // A live probe never sits behind the touch by the re-quote distance
                        // unless a replacement is in flight or the touch is known stale.
                        let behind = if s == 0 {
                            bid.0 - p.order.price
                        } else {
                            p.order.price - ask.0
                        };
                        let stale = swept_through(SIDES[s], study.swept[v][s], [bid.0, ask.0][s]);
                        assert!(behind < 1 || slot.pending.is_some() || stale);
                        if matches!(cell.model, FillModel::Queue(_)) {
                            let shown = engine
                                .displayed(v, p.order.side, PriceTicks(p.order.price))
                                .unwrap()
                                .unwrap();
                            assert!(p.order.ahead <= shown);
                        }
                    }
                }
            }
        }
        study
    }
    #[test]
    fn probes_follow_the_touch_and_fill_only_on_prints() {
        let instant = drive(0);
        let strict: u64 = instant.cells.iter().map(|c| c[0].fills()).sum();
        let queue: u64 = instant.cells.iter().map(|c| c[1].fills()).sum();
        assert!(strict > 0 && queue >= strict);
        for cell in instant.cells.iter().flatten() {
            assert_eq!(cell.markouts.fills, cell.fills());
            assert!(cell.fills() <= cell.placed);
            assert_eq!((cell.rejects, cell.fills_while_replacing), (0, 0));
        }
        // Latency delays arrivals: the old probe can fill while its replacement is in flight.
        let slow = drive(20_000_000);
        let cells = || slow.cells.iter().flatten();
        assert!(
            cells()
                .map(|c| c.fills_while_replacing + c.rejects)
                .sum::<u64>()
                > 0
        );
        for cell in cells() {
            assert_eq!(cell.markouts.fills, cell.fills());
        }
    }
}
