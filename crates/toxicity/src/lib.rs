//! Transparent environment classification (spec §13) and toxicity components (§17).
//! Explicit thresholds over one exact bounded window; no composite black box, no forecast.
use common::{Side, Timestamp};
pub const PPM: i128 = 1_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Environment {
    Dead,
    BalancedActive,
    Trending,
    LiquidityShock,
    Chaotic,
}
impl Environment {
    pub const ALL: [Self; 5] = [
        Self::Dead,
        Self::BalancedActive,
        Self::Trending,
        Self::LiquidityShock,
        Self::Chaotic,
    ];
    pub fn bit(self) -> u8 {
        1 << self as usize
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentConfig {
    pub window_ns: u64,
    /// Consolidated spread strictly above this is a liquidity shock.
    pub shock_spread_ticks: i64,
    /// Consolidated best bid + best ask quantity strictly below this is a liquidity shock.
    pub shock_min_top_qty: i128,
    /// Spread changes in the window at or above this are chaotic.
    pub chaotic_spread_changes: u32,
    /// Trending by displacement: |net| >= this many ticks AND |net| / path >= efficiency.
    pub trend_min_move_ticks: i64,
    pub trend_efficiency_ppm: u32,
    /// Trending by aggression: buy + sell >= minimum AND |buy - sell| / (buy + sell) >= ratio.
    pub trend_min_aggressive_qty: i128,
    pub trend_aggression_ppm: u32,
    /// Dead when midpoint moves AND prints are both below these counts.
    pub active_min_moves: u32,
    pub active_min_prints: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentError {
    InvalidConfig,
    InvalidObservation,
    ClockRegression,
    Capacity,
    Overflow,
}
impl EnvironmentConfig {
    pub fn validate(self) -> Result<(), EnvironmentError> {
        if self.window_ns == 0
            || self.shock_spread_ticks <= 0
            || self.shock_min_top_qty < 0
            || self.chaotic_spread_changes == 0
            || self.trend_min_move_ticks <= 0
            || !(1..=1_000_000).contains(&self.trend_efficiency_ppm)
            || self.trend_min_aggressive_qty <= 0
            || !(1..=1_000_000).contains(&self.trend_aggression_ppm)
        {
            return Err(EnvironmentError::InvalidConfig);
        }
        Ok(())
    }
}
/// One observation after the shared market update, in common-grid units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// `None` unless the consolidated market is open (two-sided and uncrossed).
    pub midpoint_x2: Option<i128>,
    pub spread_ticks: Option<i64>,
    /// Consolidated best bid + best ask quantity.
    pub top_qty: i128,
    /// Aggressor side and common quantity of a trade print.
    pub print: Option<(Side, i128)>,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Entry {
    time: Timestamp,
    move_x2: i128,
    spread_changed: bool,
    buy: i128,
    sell: i128,
    print: bool,
}
/// Exact sums over `(now - window_ns, now]`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WindowTotals {
    pub moves: u32,
    pub path_x2: i128,
    pub net_x2: i128,
    pub spread_changes: u32,
    pub buy_qty: i128,
    pub sell_qty: i128,
    pub prints: u32,
}
impl WindowTotals {
    fn accumulate(&mut self, e: Entry, sign: i128) {
        let step = |x: u32, on: bool| {
            if !on {
                x
            } else if sign > 0 {
                x + 1
            } else {
                x - 1
            }
        };
        self.moves = step(self.moves, e.move_x2 != 0);
        self.spread_changes = step(self.spread_changes, e.spread_changed);
        self.prints = step(self.prints, e.print);
        self.path_x2 += sign * e.move_x2.abs();
        self.net_x2 += sign * e.move_x2;
        self.buy_qty += sign * e.buy;
        self.sell_qty += sign * e.sell;
    }
}
/// Deterministic classifier with first-match precedence: shock, chaotic, trending, dead, balanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentTracker<const W: usize> {
    config: EnvironmentConfig,
    entries: [Entry; W],
    head: usize,
    len: usize,
    totals: WindowTotals,
    last_mid_x2: Option<i128>,
    last_spread: Option<i64>,
    state: Environment,
    since: Timestamp,
    now: Timestamp,
    transitions: u64,
    time_in_state: [u64; 5],
}
impl<const W: usize> EnvironmentTracker<W> {
    pub fn new(config: EnvironmentConfig) -> Result<Self, EnvironmentError> {
        config.validate()?;
        if W == 0 {
            return Err(EnvironmentError::InvalidConfig);
        }
        Ok(Self {
            config,
            entries: [Entry::default(); W],
            head: 0,
            len: 0,
            totals: WindowTotals::default(),
            last_mid_x2: None,
            last_spread: None,
            state: Environment::LiquidityShock,
            since: Timestamp(0),
            now: Timestamp(0),
            transitions: 0,
            time_in_state: [0; 5],
        })
    }
    pub fn config(&self) -> EnvironmentConfig {
        self.config
    }
    pub fn state(&self) -> Environment {
        self.state
    }
    pub fn since(&self) -> Timestamp {
        self.since
    }
    pub fn totals(&self) -> WindowTotals {
        self.totals
    }
    pub fn transitions(&self) -> u64 {
        self.transitions
    }
    /// Nanoseconds spent in each state, indexed by `Environment as usize`, up to the last update.
    pub fn time_in_state(&self) -> [u64; 5] {
        self.time_in_state
    }
    fn expire(&mut self, now: Timestamp) {
        while self.len > 0 && now.0 - self.entries[self.head].time.0 >= self.config.window_ns {
            let e = self.entries[self.head];
            self.totals.accumulate(e, -1);
            self.entries[self.head] = Entry::default();
            self.head = (self.head + 1) % W;
            self.len -= 1;
        }
    }
    pub fn update(
        &mut self,
        now: Timestamp,
        obs: Observation,
    ) -> Result<Environment, EnvironmentError> {
        if now < self.now {
            return Err(EnvironmentError::ClockRegression);
        }
        if obs.top_qty < 0
            || obs.spread_ticks.is_some_and(|s| s < 0)
            || obs.midpoint_x2.is_some_and(|m| m <= 0)
            || obs.print.is_some_and(|(_, q)| q <= 0)
        {
            return Err(EnvironmentError::InvalidObservation);
        }
        // Moves are only measured between two open-market midpoints; reopening is not a move.
        let move_x2 = match (self.last_mid_x2, obs.midpoint_x2) {
            (Some(a), Some(b)) => b - a,
            _ => 0,
        };
        let spread_changed =
            matches!((self.last_spread, obs.spread_ticks), (Some(a), Some(b)) if a != b);
        let (buy, sell) = match obs.print {
            Some((Side::Buy, q)) => (q, 0),
            Some((Side::Sell, q)) => (0, q),
            None => (0, 0),
        };
        let record = move_x2 != 0 || spread_changed || obs.print.is_some();
        // Every failure is detected before any state changes.
        let expiring = (0..self.len)
            .take_while(|i| {
                now.0 - self.entries[(self.head + i) % W].time.0 >= self.config.window_ns
            })
            .count();
        if record && self.len - expiring == W {
            return Err(EnvironmentError::Capacity);
        }
        let elapsed = self.time_in_state[self.state as usize]
            .checked_add(now.0 - self.now.0)
            .ok_or(EnvironmentError::Overflow)?;
        self.time_in_state[self.state as usize] = elapsed;
        self.now = now;
        self.expire(now);
        self.last_mid_x2 = obs.midpoint_x2;
        self.last_spread = obs.spread_ticks;
        if record {
            let entry = Entry {
                time: now,
                move_x2,
                spread_changed,
                buy,
                sell,
                print: obs.print.is_some(),
            };
            self.entries[(self.head + self.len) % W] = entry;
            self.len += 1;
            self.totals.accumulate(entry, 1);
        }
        let next = self.classify(obs);
        if next != self.state {
            self.state = next;
            self.since = now;
            self.transitions += 1;
        }
        Ok(next)
    }
    fn classify(&self, obs: Observation) -> Environment {
        let c = self.config;
        let t = self.totals;
        let net = t.net_x2.abs();
        let aggressive = t.buy_qty + t.sell_qty;
        if obs.midpoint_x2.is_none()
            || obs.spread_ticks.is_none_or(|s| s > c.shock_spread_ticks)
            || obs.top_qty < c.shock_min_top_qty
        {
            Environment::LiquidityShock
        } else if t.spread_changes >= c.chaotic_spread_changes {
            Environment::Chaotic
        } else if (net >= 2 * i128::from(c.trend_min_move_ticks)
            && net * PPM >= i128::from(c.trend_efficiency_ppm) * t.path_x2)
            || (aggressive >= c.trend_min_aggressive_qty
                && (t.buy_qty - t.sell_qty).abs() * PPM
                    >= i128::from(c.trend_aggression_ppm) * aggressive)
        {
            Environment::Trending
        } else if t.moves < c.active_min_moves && t.prints < c.active_min_prints {
            Environment::Dead
        } else {
            Environment::BalancedActive
        }
    }
}
/// Exposed components; the optional score is an explicit weighted mean, never a decision input.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ToxicityComponents {
    /// (buy - sell) / (buy + sell) aggressive quantity; `None` without prints.
    pub aggression_imbalance_ppm: Option<i32>,
    /// Removed / (added + removed) displayed quantity; `None` without depth changes.
    pub depletion_ppm: Option<u32>,
    /// Cancelled / removed quantity; `None` under unknown removal attribution.
    pub cancellation_ppm: Option<u32>,
    pub spread_ticks: Option<i64>,
    pub spread_changes: u32,
}
fn ratio_ppm(numerator: i128, denominator: i128) -> Option<i128> {
    (denominator > 0).then(|| numerator * PPM / denominator)
}
impl ToxicityComponents {
    /// `cancelled` is `None` when removals cannot be attributed to cancellations.
    pub fn new(
        totals: WindowTotals,
        spread_ticks: Option<i64>,
        added: i128,
        removed: i128,
        cancelled: Option<i128>,
    ) -> Result<Self, EnvironmentError> {
        if added < 0 || removed < 0 || cancelled.is_some_and(|c| c < 0 || c > removed) {
            return Err(EnvironmentError::InvalidObservation);
        }
        Ok(Self {
            aggression_imbalance_ppm: ratio_ppm(
                totals.buy_qty - totals.sell_qty,
                totals.buy_qty + totals.sell_qty,
            )
            .map(|x| x as i32),
            depletion_ppm: ratio_ppm(removed, added + removed).map(|x| x as u32),
            cancellation_ppm: cancelled.and_then(|c| ratio_ppm(c, removed).map(|x| x as u32)),
            spread_ticks,
            spread_changes: totals.spread_changes,
        })
    }
    /// Weighted mean of |aggression|, depletion and cancellation over AVAILABLE components.
    pub fn score_ppm(self, weights: [u32; 3]) -> Option<u32> {
        let parts = [
            self.aggression_imbalance_ppm.map(|x| x.unsigned_abs()),
            self.depletion_ppm,
            self.cancellation_ppm,
        ];
        let (mut sum, mut weight) = (0_u128, 0_u128);
        for (part, w) in parts.into_iter().zip(weights) {
            if let Some(p) = part {
                sum += u128::from(p) * u128::from(w);
                weight += u128::from(w);
            }
        }
        (weight > 0).then(|| (sum / weight) as u32)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> EnvironmentConfig {
        EnvironmentConfig {
            window_ns: 100,
            shock_spread_ticks: 6,
            shock_min_top_qty: 10,
            chaotic_spread_changes: 4,
            trend_min_move_ticks: 3,
            trend_efficiency_ppm: 700_000,
            trend_min_aggressive_qty: 1_000,
            trend_aggression_ppm: 800_000,
            active_min_moves: 2,
            active_min_prints: 2,
        }
    }
    fn obs(mid: i128, spread: i64) -> Observation {
        Observation {
            midpoint_x2: Some(mid * 2),
            spread_ticks: Some(spread),
            top_qty: 200,
            print: None,
        }
    }
    #[test]
    fn precedence_and_definitions() {
        let mut t = EnvironmentTracker::<64>::new(config()).unwrap();
        assert_eq!(
            t.update(Timestamp(1), obs(100, 2)).unwrap(),
            Environment::Dead
        );
        // Oscillation: many moves, little displacement.
        for (i, m) in [101, 100, 101, 100, 101].into_iter().enumerate() {
            t.update(Timestamp(2 + i as u64), obs(m, 2)).unwrap();
        }
        assert_eq!(t.state(), Environment::BalancedActive);
        // The same run inside the window is diluted by the oscillation: 12/20 < 70%.
        for (i, m) in (102..=106).enumerate() {
            t.update(Timestamp(10 + i as u64), obs(m, 2)).unwrap();
        }
        assert_eq!(t.state(), Environment::BalancedActive);
        // Once the oscillation ages out, a monotone run has |net| = path.
        for (i, m) in (107..=111).enumerate() {
            t.update(Timestamp(200 + i as u64), obs(m, 2)).unwrap();
        }
        assert_eq!(t.state(), Environment::Trending);
        // Wide spread overrides everything.
        t.update(Timestamp(210), obs(111, 7)).unwrap();
        assert_eq!(t.state(), Environment::LiquidityShock);
        let mut closed = obs(111, 2);
        closed.midpoint_x2 = None;
        assert_eq!(
            t.update(Timestamp(211), closed).unwrap(),
            Environment::LiquidityShock
        );
        // Window expiry returns to dead once moves age out; spread flapping is chaotic.
        let mut t = EnvironmentTracker::<64>::new(config()).unwrap();
        for (i, s) in [2, 3, 2, 3, 2].into_iter().enumerate() {
            t.update(Timestamp(i as u64), obs(100, s)).unwrap();
        }
        assert_eq!(t.state(), Environment::Chaotic);
        assert_eq!(
            t.update(Timestamp(200), obs(100, 2)).unwrap(),
            Environment::Dead
        );
        assert_eq!(t.totals(), WindowTotals::default());
    }
    #[test]
    fn one_sided_aggression_is_trending_and_components_are_exposed() {
        let mut t = EnvironmentTracker::<64>::new(config()).unwrap();
        for i in 0..5 {
            let mut o = obs(100 + i % 2, 2);
            o.print = Some((Side::Sell, 300));
            t.update(Timestamp(i as u64), o).unwrap();
        }
        assert_eq!(t.state(), Environment::Trending);
        let c = ToxicityComponents::new(t.totals(), Some(2), 300, 100, None).unwrap();
        assert_eq!(c.aggression_imbalance_ppm, Some(-1_000_000));
        assert_eq!(c.depletion_ppm, Some(250_000));
        assert_eq!(c.cancellation_ppm, None);
        assert_eq!(c.score_ppm([1, 1, 1]), Some(625_000));
        assert_eq!(ToxicityComponents::default().score_ppm([1, 1, 1]), None);
        assert!(ToxicityComponents::new(t.totals(), None, 0, 1, Some(2)).is_err());
    }
    #[test]
    fn capacity_clock_and_time_accounting_are_explicit() {
        let mut t = EnvironmentTracker::<2>::new(config()).unwrap();
        t.update(Timestamp(0), obs(100, 2)).unwrap();
        t.update(Timestamp(1), obs(101, 2)).unwrap();
        t.update(Timestamp(2), obs(100, 2)).unwrap();
        let before = t.clone();
        assert_eq!(
            t.update(Timestamp(3), obs(101, 2)),
            Err(EnvironmentError::Capacity)
        );
        assert_eq!(t, before);
        let mut t = EnvironmentTracker::<8>::new(config()).unwrap();
        t.update(Timestamp(10), obs(100, 2)).unwrap();
        t.update(Timestamp(30), obs(100, 2)).unwrap();
        assert_eq!(
            t.update(Timestamp(29), obs(100, 2)),
            Err(EnvironmentError::ClockRegression)
        );
        // Initial state is shock until a first observation; then 20 ns of Dead.
        assert_eq!(t.time_in_state()[Environment::LiquidityShock as usize], 10);
        assert_eq!(t.time_in_state()[Environment::Dead as usize], 20);
        let mut bad = config();
        bad.trend_efficiency_ppm = 0;
        assert!(EnvironmentTracker::<8>::new(bad).is_err());
    }
}
