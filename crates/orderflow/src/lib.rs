//! Deterministic flow measurements. Displayed depth reductions are not identified cancellations.
#![forbid(unsafe_code)]
pub mod research;
pub mod window;
use book::Level;
use common::Side;
pub use window::{FlowConfig, FlowEngine, FlowSnapshot};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowError {
    InvalidConfig,
    InvalidObservation,
    ClockRegression,
    Capacity,
    Overflow,
    Faulted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalAttribution {
    Unknown,
    AssumeCancellation,
}
/// Integer ppm, undefined when both queues are empty.
pub fn queue_imbalance(bid: i128, ask: i128) -> Result<Option<i32>, FlowError> {
    if bid < 0 || ask < 0 {
        return Err(FlowError::InvalidObservation);
    }
    let total = bid.checked_add(ask).ok_or(FlowError::Overflow)?;
    if total == 0 {
        return Ok(None);
    }
    let numerator = (bid - ask)
        .checked_mul(1_000_000)
        .ok_or(FlowError::Overflow)?;
    Ok(Some((numerator / total) as i32))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BestQuotes {
    pub bid: Level,
    pub ask: Level,
}
impl BestQuotes {
    fn valid(self) -> bool {
        self.bid.price.0 > 0
            && self.bid.price < self.ask.price
            && self.bid.qty.0 > 0
            && self.ask.qty.0 > 0
    }
}
/// Cont–Kukanov–Stoikov best-quote contribution, in the supplied quantity units.
/// Price improvements use new size; deteriorations use removed old size.
pub fn best_quote_ofi(previous: BestQuotes, current: BestQuotes) -> Result<i128, FlowError> {
    if !previous.valid() || !current.valid() {
        return Err(FlowError::InvalidObservation);
    }
    let mut value = 0;
    if current.bid.price >= previous.bid.price {
        value += i128::from(current.bid.qty.0);
    }
    if current.bid.price <= previous.bid.price {
        value -= i128::from(previous.bid.qty.0);
    }
    if current.ask.price <= previous.ask.price {
        value -= i128::from(current.ask.qty.0);
    }
    if current.ask.price >= previous.ask.price {
        value += i128::from(previous.ask.qty.0);
    }
    Ok(value)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Metric {
    Ofi,
    DepthDelta,
    AddedQty,
    RemovedQty,
    BuyQty,
    SellQty,
    AddEvents,
    RemovalEvents,
    BuyEvents,
    SellEvents,
    CancelQty,
    CancelEvents,
}
const METRICS: usize = 12;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlowTotals {
    values: [i128; METRICS],
}
impl FlowTotals {
    pub fn get(self, metric: Metric) -> i128 {
        self.values[metric as usize]
    }
    pub(crate) fn accumulate(&mut self, other: Self, sign: i128) {
        for (dst, src) in self.values.iter_mut().zip(other.values) {
            *dst += sign * src;
        }
    }
    /// Rate in micro-units per second. Counts become micro-events/second.
    pub fn rate(self, metric: Metric, exposure_ns: u64) -> Result<Option<i128>, FlowError> {
        if exposure_ns == 0 {
            return Ok(None);
        }
        Ok(Some(
            self.get(metric)
                .checked_mul(1_000_000_000_000_000)
                .ok_or(FlowError::Overflow)?
                / i128::from(exposure_ns),
        ))
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Contribution {
    totals: FlowTotals,
    attribution: Option<RemovalAttribution>,
}
impl Contribution {
    pub fn totals(self) -> FlowTotals {
        self.totals
    }
    pub fn depth(
        ofi: i128,
        side: Side,
        delta: i128,
        attribution: RemovalAttribution,
    ) -> Result<Self, FlowError> {
        let bound = i128::from(i64::MAX);
        if !(-bound..=bound).contains(&delta) || !(-4 * bound..=4 * bound).contains(&ofi) {
            return Err(FlowError::InvalidObservation);
        }
        let mut t = FlowTotals::default();
        t.values[Metric::Ofi as usize] = ofi;
        t.values[Metric::DepthDelta as usize] = if side == Side::Buy { delta } else { -delta };
        if delta > 0 {
            t.values[Metric::AddedQty as usize] = delta;
            t.values[Metric::AddEvents as usize] = 1;
        } else if delta < 0 {
            t.values[Metric::RemovedQty as usize] = -delta;
            t.values[Metric::RemovalEvents as usize] = 1;
            if attribution == RemovalAttribution::AssumeCancellation {
                t.values[Metric::CancelQty as usize] = -delta;
                t.values[Metric::CancelEvents as usize] = 1;
            }
        }
        Ok(Self {
            totals: t,
            attribution: Some(attribution),
        })
    }
    pub fn trade(side: Side, quantity: i64) -> Result<Self, FlowError> {
        if quantity <= 0 {
            return Err(FlowError::InvalidObservation);
        }
        let mut t = FlowTotals::default();
        let (q, n) = if side == Side::Buy {
            (Metric::BuyQty, Metric::BuyEvents)
        } else {
            (Metric::SellQty, Metric::SellEvents)
        };
        t.values[q as usize] = i128::from(quantity);
        t.values[n as usize] = 1;
        Ok(Self {
            totals: t,
            attribution: None,
        })
    }
}
/// Probability of at least one counted event, NOT queue-fill probability.
/// Fixed-point range reduction + 12 Taylor terms; tested within 2 ppm of exp.
pub fn poisson_event_probability_ppm(rate_micro_per_second: u64, horizon_ns: u64) -> u32 {
    const Q: i128 = 1_000_000_000_000;
    let product = u128::from(rate_micro_per_second) * u128::from(horizon_ns);
    if product >= 20_000_000_000_000_000 {
        return 999_999;
    }
    let mut x = (product / 1000) as i128;
    let mut squares = 0;
    while x > Q / 8 {
        x /= 2;
        squares += 1;
    }
    let (mut term, mut survival) = (Q, Q);
    for n in 1..=12 {
        term = -term * x / (Q * n);
        survival += term;
    }
    for _ in 0..squares {
        survival = survival * survival / Q;
    }
    (((Q - survival).clamp(0, Q) * 1_000_000 / Q) as u32).min(999_999)
}
/// Event-time bucket assignment. Trade callers supply the consumed resting side.
pub fn distance_bucket<const B: usize>(
    edges: &[u32; B],
    midpoint_x2: i128,
    price: i64,
    side: Side,
) -> Option<usize> {
    if midpoint_x2 <= 0 || midpoint_x2 > 2 * i128::from(i64::MAX) || price <= 0 {
        return None;
    }
    let distance = match side {
        Side::Buy => midpoint_x2 - 2 * i128::from(price),
        Side::Sell => 2 * i128::from(price) - midpoint_x2,
    };
    if distance < 0 {
        return None;
    }
    edges
        .iter()
        .position(|&upper| distance * 1_000_000 < i128::from(upper) * midpoint_x2)
}
