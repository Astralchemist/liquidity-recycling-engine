//! Fixed price corridor: per-venue dense depth and weighted Fenwick sums.
use crate::LiquidityError;
use common::{Side, Timestamp};
use fixed_point::PriceTicks;
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fenwick<const P: usize> {
    tree: [i128; P],
}
impl<const P: usize> Fenwick<P> {
    fn new() -> Self {
        Self { tree: [0; P] }
    }
    fn add(&mut self, mut i: usize, delta: i128) {
        while i < P {
            self.tree[i] += delta;
            i |= i + 1;
        }
    }
    fn prefix(&self, mut end: usize) -> i128 {
        let mut sum = 0;
        while end > 0 {
            sum += self.tree[end - 1];
            end &= end - 1;
        }
        sum
    }
    fn range(&self, start: usize, end: usize) -> i128 {
        self.prefix(end) - self.prefix(start)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct VenueDepth<const P: usize> {
    bid: [i64; P],
    ask: [i64; P],
    cells: [i128; P],
}
impl<const P: usize> VenueDepth<P> {
    fn new() -> Self {
        Self {
            bid: [0; P],
            ask: [0; P],
            cells: [0; P],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceGrid<const V: usize, const P: usize> {
    lower: i64,
    width: usize,
    weights: [u32; V],
    venues: [VenueDepth<P>; V],
    bid: Fenwick<P>,
    ask: Fenwick<P>,
    bid_birth: [Option<Timestamp>; P],
    ask_birth: [Option<Timestamp>; P],
    last_time: Option<Timestamp>,
}
impl<const V: usize, const P: usize> PriceGrid<V, P> {
    pub fn new(lower: PriceTicks, width: usize, weights: [u32; V]) -> Result<Self, LiquidityError> {
        if V == 0
            || V > 64
            || P == 0
            || P > 4096
            || width < 2
            || P % width != 0
            || lower.0 <= 0
            || lower.0.checked_add(P as i64 - 1).is_none()
            || weights.iter().any(|&w| w > 1_000_000)
        {
            return Err(LiquidityError::InvalidConfig);
        }
        Ok(Self {
            lower: lower.0,
            width,
            weights,
            venues: std::array::from_fn(|_| VenueDepth::new()),
            bid: Fenwick::new(),
            ask: Fenwick::new(),
            bid_birth: [None; P],
            ask_birth: [None; P],
            last_time: None,
        })
    }
    pub fn lower(&self) -> i64 {
        self.lower
    }
    pub fn upper(&self) -> i64 {
        self.lower + (P as i64 - 1)
    }
    pub fn cell_count(&self) -> usize {
        P / self.width
    }
    pub fn region(&self, cell: usize) -> voids::PriceRegion {
        voids::PriceRegion {
            lower: PriceTicks(self.lower + (cell * self.width) as i64),
            upper: PriceTicks(self.lower + (((cell + 1) * self.width) - 1) as i64),
        }
    }
    pub fn local_depth(&self, venue: usize, cell: usize) -> i128 {
        self.venues[venue].cells[cell]
    }
    pub fn set(
        &mut self,
        venue: usize,
        side: Side,
        price: PriceTicks,
        qty: i64,
        now: Timestamp,
    ) -> Result<bool, LiquidityError> {
        if venue >= V || qty < 0 {
            return Err(LiquidityError::InvalidObservation);
        }
        if self.last_time.is_some_and(|t| now < t) {
            return Err(LiquidityError::ClockRegression);
        }
        self.last_time = Some(now);
        if price.0 < self.lower || price.0 > self.upper() {
            return Ok(false);
        }
        let i = (price.0 - self.lower) as usize;
        let slot = match side {
            Side::Buy => &mut self.venues[venue].bid[i],
            Side::Sell => &mut self.venues[venue].ask[i],
        };
        let delta = i128::from(qty) - i128::from(*slot);
        *slot = qty;
        self.venues[venue].cells[i / self.width] += delta;
        let weighted = delta * i128::from(self.weights[venue]);
        let (tree, birth) = match side {
            Side::Buy => (&mut self.bid, &mut self.bid_birth),
            Side::Sell => (&mut self.ask, &mut self.ask_birth),
        };
        let old = tree.range(i, i + 1);
        tree.add(i, weighted);
        let new = old + weighted;
        if old == 0 && new > 0 {
            birth[i] = Some(now)
        } else if new == 0 {
            birth[i] = None
        }
        Ok(true)
    }
    pub fn depth(&self, side: Side, lower: i128, upper: i128) -> i128 {
        let (start, end) = self.indices(lower, upper);
        if end <= start {
            return 0;
        }
        match side {
            Side::Buy => self.bid.range(start, end),
            Side::Sell => self.ask.range(start, end),
        }
    }
    fn indices(&self, lower: i128, upper: i128) -> (usize, usize) {
        let start = if lower <= i128::from(self.lower) {
            0
        } else if lower > i128::from(self.upper()) {
            P
        } else {
            (lower - i128::from(self.lower)) as usize
        };
        let end = if upper < i128::from(self.lower) {
            0
        } else if upper >= i128::from(self.upper()) {
            P
        } else {
            (upper - i128::from(self.lower) + 1) as usize
        };
        (start, end)
    }
    /// Exact count and sum of observed level ages; averaging is deferred to the caller.
    pub fn age(
        &self,
        side: Side,
        lower: i128,
        upper: i128,
        now: Timestamp,
    ) -> Result<(u32, u128), LiquidityError> {
        if self.last_time.is_some_and(|t| now < t) {
            return Err(LiquidityError::ClockRegression);
        }
        let (start, end) = self.indices(lower, upper);
        let births = match side {
            Side::Buy => &self.bid_birth,
            Side::Sell => &self.ask_birth,
        };
        let (mut n, mut sum) = (0_u32, 0_u128);
        if end > start {
            for t in births[start..end].iter().flatten() {
                n += 1;
                sum += u128::from(now.0 - t.0);
            }
        }
        Ok((n, sum))
    }
}
