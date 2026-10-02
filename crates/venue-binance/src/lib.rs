//! Binance USDⓈ-M futures public adapter (decoding and sync rules only; no network here).
//!
//! Depth uses the partial-book stream `<symbol>@depth<K>@100ms`: every message is a COMPLETE
//! top-K snapshot carrying `U`/`u`/`pu`. Each one replaces the native window, so a missed
//! message loses intermediate changes but never corrupts the book. Rule: `u` must strictly
//! increase (older or repeated messages are dropped); `pu != previous u` is counted as skipped.
//! Trades use `<symbol>@aggTrade`: `m = true` means the buyer was maker, so the aggressor sold.
//! Endpoints are routed: depth on `/public`, aggregate trades on `/market`.
use common::Side;
use wire::{
    Scanner,
    feed::{
        DecodeError, Decoded, DecoderStats, Drop, MS, NativeDecoder, Scale, Updates, push_trade,
    },
    window::{FrameKind, NativeWindow, WireFrame},
};
pub const VENUE_NAME: &str = "binance";
pub const DEPTH_HOST: &str = "wss://fstream.binance.com/public/ws/";
pub const TRADE_HOST: &str = "wss://fstream.binance.com/market/ws/";
pub fn depth_url(stream_symbol: &str, levels: usize) -> String {
    format!("{DEPTH_HOST}{stream_symbol}@depth{levels}@100ms")
}
pub fn trade_url(stream_symbol: &str) -> String {
    format!("{TRADE_HOST}{stream_symbol}@aggTrade")
}
pub struct BinanceFutures<const W: usize> {
    symbol: &'static [u8],
    scale: Scale,
    window: NativeWindow<W>,
    updates: Updates,
    last_u: Option<u64>,
    pub stats: DecoderStats,
}
impl<const W: usize> BinanceFutures<W> {
    /// `symbol` is the exchange symbol as it appears in messages, e.g. `BTCUSDT`.
    pub fn new(symbol: &'static str, scale: Scale, window: usize) -> Result<Self, DecodeError> {
        Ok(Self {
            symbol: symbol.as_bytes(),
            scale,
            window: NativeWindow::new(window)?,
            updates: Updates::default(),
            last_u: None,
            stats: DecoderStats::default(),
        })
    }
    /// Called after a reconnect: the next depth message is treated as a fresh start.
    pub fn reset(&mut self) {
        self.window.reset();
        self.last_u = None;
    }
    pub fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        let result = self.decode_inner(text, frame);
        if let Ok(d) = result {
            self.stats.count(d);
        }
        result
    }
    fn decode_inner(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        let mut s = Scanner::new(text);
        s.begin_object()?;
        self.updates.clear();
        let mut event: &[u8] = b"";
        let (mut symbol, mut first, mut last, mut previous) = (&b""[..], None, None, None);
        let (mut time, mut agg_id, mut price, mut qty, mut maker) = (0, 0, 0, 0, None);
        let mut enveloped = false;
        while let Some(key) = s.key()? {
            match key {
                // Combined-stream envelope: descend into the payload.
                b"data" if !enveloped => {
                    s.begin_object()?;
                    enveloped = true;
                }
                b"stream" => {
                    s.string()?;
                }
                b"e" => event = s.string()?,
                b"s" => symbol = s.string()?,
                b"T" => time = s.u64()?,
                b"U" => first = Some(s.u64()?),
                b"u" => last = Some(s.u64()?),
                b"pu" => previous = Some(s.i64()?),
                b"b" => self.updates.parse_levels(&mut s, Side::Buy, self.scale)?,
                // `a` is the ask array in depth messages and the aggregate ID in trades.
                b"a" if s.next_is_array() => {
                    self.updates.parse_levels(&mut s, Side::Sell, self.scale)?
                }
                b"a" => agg_id = s.u64()?,
                b"p" => price = self.scale.price(s.string()?)?,
                b"q" => qty = self.scale.qty(s.string()?)?,
                b"m" => maker = Some(s.boolean()?),
                b"result" | b"id" => {
                    s.skip()?;
                    event = b"control";
                }
                _ => s.skip()?,
            }
        }
        // A combined envelope leaves the outer object open.
        if enveloped {
            while s.key()?.is_some() {
                s.skip()?;
            }
        }
        s.finish()?;
        if event == b"control" {
            return Ok(Decoded::Control);
        }
        if symbol != self.symbol {
            return Err(DecodeError::WrongSymbol);
        }
        match event {
            b"depthUpdate" => {
                let u = last.ok_or(DecodeError::Field("u"))?;
                first.ok_or(DecodeError::Field("U"))?;
                if self.last_u.is_some_and(|l| u <= l) {
                    return Ok(Decoded::Dropped(Drop::StaleSequence));
                }
                if self
                    .last_u
                    .is_some_and(|l| previous != i64::try_from(l).ok())
                {
                    self.stats.skipped_updates += 1;
                }
                self.last_u = Some(u);
                // Complete top-K snapshot: replace the native book, diff against the view.
                self.window.replace();
                frame.exchange_sequence = u;
                frame.exchange_ts_ns = time * MS;
                self.updates.apply(&mut self.window, frame)
            }
            b"aggTrade" => {
                let side = match maker.ok_or(DecodeError::Field("m"))? {
                    true => Side::Sell,
                    false => Side::Buy,
                };
                frame.clear(FrameKind::Trades);
                frame.exchange_sequence = agg_id;
                frame.exchange_ts_ns = time * MS;
                push_trade(frame, side, price, qty)?;
                Ok(Decoded::Frame)
            }
            _ => Err(DecodeError::Field("e")),
        }
    }
}
impl<const W: usize> NativeDecoder for BinanceFutures<W> {
    fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        BinanceFutures::decode(self, text, frame)
    }
    fn reset(&mut self) {
        BinanceFutures::reset(self)
    }
    fn stats(&self) -> DecoderStats {
        self.stats
    }
    fn refresh(&mut self, frame: &mut WireFrame) -> bool {
        self.window.refresh(frame).unwrap_or(false)
    }
}
