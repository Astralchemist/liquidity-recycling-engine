//! Live public market-data ingress. Network I/O lives only in this crate.
//!
//! One feed thread per WebSocket connection decodes native messages into fixed-size
//! `WireFrame`s and sends them over ONE bounded channel. The single consumer, the `Sequencer`,
//! stamps each frame with a strictly increasing local receive time from a shared monotonic
//! clock and assigns contiguous per-venue canonical sequences. Recording and replay therefore
//! see exactly the order the engine saw: canonical receive time is the sequencer's stamp, and
//! socket and decode times are kept separately for stage latency.
//!
//! Feed threads allocate inside the WebSocket library (one buffer per message). The sequencer
//! and everything it feeds allocate nothing after startup.
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
use std::{
    io,
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};
use wire::{
    feed::{DecodeError, Decoded, DecoderStats, NativeDecoder},
    window::{FrameKind, MAX_FRAME, WireFrame},
};
/// Process-wide monotonic nanoseconds since session start.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    origin: Instant,
}
impl Clock {
    pub fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
    pub fn now_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }
}
/// Installs the rustls crypto provider once per process.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: &'static str,
    pub venue: u16,
    pub url: String,
    /// Sent after every (re)connect.
    pub subscribe: Option<String>,
    /// Application keepalive text and interval (protocol pings are answered automatically).
    pub ping: Option<(&'static str, Duration)>,
    /// Book sources send a Reset frame whenever continuity is lost.
    pub carries_book: bool,
    /// Set by the consumer when the engine needs a fresh snapshot; the thread reconnects.
    pub resync: Arc<AtomicBool>,
}
/// Quiet-but-valid book sources refresh freshness at most this often.
pub const REFRESH_NS: u64 = 250_000_000;
#[derive(Debug, Default, Clone)]
pub struct SourceReport {
    pub name: &'static str,
    pub connects: u64,
    pub connect_errors: u64,
    pub disconnects: u64,
    pub resyncs: u64,
    pub decode_errors: u64,
    /// Zero-change refresh frames sent for quiet but valid books.
    pub refreshes: u64,
    pub last_decode_error: Option<DecodeError>,
    pub decoder: DecoderStats,
}
fn set_timeout(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, t: Duration) -> io::Result<()> {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(t)),
        MaybeTlsStream::Rustls(s) => s.get_mut().set_read_timeout(Some(t)),
        _ => Ok(()),
    }
}
/// Feed thread body: connect, subscribe, decode until `stop`, reconnecting after any fault.
/// Frames are sent with a blocking send: a slow consumer back-pressures the socket instead
/// of silently dropping data.
pub fn run_source<D: NativeDecoder>(
    spec: SourceSpec,
    mut decoder: D,
    clock: Clock,
    tx: SyncSender<WireFrame>,
    stop: Arc<AtomicBool>,
) -> SourceReport {
    let mut report = SourceReport {
        name: spec.name,
        ..SourceReport::default()
    };
    let mut frame = WireFrame::new(spec.venue);
    let reset = |frame: &mut WireFrame| {
        frame.clear(FrameKind::Reset);
        frame.socket_ns = clock.now_ns();
        frame.decoded_ns = frame.socket_ns;
        tx.send(*frame).is_ok()
    };
    while !stop.load(Ordering::Relaxed) {
        report.connects += 1;
        let mut ws = match tungstenite::connect(spec.url.as_str()) {
            Ok((ws, _)) => ws,
            Err(_) => {
                report.connect_errors += 1;
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let _ = set_timeout(&mut ws, Duration::from_millis(500));
        decoder.reset();
        if let Some(s) = &spec.subscribe {
            if ws.send(Message::Text(s.clone().into())).is_err() {
                report.disconnects += 1;
                continue;
            }
        }
        let mut last_ping = Instant::now();
        let mut last_book_ns = clock.now_ns();
        spec.resync.store(false, Ordering::Relaxed);
        loop {
            if spec.resync.swap(false, Ordering::Relaxed) {
                report.resyncs += 1;
                break;
            }
            if stop.load(Ordering::Relaxed) {
                let _ = ws.close(None);
                report.decoder = decoder.stats();
                return report;
            }
            if let Some((text, every)) = spec.ping {
                if last_ping.elapsed() >= every {
                    last_ping = Instant::now();
                    if ws.send(Message::Text(text.into())).is_err() {
                        break;
                    }
                }
            }
            let bytes = match ws.read() {
                Ok(Message::Text(t)) => t,
                Ok(Message::Binary(b)) => match String::from_utf8(b.to_vec()) {
                    Ok(t) => t.into(),
                    Err(_) => continue,
                },
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            };
            let socket_ns = clock.now_ns();
            match decoder.decode(bytes.as_bytes(), &mut frame) {
                Ok(Decoded::Unchanged)
                    if spec.carries_book && socket_ns - last_book_ns >= REFRESH_NS =>
                {
                    if decoder.refresh(&mut frame) {
                        last_book_ns = socket_ns;
                        frame.socket_ns = socket_ns;
                        frame.decoded_ns = clock.now_ns();
                        report.refreshes += 1;
                        if tx.send(frame).is_err() {
                            report.decoder = decoder.stats();
                            return report;
                        }
                    }
                }
                Ok(Decoded::Frame) => {
                    if frame.kind != FrameKind::Trades {
                        last_book_ns = socket_ns;
                    }
                    let mut more = true;
                    while more {
                        frame.socket_ns = socket_ns;
                        frame.decoded_ns = clock.now_ns();
                        if tx.send(frame).is_err() {
                            report.decoder = decoder.stats();
                            return report;
                        }
                        more = decoder.drain(&mut frame);
                    }
                }
                Ok(Decoded::Resync(_)) => {
                    report.resyncs += 1;
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    report.decode_errors += 1;
                    report.last_decode_error = Some(e);
                    break;
                }
            }
        }
        // Continuity is lost: the engine must not keep trusting this venue's book.
        report.disconnects += 1;
        if spec.carries_book && !reset(&mut frame) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    report.decoder = decoder.stats();
    report
}
/// One canonical step for the engine and recorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canonical<'a> {
    Event(MarketEvent),
    Batch(&'a [MarketEvent]),
}
/// Single consumer of all feed threads. Converts frames into canonical events with a strictly
/// increasing receive time and contiguous per-venue sequences.
pub struct Sequencer<const V: usize> {
    venues: [VenueId; V],
    instruments: [InstrumentId; V],
    sequences: [u64; V],
    last_receive: u64,
    batch: [MarketEvent; MAX_FRAME],
    /// A venue's book is live from its snapshot until its next reset.
    live: [bool; V],
    pub resets: [u64; V],
    /// Trades or batches dropped because the venue had no live book (e.g. prints that arrive
    /// before the first snapshot). Forwarding them would be invalid canonical input.
    pub dropped: [u64; V],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceError {
    UnknownVenue,
}
impl<const V: usize> Sequencer<V> {
    pub fn new(venues: [VenueId; V], instruments: [InstrumentId; V]) -> Self {
        let blank = MarketEvent {
            venue: VenueId(0),
            instrument: InstrumentId(0),
            sequence: 0,
            exchange_sequence: 0,
            exchange_ts: 0,
            receive_ts: Timestamp(0),
            event_type: K::Add,
            side: Side::Buy,
            price_ticks: PriceTicks(0),
            qty_units: QtyUnits(0),
        };
        Self {
            venues,
            instruments,
            sequences: [0; V],
            last_receive: 0,
            batch: [blank; MAX_FRAME],
            live: [false; V],
            resets: [0; V],
            dropped: [0; V],
        }
    }
    /// The consumer found the venue's engine book withdrawn: drop everything but a snapshot
    /// until one arrives (the caller also asks the feed thread to resubscribe).
    pub fn invalidate(&mut self, v: usize) {
        self.live[v] = false;
    }
    pub fn venue_index(&self, venue: u16) -> Result<usize, SequenceError> {
        self.venues
            .iter()
            .position(|v| v.0 == venue)
            .ok_or(SequenceError::UnknownVenue)
    }
    /// Stamps `frame` at `now_ns` (bumped to stay strictly increasing) and hands each canonical
    /// step to `apply`. Returns the receive time used. Reset frames emit nothing: a venue that
    /// loses continuity ages out through the engine's staleness rule and is rebuilt by the
    /// snapshot that follows resubscription, so replay needs no extra event kind.
    pub fn sequence<E>(
        &mut self,
        frame: &WireFrame,
        now_ns: u64,
        mut apply: impl FnMut(Canonical<'_>) -> Result<(), E>,
    ) -> Result<Result<Timestamp, E>, SequenceError> {
        let v = self.venue_index(frame.venue)?;
        let receive = now_ns.max(self.last_receive + 1);
        self.last_receive = receive;
        let event = |seq: &mut u64, kind, side, price, qty| {
            *seq += 1;
            MarketEvent {
                venue: self.venues[v],
                instrument: self.instruments[v],
                sequence: *seq,
                exchange_sequence: frame.exchange_sequence,
                exchange_ts: frame.exchange_ts_ns,
                receive_ts: Timestamp(receive),
                event_type: kind,
                side,
                price_ticks: PriceTicks(price),
                qty_units: QtyUnits(qty),
            }
        };
        if matches!(frame.kind, FrameKind::Trades | FrameKind::Batch) && !self.live[v] {
            self.dropped[v] += 1;
            return Ok(Ok(Timestamp(receive)));
        }
        match frame.kind {
            FrameKind::Snapshot => self.live[v] = true,
            FrameKind::Reset => self.live[v] = false,
            _ => {}
        }
        let seq = &mut self.sequences[v];
        let result = (|| {
            match frame.kind {
                FrameKind::Reset => self.resets[v] += 1,
                FrameKind::Snapshot => {
                    apply(Canonical::Event(event(
                        seq,
                        K::SnapshotStart,
                        Side::Buy,
                        0,
                        0,
                    )))?;
                    for c in frame.changes() {
                        apply(Canonical::Event(event(seq, K::Add, c.side, c.price, c.qty)))?;
                    }
                    apply(Canonical::Event(event(
                        seq,
                        K::SnapshotEnd,
                        Side::Buy,
                        0,
                        0,
                    )))?;
                }
                FrameKind::Trades => {
                    for c in frame.changes() {
                        apply(Canonical::Event(event(
                            seq,
                            K::Trade,
                            c.side,
                            c.price,
                            c.qty,
                        )))?;
                    }
                }
                FrameKind::Batch => {
                    for (k, c) in frame.changes().iter().enumerate() {
                        self.batch[k] = event(seq, c.kind, c.side, c.price, c.qty);
                    }
                    match frame.len {
                        0 => {}
                        1 => apply(Canonical::Event(self.batch[0]))?,
                        n => apply(Canonical::Batch(&self.batch[..n]))?,
                    }
                }
            }
            Ok(Timestamp(receive))
        })();
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use wire::window::Change;
    fn frame(venue: u16, kind: FrameKind, changes: &[(K, Side, i64, i64)]) -> WireFrame {
        let mut f = WireFrame::new(venue);
        f.clear(kind);
        f.exchange_sequence = 77;
        for &(kind, side, price, qty) in changes {
            f.push(Change {
                kind,
                side,
                price,
                qty,
            })
            .unwrap();
        }
        f
    }
    #[test]
    fn frames_become_contiguous_canonical_steps_with_strictly_increasing_time() {
        let mut s = Sequencer::new(
            [VenueId(1), VenueId(2)],
            [InstrumentId(11), InstrumentId(22)],
        );
        let log = std::cell::RefCell::new(Vec::new());
        let mut collect = |c: Canonical<'_>| -> Result<(), ()> {
            match c {
                Canonical::Event(e) => log.borrow_mut().push((false, vec![e])),
                Canonical::Batch(b) => log.borrow_mut().push((true, b.to_vec())),
            }
            Ok(())
        };
        let snap = frame(
            1,
            FrameKind::Snapshot,
            &[(K::Add, Side::Buy, 99, 1), (K::Add, Side::Sell, 101, 2)],
        );
        let batch = frame(
            1,
            FrameKind::Batch,
            &[(K::Cancel, Side::Buy, 99, 0), (K::Add, Side::Buy, 98, 3)],
        );
        let trades = frame(
            2,
            FrameKind::Trades,
            &[
                (K::Trade, Side::Sell, 100, 5),
                (K::Trade, Side::Buy, 101, 1),
            ],
        );
        // Prints before a venue's first snapshot are dropped, never forwarded.
        let early = frame(1, FrameKind::Trades, &[(K::Trade, Side::Buy, 100, 1)]);
        s.sequence(&early, 40, &mut collect).unwrap().unwrap();
        assert!(log.borrow().is_empty());
        assert_eq!(s.dropped, [1, 0]);
        // Equal and regressing wall readings still produce strictly increasing receive times.
        let t1 = s.sequence(&snap, 50, &mut collect).unwrap().unwrap();
        let t2 = s.sequence(&batch, 50, &mut collect).unwrap().unwrap();
        let snap2 = frame(
            2,
            FrameKind::Snapshot,
            &[(K::Add, Side::Buy, 99, 1), (K::Add, Side::Sell, 101, 1)],
        );
        s.sequence(&snap2, 55, &mut collect).unwrap().unwrap();
        let t3 = s.sequence(&trades, 10, &mut collect).unwrap().unwrap();
        assert!(t1 < t2 && t2 < t3);
        {
            let out = log.borrow();
            let s1: Vec<u64> = out
                .iter()
                .flat_map(|(_, es)| es.iter())
                .filter(|e| e.venue == VenueId(1))
                .map(|e| e.sequence)
                .collect();
            assert_eq!(s1, [1, 2, 3, 4, 5, 6]);
            assert_eq!(out[0].1[0].event_type, K::SnapshotStart);
            assert_eq!(out[3].1[0].event_type, K::SnapshotEnd);
            assert!(out[4].0 && out[4].1.len() == 2);
            assert!(
                out[4]
                    .1
                    .iter()
                    .all(|e| e.receive_ts == t2 && e.exchange_sequence == 77)
            );
            // Venue 2: snapshot markers and two adds (1..=4), then its prints (5, 6).
            assert_eq!(out[9].1[0].instrument, InstrumentId(22));
            assert_eq!((out[9].1[0].sequence, out[10].1[0].sequence), (5, 6));
        }
        let reset = frame(2, FrameKind::Reset, &[]);
        let before = log.borrow().len();
        s.sequence(&reset, 60, &mut collect).unwrap().unwrap();
        assert_eq!((log.borrow().len(), s.resets), (before, [0, 1]));
        // After a reset, nothing reaches the engine for that venue until a new snapshot.
        let stale = frame(2, FrameKind::Batch, &[(K::Modify, Side::Buy, 99, 2)]);
        s.sequence(&stale, 61, &mut collect).unwrap().unwrap();
        assert_eq!((log.borrow().len(), s.dropped), (before, [1, 1]));
        assert_eq!(
            s.sequence(&frame(9, FrameKind::Batch, &[]), 1, &mut collect),
            Err(SequenceError::UnknownVenue)
        );
    }
}
