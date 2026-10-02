//! Simulated passive order in an L2 price-level queue (specification §26).
//!
//! The order is NOT in the displayed book; displayed sizes are everyone else's. Model:
//!
//! - Placement joins the BACK of the level: `ahead` = displayed size at the price.
//! - A print at our price consumes `ahead` first; any remainder fills us, and anything beyond
//!   our remaining size went to orders behind us. All of that printed volume (excluding our own
//!   fill, which is not displayed) is remembered as `pending_execution`.
//! - A later decrease of the displayed size is first matched against `pending_execution` (the
//!   depth echo of those prints). Only the unexplained remainder is treated as cancellations,
//!   attributed by the configured `CancelModel`.
//! - A print strictly THROUGH our price fills all remaining size: by price priority, the whole
//!   level, including us, traded before the price moved through it.
//! - `ahead` never exceeds the displayed size; a vanished level leaves nobody ahead.
//!
//! Ledger children are one quantum, so a fill completes only when `filled == size`; partial
//! progress is tracked here but never booked (see Phase 9 contracts for the bias this implies).
//! Cross-stream ordering matters: if a venue publishes the depth decrease before the print,
//! the decrease is first treated as a cancellation and the print then consumes `ahead` again.
//! The pessimistic model is immune to that optimistic double count.
use common::Side;
/// Where unexplained displayed-size decreases at our price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelModel {
    /// All cancellations were behind us: `ahead` only shrinks by prints (lower bound on fills).
    Pessimistic,
    /// Cancellations were spread evenly through the level: `ahead` shrinks in proportion.
    Proportional,
    /// All cancellations were ahead of us (upper bound on fills).
    Optimistic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedOrder {
    pub side: Side,
    pub price: i64,
    pub size: i64,
    /// Displayed quantity ahead of us at our price.
    pub ahead: i64,
    pub filled: i64,
    /// Printed volume at our price whose depth echo has not been seen yet.
    pub pending_execution: i64,
    /// Displayed size at our price when placed (queue position at entry).
    pub ahead_at_entry: i64,
}
fn aggressor_hits(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}
impl QueuedOrder {
    /// `displayed` is everyone else's size at `price` on our side when we join.
    pub fn place(side: Side, price: i64, size: i64, displayed: i64) -> Self {
        let ahead = displayed.max(0);
        Self {
            side,
            price,
            size,
            ahead,
            filled: 0,
            pending_execution: 0,
            ahead_at_entry: ahead,
        }
    }
    pub fn remaining(&self) -> i64 {
        self.size - self.filled
    }
    pub fn complete(&self) -> bool {
        self.filled >= self.size
    }
    /// A trade print; returns the quantity it filled for us.
    pub fn on_trade(&mut self, aggressor: Side, price: i64, qty: i64) -> i64 {
        if aggressor != aggressor_hits(self.side) || qty <= 0 || self.complete() {
            return 0;
        }
        let through = match self.side {
            Side::Buy => price < self.price,
            Side::Sell => price > self.price,
        };
        if through {
            let fill = self.remaining();
            self.filled = self.size;
            self.ahead = 0;
            return fill;
        }
        if price != self.price {
            return 0;
        }
        let consumed = qty.min(self.ahead);
        self.ahead -= consumed;
        let fill = (qty - consumed).min(self.remaining());
        self.filled += fill;
        // Everything printed except our own fill will disappear from the displayed level.
        self.pending_execution += qty - fill;
        fill
    }
    /// The displayed size at our price changed from `old` to `new`.
    pub fn on_depth(&mut self, old: i64, new: i64, model: CancelModel) {
        let (old, new) = (old.max(0), new.max(0));
        if new < old {
            let decrease = old - new;
            let explained = decrease.min(self.pending_execution);
            self.pending_execution -= explained;
            let cancels = decrease - explained;
            if cancels > 0 {
                let before = old - explained;
                self.ahead = match model {
                    CancelModel::Pessimistic => self.ahead,
                    CancelModel::Optimistic => (self.ahead - cancels).max(0),
                    CancelModel::Proportional if before > 0 => {
                        let removed = (i128::from(self.ahead) * i128::from(cancels)
                            / i128::from(before)) as i64;
                        self.ahead - removed
                    }
                    CancelModel::Proportional => self.ahead,
                };
            }
        }
        // Orders that join after us queue behind; a level can never hold more ahead than shown.
        self.ahead = self.ahead.min(new);
        if new == 0 {
            self.pending_execution = 0;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prints_consume_the_queue_ahead_before_filling_us() {
        let mut o = QueuedOrder::place(Side::Buy, 100, 10, 30);
        // Aggressive buys never hit a resting bid.
        assert_eq!(o.on_trade(Side::Buy, 100, 50), 0);
        assert_eq!(o.on_trade(Side::Sell, 100, 25), 0);
        assert_eq!((o.ahead, o.pending_execution), (5, 25));
        // The depth echo of those prints is not a cancellation.
        o.on_depth(30, 5, CancelModel::Optimistic);
        assert_eq!((o.ahead, o.pending_execution), (5, 0));
        assert_eq!(o.on_trade(Side::Sell, 100, 8), 3);
        assert_eq!((o.ahead, o.filled, o.pending_execution), (0, 3, 5));
        assert_eq!(o.on_trade(Side::Sell, 100, 20), 7);
        assert!(o.complete());
        assert_eq!(o.pending_execution, 18);
        assert_eq!(o.on_trade(Side::Sell, 99, 1), 0);
    }
    #[test]
    fn a_print_through_our_price_fills_everything() {
        let mut o = QueuedOrder::place(Side::Sell, 101, 10, 500);
        assert_eq!(o.on_trade(Side::Buy, 101, 3), 0);
        assert_eq!(o.on_trade(Side::Buy, 102, 1), 10);
        assert!(o.complete() && o.ahead == 0);
    }
    #[test]
    fn cancel_models_bound_the_queue_position() {
        let cases = [
            (CancelModel::Pessimistic, 40),
            (CancelModel::Proportional, 20),
            (CancelModel::Optimistic, 0),
        ];
        for (model, ahead) in cases {
            // 40 ahead of us in a level of 80; 40 is cancelled with no prints.
            let mut o = QueuedOrder::place(Side::Buy, 100, 10, 40);
            o.on_depth(40, 80, model); // others join behind us
            assert_eq!(o.ahead, 40);
            o.on_depth(80, 40, model);
            assert_eq!(o.ahead, ahead, "{model:?}");
        }
        // Even the pessimistic model cannot keep more ahead than is displayed.
        let mut o = QueuedOrder::place(Side::Buy, 100, 10, 40);
        o.on_depth(40, 15, CancelModel::Pessimistic);
        assert_eq!(o.ahead, 15);
        o.on_depth(15, 0, CancelModel::Pessimistic);
        assert_eq!(o.ahead, 0);
    }
    /// Generated sequences: the models stay ordered (optimistic fills earliest), `ahead` stays
    /// within [0, displayed], and fills never exceed size.
    #[test]
    fn generated_sequences_keep_models_ordered_and_bounded() {
        let mut state = 9_u64;
        let mut draw = |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        for _ in 0..2_000 {
            let start = draw(200) as i64;
            let mut orders = [
                CancelModel::Pessimistic,
                CancelModel::Proportional,
                CancelModel::Optimistic,
            ]
            .map(|m| (m, QueuedOrder::place(Side::Buy, 100, 10, start)));
            let mut displayed = start;
            let mut completed_at = [None; 3];
            for step in 0..60 {
                match draw(3) {
                    0 => {
                        let qty = 1 + draw(40) as i64;
                        let price = if draw(10) == 0 { 99 } else { 100 };
                        for (i, (_, o)) in orders.iter_mut().enumerate() {
                            o.on_trade(Side::Sell, price, qty);
                            if o.complete() && completed_at[i].is_none() {
                                completed_at[i] = Some(step);
                            }
                        }
                    }
                    1 => {
                        let new = (displayed - draw(60) as i64).max(0);
                        for (m, o) in orders.iter_mut() {
                            o.on_depth(displayed, new, *m);
                        }
                        displayed = new;
                    }
                    _ => {
                        let new = displayed + draw(60) as i64;
                        for (m, o) in orders.iter_mut() {
                            o.on_depth(displayed, new, *m);
                        }
                        displayed = new;
                    }
                }
                for (_, o) in &orders {
                    assert!((0..=displayed.max(o.ahead_at_entry)).contains(&o.ahead));
                    assert!(o.ahead <= displayed || o.complete());
                    assert!(o.filled <= o.size);
                }
                let [p, q, o] = [orders[0].1, orders[1].1, orders[2].1];
                assert!(o.ahead <= q.ahead && q.ahead <= p.ahead);
                assert!(o.filled >= q.filled && q.filled >= p.filled);
            }
        }
    }
}
