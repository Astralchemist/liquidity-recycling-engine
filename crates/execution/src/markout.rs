//! Post-fill markouts (specification §18), in doubled ticks per quantity unit.
//!
//! For a buy at price p, the markout at horizon h is `mid(t + h) - p`; for a sell, `p - mid`.
//! Each horizon is evaluated at the FIRST observation at or after `t + h` (event-time sampling,
//! documented as such), using whatever reference midpoint the caller supplies. A horizon whose
//! sample has no midpoint counts as missing.
//!
//! When the midpoint at the fill is known, the markout splits exactly into the spread captured
//! at the fill (`mid(t) - p` for a buy) plus the side-signed DRIFT `mid(t + h) - mid(t)`. A
//! negative drift is adverse selection proper; the markout itself is the realized spread.
//!
//! Pending fills live in a fixed ring; an overflow drops the oldest pending fill and is
//! counted, never silent. Aggregates are exact integers.
use common::Side;
pub const MAX_HORIZONS: usize = 10;
/// The §18 horizons: 1, 5, 10, 25, 50, 100, 250 and 500 ms, 1 s and 5 s.
pub const SPEC_HORIZONS_NS: [u64; MAX_HORIZONS] = [
    1_000_000,
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    5_000_000_000,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkoutConfig {
    /// Strictly increasing horizons in nanoseconds; unused slots are 0 after `count`.
    pub horizons_ns: [u64; MAX_HORIZONS],
    pub count: usize,
}
impl MarkoutConfig {
    pub fn new(horizons: &[u64]) -> Option<Self> {
        if horizons.is_empty()
            || horizons.len() > MAX_HORIZONS
            || horizons[0] == 0
            || horizons.windows(2).any(|w| w[0] >= w[1])
        {
            return None;
        }
        let mut horizons_ns = [0; MAX_HORIZONS];
        horizons_ns[..horizons.len()].copy_from_slice(horizons);
        Some(Self {
            horizons_ns,
            count: horizons.len(),
        })
    }
    pub fn horizons(&self) -> &[u64] {
        &self.horizons_ns[..self.count]
    }
    pub fn spec() -> Self {
        Self::new(&SPEC_HORIZONS_NS).expect("strictly increasing")
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HorizonStats {
    pub samples: u64,
    pub missing: u64,
    /// Sum of markout_x2 per unit times quantity.
    pub sum_x2: i128,
    /// Sum of squared per-unit markout_x2 (each fill weighted once).
    pub sum_sq_x2: i128,
    pub adverse: u64,
    pub quantity: i128,
    /// Samples whose fill also carried the midpoint at the fill.
    pub drift_samples: u64,
    /// Sum of side-signed midpoint drift_x2 per unit times quantity.
    pub sum_drift_x2: i128,
    pub drift_quantity: i128,
}
impl HorizonStats {
    /// Quantity-weighted mean markout in ticks.
    pub fn mean_ticks(&self) -> Option<f64> {
        (self.quantity > 0).then(|| self.sum_x2 as f64 / self.quantity as f64 / 2.0)
    }
    /// Population standard deviation of per-fill markouts, in ticks.
    pub fn stdev_ticks(&self, per_fill_sum_x2: i128) -> Option<f64> {
        (self.samples > 1).then(|| {
            let n = self.samples as f64;
            let mean = per_fill_sum_x2 as f64 / n;
            ((self.sum_sq_x2 as f64 / n - mean * mean).max(0.0)).sqrt() / 2.0
        })
    }
    pub fn adverse_fraction(&self) -> Option<f64> {
        (self.samples > 0).then(|| self.adverse as f64 / self.samples as f64)
    }
    /// Quantity-weighted mean side-signed drift in ticks (negative: adverse selection).
    pub fn mean_drift_ticks(&self) -> Option<f64> {
        (self.drift_quantity > 0)
            .then(|| self.sum_drift_x2 as f64 / self.drift_quantity as f64 / 2.0)
    }
    /// Adverse selection per unit in doubled ticks, rounded UP, never negative; `None` below
    /// `min_samples` drift samples.
    pub fn adverse_x2_per_unit(&self, min_samples: u64) -> Option<i128> {
        if self.drift_samples < min_samples.max(1) || self.drift_quantity <= 0 {
            return None;
        }
        let n = -self.sum_drift_x2;
        let d = self.drift_quantity;
        Some((n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)).max(0))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    time: u64,
    side: Side,
    price_x2: i128,
    mid_x2: Option<i128>,
    qty: i64,
}
const EMPTY: Pending = Pending {
    time: 0,
    side: Side::Buy,
    price_x2: 0,
    mid_x2: None,
    qty: 0,
};
/// Fills are recorded in time order, so the fills due at one horizon are always a prefix of
/// those not yet evaluated there: one cursor per horizon makes `observe` cost O(horizons +
/// samples due) instead of a scan of every pending fill. A fill is retained until the LARGEST
/// horizon has evaluated it; that cursor is always the smallest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkoutTracker<const C: usize> {
    config: MarkoutConfig,
    ring: [Pending; C],
    /// Absolute index of the oldest retained fill, and one past the newest.
    start: u64,
    end: u64,
    /// Per horizon: absolute index of the next fill to evaluate there.
    cursor: [u64; MAX_HORIZONS],
    stats: [HorizonStats; MAX_HORIZONS],
    /// Sum over fills of the per-unit markout_x2, per horizon (for the standard deviation).
    per_fill: [i128; MAX_HORIZONS],
    pub fills: u64,
    pub overflows: u64,
}
impl<const C: usize> MarkoutTracker<C> {
    pub fn new(config: MarkoutConfig) -> Self {
        assert!(C > 0);
        Self {
            config,
            ring: [EMPTY; C],
            start: 0,
            end: 0,
            cursor: [0; MAX_HORIZONS],
            stats: [HorizonStats::default(); MAX_HORIZONS],
            per_fill: [0; MAX_HORIZONS],
            fills: 0,
            overflows: 0,
        }
    }
    pub fn config(&self) -> MarkoutConfig {
        self.config
    }
    pub fn stats(&self) -> &[HorizonStats] {
        &self.stats[..self.config.count]
    }
    pub fn per_fill_sum_x2(&self, horizon: usize) -> i128 {
        self.per_fill[horizon]
    }
    pub fn pending(&self) -> usize {
        (self.end - self.start) as usize
    }
    fn slot(i: u64) -> usize {
        (i % C as u64) as usize
    }
    /// A passive fill of `qty` units at `price` ticks on `side`, at local time `now`, with the
    /// reference midpoint at the fill when known.
    pub fn record(&mut self, now: u64, side: Side, price: i64, qty: i64, mid_x2: Option<i128>) {
        if qty <= 0 {
            return;
        }
        self.fills += 1;
        if self.pending() == C {
            // Drop the oldest fill; any horizon not yet evaluated for it is lost (counted).
            self.start += 1;
            self.overflows += 1;
            for c in &mut self.cursor[..self.config.count] {
                *c = (*c).max(self.start);
            }
        }
        self.ring[Self::slot(self.end)] = Pending {
            time: now,
            side,
            price_x2: i128::from(price) * 2,
            mid_x2,
            qty,
        };
        self.end += 1;
    }
    /// Evaluates every due (fill, horizon) pair at `now` against `mid_x2`.
    pub fn observe(&mut self, now: u64, mid_x2: Option<i128>) {
        let count = self.config.count;
        for h in 0..count {
            let horizon = self.config.horizons_ns[h];
            while self.cursor[h] < self.end {
                let p = self.ring[Self::slot(self.cursor[h])];
                if now - p.time < horizon {
                    break;
                }
                self.cursor[h] += 1;
                let s = &mut self.stats[h];
                let Some(mid) = mid_x2 else {
                    s.missing += 1;
                    continue;
                };
                let m = match p.side {
                    Side::Buy => mid - p.price_x2,
                    Side::Sell => p.price_x2 - mid,
                };
                s.samples += 1;
                s.sum_x2 += m * i128::from(p.qty);
                s.sum_sq_x2 += m * m;
                s.quantity += i128::from(p.qty);
                s.adverse += u64::from(m < 0);
                self.per_fill[h] += m;
                if let Some(at_fill) = p.mid_x2 {
                    let drift = match p.side {
                        Side::Buy => mid - at_fill,
                        Side::Sell => at_fill - mid,
                    };
                    s.drift_samples += 1;
                    s.sum_drift_x2 += drift * i128::from(p.qty);
                    s.drift_quantity += i128::from(p.qty);
                }
            }
        }
        self.start = self.cursor[count - 1];
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markouts_are_signed_by_side_and_sampled_at_each_horizon() {
        let config = MarkoutConfig::new(&[10, 100]).unwrap();
        let mut t = MarkoutTracker::<4>::new(config);
        t.record(0, Side::Buy, 100, 10, Some(201)); // bought at 100, mid 100.5
        t.record(5, Side::Sell, 101, 10, None); // sold at 101, mid unknown
        t.observe(9, Some(202)); // nothing due
        assert_eq!(t.stats()[0].samples, 0);
        t.observe(15, Some(198)); // mid 99: buy -1 tick, sell +2 ticks
        let h = t.stats()[0];
        assert_eq!((h.samples, h.adverse, h.sum_x2), (2, 1, -20 + 40));
        assert_eq!(h.mean_ticks(), Some(0.5));
        // The buy captured half a tick of spread and then lost 1.5 ticks of drift.
        assert_eq!((h.drift_samples, h.sum_drift_x2), (1, -30));
        assert_eq!(h.mean_drift_ticks(), Some(-1.5));
        assert_eq!(h.adverse_x2_per_unit(1), Some(3));
        assert_eq!(h.adverse_x2_per_unit(2), None);
        t.observe(200, None);
        assert_eq!((t.stats()[1].missing, t.pending()), (2, 0));
        assert!(MarkoutConfig::new(&[10, 10]).is_none());
        assert!(MarkoutConfig::new(&[]).is_none());
    }
    /// The cursor tracker equals the original full-scan algorithm (every pending fill visited on
    /// every observation) on random fills, missing midpoints and ring overflow.
    #[test]
    fn cursors_match_a_full_scan_oracle() {
        let mut state = 11_u64;
        let mut draw = |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        let config = MarkoutConfig::new(&[3, 10, 40, 200]).unwrap();
        for _ in 0..200 {
            let mut fast = MarkoutTracker::<16>::new(config);
            // (time, side, price_x2, mid at fill, qty, next horizon)
            let mut naive: Vec<(u64, Side, i128, Option<i128>, i64, usize)> = Vec::new();
            let mut stats = [HorizonStats::default(); MAX_HORIZONS];
            let (mut now, mut overflows) = (0_u64, 0_u64);
            for _ in 0..400 {
                now += draw(6);
                let mid = (draw(10) > 0).then(|| 2_000 + draw(40) as i128);
                if draw(3) == 0 {
                    let side = if draw(2) == 0 { Side::Buy } else { Side::Sell };
                    let (price, qty) = (990 + draw(20) as i64, 1 + draw(5) as i64);
                    fast.record(now, side, price, qty, mid);
                    if naive.len() == 16 {
                        naive.remove(0);
                        overflows += 1;
                    }
                    naive.push((now, side, i128::from(price) * 2, mid, qty, 0));
                }
                fast.observe(now, mid);
                for f in naive.iter_mut() {
                    while f.5 < config.count && now - f.0 >= config.horizons_ns[f.5] {
                        let s = &mut stats[f.5];
                        f.5 += 1;
                        let Some(m2) = mid else {
                            s.missing += 1;
                            continue;
                        };
                        let m = if f.1 == Side::Buy { m2 - f.2 } else { f.2 - m2 };
                        s.samples += 1;
                        s.sum_x2 += m * i128::from(f.4);
                        s.sum_sq_x2 += m * m;
                        s.quantity += i128::from(f.4);
                        s.adverse += u64::from(m < 0);
                        if let Some(at) = f.3 {
                            let d = if f.1 == Side::Buy { m2 - at } else { at - m2 };
                            s.drift_samples += 1;
                            s.sum_drift_x2 += d * i128::from(f.4);
                            s.drift_quantity += i128::from(f.4);
                        }
                    }
                }
                naive.retain(|f| f.5 < config.count);
                assert_eq!(fast.pending(), naive.len());
            }
            assert_eq!(fast.stats(), &stats[..config.count]);
            assert_eq!(fast.overflows, overflows);
        }
    }
    #[test]
    fn overflow_is_counted_and_order_preserved() {
        let mut t = MarkoutTracker::<2>::new(MarkoutConfig::new(&[10]).unwrap());
        for i in 0..5 {
            t.record(i, Side::Buy, 100, 1, None);
        }
        assert_eq!((t.fills, t.overflows, t.pending()), (5, 3, 2));
        t.observe(100, Some(204));
        assert_eq!((t.stats()[0].samples, t.stats()[0].sum_x2), (2, 8));
        assert_eq!(t.pending(), 0);
    }
}
