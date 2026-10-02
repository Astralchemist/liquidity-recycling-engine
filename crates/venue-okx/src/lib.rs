//! OKX v5 public adapter for perpetual swaps (decoding and sync rules only; no network here).
//!
//! Depth uses the public `books` channel (400 levels, 100 ms; the tick-by-tick channels need
//! login and VIP4). The snapshot carries `prevSeqId = -1`; each update must have
//! `prevSeqId == previous seqId`. `seqId` may DECREASE after maintenance while the link still
//! holds, so only the link is checked. An update with no levels and `prevSeqId == seqId` is a
//! heartbeat. Sizes are absolute (0 deletes) and, for swaps, in CONTRACTS: the scale's size
//! decimals must match the contract lot, and recording metadata must state each lot's base
//! quantity (`ctVal`). The `checksum` field is deprecated (always 0) and is not validated.
//! Trades use `trades`: `side` is the taker side; `source = 1` (Retail Price Improvement)
//! prints are excluded. Keepalive is the text frame `ping`, answered by `pong`.
use common::Side;
use wire::{
    Scanner,
    feed::{
        DecodeError, Decoded, DecoderStats, Drop, MS, NativeDecoder, Prints, Scale, Updates, resync,
    },
    window::{NativeWindow, WireFrame},
};
pub const VENUE_NAME: &str = "okx";
pub const PUBLIC_URL: &str = "wss://ws.okx.com/ws/v5/public";
pub fn subscribe(inst_id: &str) -> String {
    format!(
        r#"{{"op":"subscribe","args":[{{"channel":"books","instId":"{inst_id}"}},{{"channel":"trades","instId":"{inst_id}"}}]}}"#
    )
}
pub const PING: &str = "ping";
pub struct OkxSwap<const W: usize> {
    inst_id: &'static [u8],
    scale: Scale,
    window: NativeWindow<W>,
    updates: Updates,
    prints: Prints,
    last_seq: Option<i64>,
    pub stats: DecoderStats,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    Unknown,
    Books,
    Trades,
}
impl<const W: usize> OkxSwap<W> {
    pub fn new(inst_id: &'static str, scale: Scale, window: usize) -> Result<Self, DecodeError> {
        Ok(Self {
            inst_id: inst_id.as_bytes(),
            scale,
            window: NativeWindow::new(window)?,
            updates: Updates::default(),
            prints: Prints::default(),
            last_seq: None,
            stats: DecoderStats::default(),
        })
    }
    pub fn reset(&mut self) {
        self.window.reset();
        self.last_seq = None;
    }
    pub fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        let result = self.decode_inner(text, frame);
        if let Ok(d) = result {
            self.stats.count(d);
        }
        result
    }
    fn decode_inner(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        if text == b"pong" {
            return Ok(Decoded::Control);
        }
        let mut s = Scanner::new(text);
        s.begin_object()?;
        self.updates.clear();
        self.prints.begin();
        let (mut channel, mut action, mut event) = (Channel::Unknown, &b""[..], &b""[..]);
        let (mut previous, mut seq, mut ts, mut instrument_ok, mut excluded) =
            (None, None, 0, true, 0_u64);
        let mut trade_seq = 0;
        while let Some(key) = s.key()? {
            match key {
                b"event" => event = s.string()?,
                b"action" => action = s.string()?,
                b"arg" => {
                    s.begin_object()?;
                    while let Some(k) = s.key()? {
                        match k {
                            b"channel" => {
                                channel = match s.string()? {
                                    b"books" => Channel::Books,
                                    b"trades" => Channel::Trades,
                                    _ => Channel::Unknown,
                                }
                            }
                            b"instId" => instrument_ok &= s.string()? == self.inst_id,
                            _ => s.skip()?,
                        }
                    }
                }
                b"data" => {
                    s.begin_array()?;
                    while s.item()? {
                        s.begin_object()?;
                        let (mut px, mut sz, mut side, mut source_rpi) = (0, 0, None, false);
                        while let Some(k) = s.key()? {
                            match k {
                                b"bids" => {
                                    self.updates.parse_levels(&mut s, Side::Buy, self.scale)?
                                }
                                b"asks" => {
                                    self.updates.parse_levels(&mut s, Side::Sell, self.scale)?
                                }
                                b"prevSeqId" => previous = Some(s.i64()?),
                                // `arg` precedes `data`, so the channel is known here.
                                b"seqId" if channel == Channel::Trades => trade_seq = s.u64()?,
                                b"seqId" => seq = Some(s.i64()?),
                                b"ts" => ts = s.u64()?,
                                b"px" => px = self.scale.price(s.string()?)?,
                                b"sz" => sz = self.scale.qty(s.string()?)?,
                                b"side" => {
                                    side = Some(match s.string()? {
                                        b"buy" => Side::Buy,
                                        b"sell" => Side::Sell,
                                        _ => return Err(DecodeError::Field("side")),
                                    })
                                }
                                b"source" => source_rpi = s.u64()? == 1,
                                b"instId" => instrument_ok &= s.string()? == self.inst_id,
                                _ => s.skip()?,
                            }
                        }
                        if channel == Channel::Trades {
                            if source_rpi {
                                excluded += 1;
                                continue;
                            }
                            let side = side.ok_or(DecodeError::Field("side"))?;
                            self.prints.push(side, px, sz, ts * MS)?;
                        }
                    }
                }
                _ => s.skip()?,
            }
        }
        s.finish()?;
        match event {
            b"" => {}
            b"error" => return Err(DecodeError::Venue),
            _ => return Ok(Decoded::Control),
        }
        if !instrument_ok {
            return Err(DecodeError::WrongSymbol);
        }
        match channel {
            Channel::Trades => {
                self.prints.set_sequence(trade_seq);
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
            Channel::Books => {
                let seq = seq.ok_or(DecodeError::Field("seqId"))?;
                let previous = previous.ok_or(DecodeError::Field("prevSeqId"))?;
                frame.exchange_sequence = seq as u64;
                frame.exchange_ts_ns = ts * MS;
                match action {
                    b"snapshot" => {
                        if previous != -1 {
                            return Err(DecodeError::Field("prevSeqId"));
                        }
                        self.window.reset();
                    }
                    b"update" => {
                        let Some(last) = self.last_seq else {
                            return Ok(Decoded::Dropped(Drop::BeforeSnapshot));
                        };
                        if previous != last {
                            self.last_seq = None;
                            return Ok(resync(&mut self.window, frame, wire::feed::Resync::Gap));
                        }
                        if previous == seq && self.updates.is_empty() {
                            return Ok(Decoded::Unchanged);
                        }
                    }
                    _ => return Err(DecodeError::Field("action")),
                }
                self.last_seq = Some(seq);
                let decoded = self.updates.apply(&mut self.window, frame)?;
                if matches!(decoded, Decoded::Resync(_)) {
                    self.last_seq = None;
                }
                Ok(decoded)
            }
            Channel::Unknown => Err(DecodeError::Field("channel")),
        }
    }
}
impl<const W: usize> NativeDecoder for OkxSwap<W> {
    fn decode(&mut self, text: &[u8], frame: &mut WireFrame) -> Result<Decoded, DecodeError> {
        OkxSwap::decode(self, text, frame)
    }
    fn reset(&mut self) {
        OkxSwap::reset(self)
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
