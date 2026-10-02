//! Bybit V5 linear public adapter (decoding and sync rules only; no network here).
//!
//! Depth uses `orderbook.<D>.<SYMBOL>`: a snapshot on subscribe (or after a service restart,
//! `u = 1`), then deltas with ABSOLUTE sizes (0 deletes). Rule: a delta's `u` must equal the
//! previous `u + 1`; `u <= previous` is dropped as a duplicate; anything larger is a gap and
//! forces a resync (a new snapshot is only sent on resubscription). A cross sequence `seq`
//! that moves backwards is dropped. Contiguity is documented for the full-depth topic and was
//! observed on the depth topics used here; it is enforced either way.
//! Trades use `publicTrade.<SYMBOL>`: `S` is the taker side. Block trades and Retail Price
//! Improvement prints are excluded; they do not interact with the displayed book.
use common::Side;
use wire::{
    Scanner,
    feed::{
        DecodeError, Decoded, DecoderStats, Drop, MS, NativeDecoder, Prints, Scale, Updates, resync,
    },
    window::{NativeWindow, WireFrame},
};
pub const VENUE_NAME: &str = "bybit";
pub const LINEAR_URL: &str = "wss://stream.bybit.com/v5/public/linear";
pub fn subscribe(symbol: &str, depth: usize) -> String {
    format!(r#"{{"op":"subscribe","args":["orderbook.{depth}.{symbol}","publicTrade.{symbol}"]}}"#)
}
/// Application-level keepalive; send every 20 s.
pub const PING: &str = r#"{"op":"ping"}"#;
pub struct BybitLinear<const W: usize> {
    symbol: &'static [u8],
    scale: Scale,
    window: NativeWindow<W>,
    updates: Updates,
    prints: Prints,
    last: Option<(u64, u64)>,
    pub stats: DecoderStats,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Topic {
    Unknown,
    Book,
    Trade,
}
impl<const W: usize> BybitLinear<W> {
    pub fn new(symbol: &'static str, scale: Scale, window: usize) -> Result<Self, DecodeError> {
        Ok(Self {
            symbol: symbol.as_bytes(),
            scale,
            window: NativeWindow::new(window)?,
            updates: Updates::default(),
            prints: Prints::default(),
            last: None,
            stats: DecoderStats::default(),
        })
    }
    pub fn reset(&mut self) {
        self.window.reset();
        self.last = None;
    }
    pub fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        let result = self.decode_inner(text, frame);
        if let Ok(d) = result {
            self.stats.count(d);
        }
        result
    }
    fn topic(&self, raw: &[u8]) -> Topic {
        let ends = raw.ends_with(self.symbol) && raw.len() > self.symbol.len();
        if ends && raw.starts_with(b"orderbook.") {
            Topic::Book
        } else if ends && raw.starts_with(b"publicTrade.") {
            Topic::Trade
        } else {
            Topic::Unknown
        }
    }
    fn decode_inner(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        let mut s = Scanner::new(text);
        s.begin_object()?;
        self.updates.clear();
        self.prints.begin();
        let (mut topic, mut kind) = (Topic::Unknown, &b""[..]);
        let (mut control, mut success) = (false, true);
        let (mut u, mut seq, mut cts, mut excluded) = (None, None, 0, 0_u64);
        let mut symbol_ok = true;
        while let Some(key) = s.key()? {
            match key {
                b"topic" => topic = self.topic(s.string()?),
                b"type" => kind = s.string()?,
                b"cts" => cts = s.u64()?,
                b"op" => {
                    s.string()?;
                    control = true;
                }
                b"success" => success = s.boolean()?,
                // Book data is an object; trade data is an array of prints.
                b"data" if s.next_is_array() => {
                    s.begin_array()?;
                    while s.item()? {
                        s.begin_object()?;
                        let (mut side, mut price, mut qty, mut time) = (None, 0, 0, 0);
                        let mut skip_print = false;
                        while let Some(k) = s.key()? {
                            match k {
                                b"S" => {
                                    side = Some(match s.string()? {
                                        b"Buy" => Side::Buy,
                                        b"Sell" => Side::Sell,
                                        _ => return Err(DecodeError::Field("S")),
                                    })
                                }
                                b"p" => price = self.scale.price(s.string()?)?,
                                b"v" => qty = self.scale.qty(s.string()?)?,
                                b"T" => time = s.u64()?,
                                b"s" => symbol_ok &= s.string()? == self.symbol,
                                b"BT" | b"RPI" => skip_print |= s.boolean()?,
                                b"seq" => seq = Some(s.u64()?),
                                _ => s.skip()?,
                            }
                        }
                        if skip_print {
                            excluded += 1;
                            continue;
                        }
                        self.prints.push(
                            side.ok_or(DecodeError::Field("S"))?,
                            price,
                            qty,
                            time * MS,
                        )?;
                    }
                }
                b"data" => {
                    s.begin_object()?;
                    while let Some(k) = s.key()? {
                        match k {
                            b"s" => symbol_ok &= s.string()? == self.symbol,
                            b"b" => self.updates.parse_levels(&mut s, Side::Buy, self.scale)?,
                            b"a" => self.updates.parse_levels(&mut s, Side::Sell, self.scale)?,
                            b"u" => u = Some(s.u64()?),
                            b"seq" => seq = Some(s.u64()?),
                            _ => s.skip()?,
                        }
                    }
                }
                _ => s.skip()?,
            }
        }
        s.finish()?;
        if control {
            return if success {
                Ok(Decoded::Control)
            } else {
                Err(DecodeError::Venue)
            };
        }
        if !symbol_ok {
            return Err(DecodeError::WrongSymbol);
        }
        match topic {
            Topic::Trade => {
                self.prints.set_sequence(seq.unwrap_or(0));
                if !self.prints.fill(frame)? {
                    return Ok(if excluded > 0 {
                        Decoded::Dropped(Drop::ExcludedPrint)
                    } else {
                        Decoded::Unchanged
                    });
                }
                self.stats.excluded_prints += excluded;
                Ok(Decoded::Frame)
            }
            Topic::Book => {
                let u = u.ok_or(DecodeError::Field("u"))?;
                let seq = seq.ok_or(DecodeError::Field("seq"))?;
                frame.exchange_sequence = u;
                frame.exchange_ts_ns = cts * MS;
                match kind {
                    b"snapshot" => {
                        self.window.reset();
                        self.last = Some((u, seq));
                    }
                    b"delta" => {
                        let Some((last_u, last_seq)) = self.last else {
                            return Ok(Decoded::Dropped(Drop::BeforeSnapshot));
                        };
                        if u <= last_u {
                            return Ok(Decoded::Dropped(Drop::Duplicate));
                        }
                        if seq < last_seq {
                            return Ok(Decoded::Dropped(Drop::StaleSequence));
                        }
                        if u != last_u + 1 {
                            self.last = None;
                            return Ok(resync(&mut self.window, frame, wire::feed::Resync::Gap));
                        }
                        self.last = Some((u, seq));
                    }
                    _ => return Err(DecodeError::Field("type")),
                }
                let decoded = self.updates.apply(&mut self.window, frame)?;
                if matches!(decoded, Decoded::Resync(_)) {
                    self.last = None;
                }
                Ok(decoded)
            }
            Topic::Unknown => Err(DecodeError::Field("topic")),
        }
    }
}
impl<const W: usize> NativeDecoder for BybitLinear<W> {
    fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        BybitLinear::decode(self, text, frame)
    }
    fn reset(&mut self) {
        BybitLinear::reset(self)
    }
    fn stats(&self) -> DecoderStats {
        self.stats
    }
    fn refresh(&mut self, frame: &mut WireFrame) -> bool {
        self.window.refresh(frame).unwrap_or(false)
    }
    fn drain(&mut self, frame: &mut WireFrame) -> bool {
        self.prints.fill(frame).unwrap_or(false)
    }
}
