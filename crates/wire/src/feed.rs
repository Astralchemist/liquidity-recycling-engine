//! Shared decoder vocabulary for venue adapters. Decoders never allocate: native levels are
//! parsed into a preallocated update buffer and validated BEFORE the native book changes.
use crate::{
    WireError, decimal,
    window::{Change, FrameKind, NativeWindow, WindowError, WireFrame},
};
use common::Side;
use market_events::MarketEventType;
/// Most native levels one message may carry (OKX snapshots: 400 per side).
pub const MAX_UPDATES: usize = 2048;
/// Native wire scales: decimal places and atoms per integer unit, for prices and sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    pub price_decimals: u32,
    pub price_atoms: i64,
    pub qty_decimals: u32,
    pub qty_atoms: i64,
}
impl Scale {
    pub fn price(self, raw: &[u8]) -> Result<i64, WireError> {
        decimal(raw, self.price_decimals, self.price_atoms)
    }
    pub fn qty(self, raw: &[u8]) -> Result<i64, WireError> {
        decimal(raw, self.qty_decimals, self.qty_atoms)
    }
}
/// What one native message produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// `frame` holds a snapshot, batch or trades to forward.
    Frame,
    /// Valid message that changes nothing visible (outside the window, heartbeat).
    Unchanged,
    /// Subscription acknowledgement, pong or other control traffic.
    Control,
    /// Dropped by a documented rule (duplicate, out-of-order, excluded print).
    Dropped(Drop),
    /// Book continuity lost: forward the Reset frame in `frame`, then reconnect.
    Resync(Resync),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drop {
    Duplicate,
    StaleSequence,
    BeforeSnapshot,
    ExcludedPrint,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resync {
    Gap,
    Crossed,
    OneSided,
    Capacity,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Wire(WireError),
    Window(WindowError),
    /// A required field was missing or had the wrong value.
    Field(&'static str),
    WrongSymbol,
    TooManyLevels,
    /// The venue reported an error (e.g. a rejected subscription).
    Venue,
}
impl From<WireError> for DecodeError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}
impl From<WindowError> for DecodeError {
    fn from(e: WindowError) -> Self {
        Self::Window(e)
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecoderStats {
    pub messages: u64,
    pub frames: u64,
    pub unchanged: u64,
    pub control: u64,
    pub duplicates: u64,
    pub stale: u64,
    pub before_snapshot: u64,
    pub excluded_prints: u64,
    pub resyncs: u64,
    /// Native sequence advanced by more than one between snapshot messages (informational).
    pub skipped_updates: u64,
}
impl DecoderStats {
    pub fn count(&mut self, d: Decoded) {
        self.messages += 1;
        match d {
            Decoded::Frame => self.frames += 1,
            Decoded::Unchanged => self.unchanged += 1,
            Decoded::Control => self.control += 1,
            Decoded::Dropped(Drop::Duplicate) => self.duplicates += 1,
            Decoded::Dropped(Drop::StaleSequence) => self.stale += 1,
            Decoded::Dropped(Drop::BeforeSnapshot) => self.before_snapshot += 1,
            Decoded::Dropped(Drop::ExcludedPrint) => self.excluded_prints += 1,
            Decoded::Resync(_) => self.resyncs += 1,
        }
    }
}
/// Preallocated native levels parsed from one message.
pub struct Updates {
    levels: Box<[(Side, i64, i64)]>,
    len: usize,
}
impl Default for Updates {
    fn default() -> Self {
        Self {
            levels: vec![(Side::Buy, 0, 0); MAX_UPDATES].into_boxed_slice(),
            len: 0,
        }
    }
}
impl Updates {
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn push(&mut self, side: Side, price: i64, qty: i64) -> Result<(), DecodeError> {
        *self
            .levels
            .get_mut(self.len)
            .ok_or(DecodeError::TooManyLevels)? = (side, price, qty);
        self.len += 1;
        Ok(())
    }
    /// Parses a JSON array of `[price, size, ...]` string arrays (extra elements skipped).
    pub fn parse_levels(
        &mut self,
        s: &mut crate::Scanner<'_>,
        side: Side,
        scale: Scale,
    ) -> Result<(), DecodeError> {
        s.begin_array()?;
        while s.item()? {
            s.begin_array()?;
            if !s.item()? {
                return Err(DecodeError::Field("level price"));
            }
            let price = scale.price(s.string()?)?;
            if !s.item()? {
                return Err(DecodeError::Field("level size"));
            }
            let qty = scale.qty(s.string()?)?;
            while s.item()? {
                s.skip()?;
            }
            self.push(side, price, qty)?;
        }
        Ok(())
    }
    /// Applies every parsed level, then emits the window change. Window faults become resyncs
    /// with a Reset frame, so the engine withdraws the venue instead of trusting a bad book.
    pub fn apply<const W: usize>(
        &self,
        window: &mut NativeWindow<W>,
        frame: &mut WireFrame,
    ) -> Result<Decoded, DecodeError> {
        let result = (|| {
            for &(side, price, qty) in &self.levels[..self.len] {
                window.set(side, price, qty)?;
            }
            window.emit(frame)
        })();
        match result {
            Ok(true) => Ok(Decoded::Frame),
            Ok(false) => Ok(Decoded::Unchanged),
            Err(e) => {
                let reason = match e {
                    WindowError::Crossed => Resync::Crossed,
                    WindowError::NotLive => Resync::OneSided,
                    WindowError::Capacity => Resync::Capacity,
                    other => return Err(other.into()),
                };
                Ok(resync(window, frame, reason))
            }
        }
    }
}
/// Prints of one message (Bybit sends up to 1024). They leave in chunks of at most
/// `MAX_FRAME`: the first through the decode result, the rest through `drain`.
#[derive(Default)]
pub struct Prints {
    prints: Updates,
    cursor: usize,
    sequence: u64,
    ts_ns: u64,
}
impl Prints {
    pub fn begin(&mut self) {
        self.prints.clear();
        self.cursor = 0;
    }
    pub fn push(
        &mut self,
        side: Side,
        price: i64,
        qty: i64,
        ts_ns: u64,
    ) -> Result<(), DecodeError> {
        if price <= 0 || qty <= 0 {
            return Err(DecodeError::Field("trade price/size"));
        }
        self.ts_ns = ts_ns;
        self.prints.push(side, price, qty)
    }
    pub fn is_empty(&self) -> bool {
        self.prints.is_empty()
    }
    pub fn set_sequence(&mut self, sequence: u64) {
        self.sequence = sequence;
    }
    /// Moves the next chunk into `frame`; false when every print has been emitted.
    pub fn fill(&mut self, frame: &mut WireFrame) -> Result<bool, DecodeError> {
        let total = self.prints.len();
        if self.cursor >= total {
            return Ok(false);
        }
        frame.clear(FrameKind::Trades);
        frame.exchange_sequence = self.sequence;
        frame.exchange_ts_ns = self.ts_ns;
        let end = total.min(self.cursor + crate::window::MAX_FRAME);
        for &(side, price, qty) in &self.prints.levels[self.cursor..end] {
            push_trade(frame, side, price, qty)?;
        }
        self.cursor = end;
        Ok(true)
    }
}
/// Discards the native book and fills `frame` with a Reset.
pub fn resync<const W: usize>(
    window: &mut NativeWindow<W>,
    frame: &mut WireFrame,
    reason: Resync,
) -> Decoded {
    window.reset();
    frame.clear(FrameKind::Reset);
    Decoded::Resync(reason)
}
/// Appends one trade print (aggressor side) to a Trades frame.
pub fn push_trade(
    frame: &mut WireFrame,
    side: Side,
    price: i64,
    qty: i64,
) -> Result<(), DecodeError> {
    if price <= 0 || qty <= 0 {
        return Err(DecodeError::Field("trade price/size"));
    }
    frame.push(Change {
        kind: MarketEventType::Trade,
        side,
        price,
        qty,
    })?;
    Ok(())
}
pub const MS: u64 = 1_000_000;
/// Common interface so one feed loop drives every venue decoder.
pub trait NativeDecoder {
    fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError>;
    /// Forget native state after a reconnect; the next book message must be a snapshot.
    fn reset(&mut self);
    fn stats(&self) -> DecoderStats;
    /// Further frames produced by the last message (large trade bursts); false when done.
    fn drain(&mut self, _frame: &mut WireFrame) -> bool {
        false
    }
    /// A zero-change refresh of the emitted window (see `NativeWindow::refresh`).
    fn refresh(&mut self, _frame: &mut WireFrame) -> bool {
        false
    }
}
