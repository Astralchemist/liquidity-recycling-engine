//! Native-to-canonical conversion for windowed L2 feeds.
//!
//! A venue adapter applies absolute native updates (size 0 deletes) to a bounded native book,
//! then emits the change of its TOP-K view since the last emission as one atomic batch:
//! cancels, then modifies, then adds. The engine therefore holds exactly the top K levels per
//! side, a complete contiguous window, so the Phase 4 coverage contract holds within it.
//! Levels leaving the window appear as removals; this is a window artifact, not a cancel.
use common::Side;
use market_events::MarketEventType;
/// Largest top-K window; frames hold at most `2 * MAX_WINDOW` canonical changes.
pub const MAX_WINDOW: usize = 64;
pub const MAX_FRAME: usize = 2 * MAX_WINDOW;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub price: i64,
    pub qty: i64,
}
/// One canonical change in native units. For trades, `side` is the aggressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub kind: MarketEventType,
    pub side: Side,
    pub price: i64,
    pub qty: i64,
}
const BLANK: Change = Change {
    kind: MarketEventType::Add,
    side: Side::Buy,
    price: 0,
    qty: 0,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Complete top-K view: emit SnapshotStart, the adds, SnapshotEnd.
    Snapshot,
    /// Atomic depth batch.
    Batch,
    /// Independent trade prints sharing one receive time.
    Trades,
    /// The venue's book is no longer valid (gap, protocol fault, disconnect).
    Reset,
}
/// Fixed-size transport unit from a feed thread to the sequencer; never allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireFrame {
    pub venue: u16,
    pub kind: FrameKind,
    pub exchange_sequence: u64,
    pub exchange_ts_ns: u64,
    /// Monotonic local nanoseconds when the socket read returned.
    pub socket_ns: u64,
    /// Monotonic local nanoseconds after decoding and canonicalization.
    pub decoded_ns: u64,
    pub len: usize,
    pub changes: [Change; MAX_FRAME],
}
impl WireFrame {
    pub fn new(venue: u16) -> Self {
        Self {
            venue,
            kind: FrameKind::Batch,
            exchange_sequence: 0,
            exchange_ts_ns: 0,
            socket_ns: 0,
            decoded_ns: 0,
            len: 0,
            changes: [BLANK; MAX_FRAME],
        }
    }
    pub fn changes(&self) -> &[Change] {
        &self.changes[..self.len]
    }
    pub fn clear(&mut self, kind: FrameKind) {
        self.kind = kind;
        self.len = 0;
    }
    pub fn push(&mut self, change: Change) -> Result<(), WindowError> {
        *self
            .changes
            .get_mut(self.len)
            .ok_or(WindowError::FrameFull)? = change;
        self.len += 1;
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    InvalidWindow,
    InvalidLevel,
    Capacity,
    FrameFull,
    Crossed,
    NotLive,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ladder<const W: usize> {
    levels: [Level; W],
    len: usize,
    side: Side,
}
impl<const W: usize> Ladder<W> {
    fn new(side: Side) -> Self {
        Self {
            levels: [Level::default(); W],
            len: 0,
            side,
        }
    }
    /// Better prices first: bids descending, asks ascending.
    fn locate(&self, price: i64) -> Result<usize, usize> {
        self.levels[..self.len].binary_search_by(|l| match self.side {
            Side::Buy => price.cmp(&l.price),
            Side::Sell => l.price.cmp(&price),
        })
    }
    fn set(&mut self, price: i64, qty: i64) -> Result<(), WindowError> {
        if price <= 0 || qty < 0 {
            return Err(WindowError::InvalidLevel);
        }
        match (self.locate(price), qty) {
            (Ok(i), 0) => {
                self.levels.copy_within(i + 1..self.len, i);
                self.len -= 1;
            }
            (Ok(i), q) => self.levels[i].qty = q,
            // Deleting an absent level is a documented no-op (e.g. Binance futures).
            (Err(_), 0) => {}
            (Err(i), q) => {
                if self.len == W {
                    return Err(WindowError::Capacity);
                }
                self.levels.copy_within(i..self.len, i + 1);
                self.levels[i] = Level { price, qty: q };
                self.len += 1;
            }
        }
        Ok(())
    }
    fn top(&self, k: usize) -> &[Level] {
        &self.levels[..self.len.min(k)]
    }
}
/// Bounded native book (W levels per side) plus the top-K view last emitted to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWindow<const W: usize> {
    bids: Ladder<W>,
    asks: Ladder<W>,
    view_bids: [Level; MAX_WINDOW],
    view_asks: [Level; MAX_WINDOW],
    view_len: (usize, usize),
    k: usize,
    live: bool,
}
impl<const W: usize> NativeWindow<W> {
    pub fn new(k: usize) -> Result<Self, WindowError> {
        if k == 0 || k > MAX_WINDOW || k > W {
            return Err(WindowError::InvalidWindow);
        }
        Ok(Self {
            bids: Ladder::new(Side::Buy),
            asks: Ladder::new(Side::Sell),
            view_bids: [Level::default(); MAX_WINDOW],
            view_asks: [Level::default(); MAX_WINDOW],
            view_len: (0, 0),
            k,
            live: false,
        })
    }
    pub fn live(&self) -> bool {
        self.live
    }
    /// Discards native state; the next emission is a snapshot.
    pub fn reset(&mut self) {
        self.bids.len = 0;
        self.asks.len = 0;
        self.view_len = (0, 0);
        self.live = false;
    }
    /// Clears the native book but keeps the emitted view, so a complete native snapshot that
    /// replaces the book still produces a diff against what the engine already holds.
    pub fn replace(&mut self) {
        self.bids.len = 0;
        self.asks.len = 0;
    }
    pub fn set(&mut self, side: Side, price: i64, qty: i64) -> Result<(), WindowError> {
        match side {
            Side::Buy => self.bids.set(price, qty),
            Side::Sell => self.asks.set(price, qty),
        }
    }
    fn crossed(&self) -> bool {
        matches!((self.bids.top(1).first(), self.asks.top(1).first()),
            (Some(b), Some(a)) if b.price >= a.price)
    }
    /// A zero-change refresh: one `Modify` of the best bid at its current size. Feeds send it
    /// when valid native messages leave the window unchanged for a while, so a healthy but
    /// quiet venue is not withdrawn as stale. False before the window is live.
    pub fn refresh(&self, frame: &mut WireFrame) -> Result<bool, WindowError> {
        let Some(best) = self.view_bids[..self.view_len.0].first().copied() else {
            return Ok(false);
        };
        if !self.live {
            return Ok(false);
        }
        frame.clear(FrameKind::Batch);
        frame.push(Change {
            kind: MarketEventType::Modify,
            side: Side::Buy,
            price: best.price,
            qty: best.qty,
        })?;
        Ok(true)
    }
    /// Emits the canonical change of the top-K view into `frame`. Returns false when nothing
    /// changed. The first emission after `reset` is a complete snapshot. A crossed or one-sided
    /// window is refused: the caller must resynchronize.
    pub fn emit(&mut self, frame: &mut WireFrame) -> Result<bool, WindowError> {
        if self.crossed() {
            return Err(WindowError::Crossed);
        }
        let (bids, asks) = (self.bids.top(self.k), self.asks.top(self.k));
        if bids.is_empty() || asks.is_empty() {
            return Err(WindowError::NotLive);
        }
        if !self.live {
            frame.clear(FrameKind::Snapshot);
            for (side, levels) in [(Side::Buy, bids), (Side::Sell, asks)] {
                for l in levels {
                    frame.push(Change {
                        kind: MarketEventType::Add,
                        side,
                        price: l.price,
                        qty: l.qty,
                    })?;
                }
            }
        } else {
            frame.clear(FrameKind::Batch);
            for pass in [
                MarketEventType::Cancel,
                MarketEventType::Modify,
                MarketEventType::Add,
            ] {
                diff(
                    Side::Buy,
                    &self.view_bids[..self.view_len.0],
                    bids,
                    pass,
                    frame,
                )?;
                diff(
                    Side::Sell,
                    &self.view_asks[..self.view_len.1],
                    asks,
                    pass,
                    frame,
                )?;
            }
        }
        self.view_bids[..bids.len()].copy_from_slice(bids);
        self.view_asks[..asks.len()].copy_from_slice(asks);
        self.view_len = (bids.len(), asks.len());
        let changed = !self.live || frame.len > 0;
        self.live = true;
        Ok(changed)
    }
}
/// One pass of the sorted merge of old and new views, pushing changes of kind `pass`.
fn diff(
    side: Side,
    old: &[Level],
    new: &[Level],
    pass: MarketEventType,
    frame: &mut WireFrame,
) -> Result<(), WindowError> {
    let better = |a: i64, b: i64| match side {
        Side::Buy => a > b,
        Side::Sell => a < b,
    };
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        let change = match (old.get(i), new.get(j)) {
            (Some(o), Some(n)) if o.price == n.price => {
                i += 1;
                j += 1;
                (o.qty != n.qty).then_some((MarketEventType::Modify, n.price, n.qty))
            }
            (Some(o), Some(n)) if better(o.price, n.price) => {
                i += 1;
                Some((MarketEventType::Cancel, o.price, 0))
            }
            (Some(o), None) => {
                i += 1;
                Some((MarketEventType::Cancel, o.price, 0))
            }
            (_, Some(n)) => {
                j += 1;
                Some((MarketEventType::Add, n.price, n.qty))
            }
            (None, None) => None,
        };
        if let Some((kind, price, qty)) = change.filter(|c| c.0 == pass) {
            frame.push(Change {
                kind,
                side,
                price,
                qty,
            })?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use MarketEventType::*;
    fn view(w: &NativeWindow<16>) -> (Vec<Level>, Vec<Level>) {
        (
            w.view_bids[..w.view_len.0].to_vec(),
            w.view_asks[..w.view_len.1].to_vec(),
        )
    }
    #[test]
    fn snapshot_then_ordered_window_diffs() {
        let mut w = NativeWindow::<16>::new(2).unwrap();
        let mut f = WireFrame::new(1);
        for (side, p, q) in [
            (Side::Buy, 99, 5),
            (Side::Buy, 98, 6),
            (Side::Buy, 97, 7),
            (Side::Sell, 101, 1),
        ] {
            w.set(side, p, q).unwrap();
        }
        assert!(w.emit(&mut f).unwrap());
        assert_eq!(f.kind, FrameKind::Snapshot);
        assert_eq!(f.len, 3);
        // Best bid removed: 97 enters the two-level window; an ask is added and one resized.
        w.set(Side::Buy, 99, 0).unwrap();
        w.set(Side::Sell, 102, 4).unwrap();
        w.set(Side::Sell, 101, 3).unwrap();
        assert!(w.emit(&mut f).unwrap());
        assert_eq!(f.kind, FrameKind::Batch);
        let got: Vec<_> = f
            .changes()
            .iter()
            .map(|c| (c.kind, c.side, c.price, c.qty))
            .collect();
        assert_eq!(
            got,
            [
                (Cancel, Side::Buy, 99, 0),
                (Modify, Side::Sell, 101, 3),
                (Add, Side::Buy, 97, 7),
                (Add, Side::Sell, 102, 4),
            ]
        );
        assert_eq!(
            view(&w).0,
            [Level { price: 98, qty: 6 }, Level { price: 97, qty: 7 }]
        );
        // Changes outside the window, and deletes of absent levels, emit nothing.
        w.set(Side::Buy, 90, 1).unwrap();
        w.set(Side::Buy, 50, 0).unwrap();
        assert!(!w.emit(&mut f).unwrap());
        // A refresh re-states the best bid without changing anything.
        assert!(w.refresh(&mut f).unwrap());
        let got: Vec<_> = f
            .changes()
            .iter()
            .map(|c| (c.kind, c.side, c.price, c.qty))
            .collect();
        assert_eq!(
            (f.kind, got),
            (FrameKind::Batch, vec![(Modify, Side::Buy, 98, 6)])
        );
        w.reset();
        assert!(!w.refresh(&mut f).unwrap());
    }
    #[test]
    fn crossed_one_sided_and_capacity_faults_are_explicit() {
        let mut w = NativeWindow::<2>::new(2).unwrap();
        let mut f = WireFrame::new(1);
        w.set(Side::Buy, 100, 1).unwrap();
        assert_eq!(w.emit(&mut f), Err(WindowError::NotLive));
        w.set(Side::Sell, 100, 1).unwrap();
        assert_eq!(w.emit(&mut f), Err(WindowError::Crossed));
        w.set(Side::Buy, 99, 1).unwrap();
        assert_eq!(w.set(Side::Buy, 98, 1), Err(WindowError::Capacity));
        assert_eq!(w.set(Side::Buy, 0, 1), Err(WindowError::InvalidLevel));
        assert!(NativeWindow::<2>::new(3).is_err());
        assert!(NativeWindow::<128>::new(MAX_WINDOW + 1).is_err());
        w.reset();
        assert!(!w.live());
    }
    /// Applying every emitted batch to a mirror reproduces the window's top-K exactly.
    #[test]
    fn generated_updates_keep_a_mirror_equal_to_the_top_k_view() {
        let mut w = NativeWindow::<64>::new(5).unwrap();
        let mut f = WireFrame::new(1);
        let mut mirror: (Vec<Level>, Vec<Level>) = (Vec::new(), Vec::new());
        let mut state = 5_u64;
        let mut draw = |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        for p in 90..100 {
            w.set(Side::Buy, p, 10).unwrap();
            w.set(Side::Sell, p + 16, 10).unwrap();
        }
        for step in 0..5_000 {
            let side = if draw(2) == 0 { Side::Buy } else { Side::Sell };
            let price = match side {
                Side::Buy => 80 + draw(25) as i64,
                Side::Sell => 105 + draw(25) as i64,
            };
            w.set(
                side,
                price,
                if draw(4) == 0 { 0 } else { 1 + draw(9) as i64 },
            )
            .unwrap();
            match w.emit(&mut f) {
                Ok(_) => {}
                Err(WindowError::NotLive) => continue,
                Err(e) => panic!("{e:?}"),
            }
            if f.kind == FrameKind::Snapshot {
                mirror = (Vec::new(), Vec::new());
            }
            for c in f.changes() {
                let ladder = if c.side == Side::Buy {
                    &mut mirror.0
                } else {
                    &mut mirror.1
                };
                match c.kind {
                    Cancel => ladder.retain(|l| l.price != c.price),
                    Modify => ladder.iter_mut().find(|l| l.price == c.price).unwrap().qty = c.qty,
                    _ => ladder.push(Level {
                        price: c.price,
                        qty: c.qty,
                    }),
                }
            }
            mirror.0.sort_by_key(|l| std::cmp::Reverse(l.price));
            mirror.1.sort_by_key(|l| l.price);
            let v = (
                w.view_bids[..w.view_len.0].to_vec(),
                w.view_asks[..w.view_len.1].to_vec(),
            );
            assert_eq!(mirror, v, "step {step}");
        }
    }
}
