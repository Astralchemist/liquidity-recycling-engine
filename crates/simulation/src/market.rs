//! Synthetic three-venue L2 market driven by explicit operations. Not a matching engine:
//! prints are emitted by the operations below, never inferred from resting orders.
//!
//! Each venue holds a contiguous ladder: bids at every price `low..=bid_top`, asks at every
//! price `ask_top..=high`. A sell sweep prints at and removes the best bid; a refill adds
//! liquidity one tick inside the spread. A normal step is one sweep plus one refill, so the
//! spread returns to two ticks with the midpoint level empty.
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
pub const ALL: u8 = 0b111;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub low: i64,
    pub high: i64,
    pub base_qty: i64,
    /// Local clock advance per emitted event.
    pub step_ns: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Band {
    lower: i64,
    upper: i64,
    mask: u8,
    qty: i64,
}
#[derive(Debug, Clone)]
pub struct SyntheticMarket {
    shape: Shape,
    venues: [VenueId; 3],
    instruments: [InstrumentId; 3],
    sequences: [u64; 3],
    bid_top: [i64; 3],
    ask_top: [i64; 3],
    bands: Vec<Band>,
    now: u64,
    events: Vec<MarketEvent>,
}
impl SyntheticMarket {
    /// Starts with a two-tick spread around `mid` on every venue; call `snapshot` first.
    pub fn new(
        shape: Shape,
        venues: [VenueId; 3],
        instruments: [InstrumentId; 3],
        mid: i64,
    ) -> Self {
        assert!(shape.low < mid - 1 && mid + 1 < shape.high && shape.base_qty > 0);
        Self {
            shape,
            venues,
            instruments,
            sequences: [0; 3],
            bid_top: [mid - 1; 3],
            ask_top: [mid + 1; 3],
            bands: Vec::new(),
            now: 0,
            events: Vec::new(),
        }
    }
    pub fn events(self) -> Vec<MarketEvent> {
        self.events
    }
    pub fn mid_x2(&self, v: usize) -> i64 {
        self.bid_top[v] + self.ask_top[v]
    }
    pub fn tops(&self, v: usize) -> (i64, i64) {
        (self.bid_top[v], self.ask_top[v])
    }
    /// Displayed quantity the profile assigns to `price` on venue `v`; the last band wins.
    pub fn qty(&self, v: usize, price: i64) -> i64 {
        self.bands
            .iter()
            .rev()
            .find(|b| b.mask & (1 << v) != 0 && (b.lower..=b.upper).contains(&price))
            .map_or(self.shape.base_qty, |b| b.qty)
    }
    fn emit(&mut self, v: usize, kind: K, side: Side, price: i64, qty: i64) {
        self.sequences[v] += 1;
        self.now += self.shape.step_ns;
        self.events.push(MarketEvent {
            venue: self.venues[v],
            instrument: self.instruments[v],
            sequence: self.sequences[v],
            exchange_sequence: self.sequences[v],
            exchange_ts: self.now,
            receive_ts: Timestamp(self.now),
            event_type: kind,
            side,
            price_ticks: PriceTicks(price),
            qty_units: QtyUnits(qty),
        });
    }
    fn each(mask: u8) -> impl Iterator<Item = usize> {
        (0..3).filter(move |v| mask & (1 << v) != 0)
    }
    pub fn snapshot(&mut self) {
        for v in 0..3 {
            self.emit(v, K::SnapshotStart, Side::Buy, 0, 0);
            for p in (self.shape.low..=self.bid_top[v]).rev() {
                self.emit(v, K::Add, Side::Buy, p, self.qty(v, p));
            }
            for p in self.ask_top[v]..=self.shape.high {
                self.emit(v, K::Add, Side::Sell, p, self.qty(v, p));
            }
            self.emit(v, K::SnapshotEnd, Side::Buy, 0, 0);
        }
    }
    /// Changes displayed depth inside `[lower, upper]` on masked venues, emitting modifications.
    pub fn set_band(&mut self, lower: i64, upper: i64, mask: u8, qty: i64) {
        assert!(qty > 0 && lower <= upper);
        self.bands.push(Band {
            lower,
            upper,
            mask,
            qty,
        });
        for v in Self::each(mask) {
            for p in lower..=upper {
                if p <= self.bid_top[v] {
                    self.emit(v, K::Modify, Side::Buy, p, qty);
                } else if p >= self.ask_top[v] {
                    self.emit(v, K::Modify, Side::Sell, p, qty);
                }
            }
        }
    }
    /// Aggressive sells consume `n` bid levels per venue; the spread widens.
    pub fn sell(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                let p = self.bid_top[v];
                assert!(p > self.shape.low, "sweep left the corridor");
                self.emit(v, K::Trade, Side::Sell, p, self.qty(v, p));
                self.emit(v, K::Cancel, Side::Buy, p, 0);
                self.bid_top[v] -= 1;
            }
        }
    }
    pub fn buy(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                let p = self.ask_top[v];
                assert!(p < self.shape.high, "sweep left the corridor");
                self.emit(v, K::Trade, Side::Buy, p, self.qty(v, p));
                self.emit(v, K::Cancel, Side::Sell, p, 0);
                self.ask_top[v] += 1;
            }
        }
    }
    /// Liquidity providers add asks one tick inside the spread, never narrower than two ticks.
    pub fn refill_asks(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                if self.ask_top[v] - self.bid_top[v] > 2 {
                    let p = self.ask_top[v] - 1;
                    self.emit(v, K::Add, Side::Sell, p, self.qty(v, p));
                    self.ask_top[v] = p;
                }
            }
        }
    }
    pub fn refill_bids(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                if self.ask_top[v] - self.bid_top[v] > 2 {
                    let p = self.bid_top[v] + 1;
                    self.emit(v, K::Add, Side::Buy, p, self.qty(v, p));
                    self.bid_top[v] = p;
                }
            }
        }
    }
    /// One-tick midpoint steps with an unchanged two-tick spread; venues move in index order.
    pub fn down(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                self.sell(1 << v, 1);
                self.refill_asks(1 << v, 1);
            }
        }
    }
    pub fn up(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                self.buy(1 << v, 1);
                self.refill_bids(1 << v, 1);
            }
        }
    }
    /// `cycles` round trips of `amplitude` ticks: up first, then back down.
    pub fn oscillate(&mut self, mask: u8, amplitude: usize, cycles: usize) {
        for _ in 0..cycles {
            self.up(mask, amplitude);
            self.down(mask, amplitude);
        }
    }
    /// Zero-change refresh of the deepest bid: keeps feeds fresh without moving anything.
    pub fn idle(&mut self, mask: u8, n: usize) {
        for _ in 0..n {
            for v in Self::each(mask) {
                let p = self.shape.low;
                self.emit(v, K::Modify, Side::Buy, p, self.qty(v, p));
            }
        }
    }
}
