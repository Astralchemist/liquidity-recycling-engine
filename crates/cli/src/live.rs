//! Live public feeds (Binance USDⓈ-M, Bybit linear, OKX swap) through the shared engine.
//! Public market data only; fills are SIMULATED and no order is ever sent.
use common::VenueId;
use consolidator::normalization::Normalizer;
use engine::Engine;
use feeds::{Canonical, Clock, Sequencer, SourceReport, SourceSpec};
use fixed_point::PriceTicks;
use inventory::{
    InventoryEvent, Ledger,
    journal::{Reader, Writer},
};
use recorder::FrameRecorder;
use replay::merged::{MergedFrame, MergedFrames};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{RecvTimeoutError, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};
use wire::{
    feed::Scale,
    window::{FrameKind, WireFrame},
};
/// 3 venues, 64 levels per book side, 192 consolidated, 1024-tick corridor, 6 buckets,
/// 64 zone slots, 4096-slot windows, 32 inventory slots, 8 episodes.
pub(super) type LiveEngine = Engine<3, 64, 192, 1024, 6, 64, 4096, 32, 8>;
const CORRIDOR: i64 = 1024;
/// Top-K window per venue: Binance's partial-depth stream carries at most 20 levels.
const WINDOW: usize = 20;
const BINANCE_SYMBOL: &str = "BTCUSDT";
const BYBIT_SYMBOL: &str = "BTCUSDT";
const OKX_INSTRUMENT: &str = "BTC-USDT-SWAP";
/// Native wire scales (exact decimals; see config/phase8-market.toml).
const BINANCE: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 3,
    qty_atoms: 1,
};
const BYBIT: Scale = BINANCE;
/// OKX sizes are contracts in lots of 0.01.
const OKX: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 2,
    qty_atoms: 1,
};
struct Texts {
    market: String,
    structures: String,
    flow: String,
    engine: String,
}
fn build(t: &Texts) -> Result<Box<LiveEngine>, String> {
    let (grid, venues) = super::multi::parse(&t.market)?;
    let (liquidity, voids) = super::structures::parse(&t.structures)?;
    let (flow, _) = super::flow::parse(&t.flow)?;
    let (inventory, engine) = super::scenarios::parse(&t.engine)?;
    Ok(Box::new(
        LiveEngine::new(grid, venues, liquidity, voids, flow, inventory, engine)
            .map_err(|e| format!("engine configuration: {e:?}"))?,
    ))
}
fn spawn_sources(
    clock: Clock,
    stop: &Arc<AtomicBool>,
    tx: &std::sync::mpsc::SyncSender<WireFrame>,
    resync: &[Arc<AtomicBool>; 3],
) -> Result<Vec<thread::JoinHandle<SourceReport>>, String> {
    let ping = Duration::from_secs(20);
    let specs = [
        SourceSpec {
            name: "binance-depth",
            resync: resync[0].clone(),
            venue: 1,
            url: venue_binance::depth_url(&BINANCE_SYMBOL.to_lowercase(), WINDOW),
            subscribe: None,
            ping: None,
            carries_book: true,
        },
        SourceSpec {
            name: "binance-trades",
            resync: Arc::new(AtomicBool::new(false)),
            venue: 1,
            url: venue_binance::trade_url(&BINANCE_SYMBOL.to_lowercase()),
            subscribe: None,
            ping: None,
            carries_book: false,
        },
        SourceSpec {
            name: "bybit",
            resync: resync[1].clone(),
            venue: 2,
            url: venue_bybit::LINEAR_URL.into(),
            subscribe: Some(venue_bybit::subscribe(BYBIT_SYMBOL, 50)),
            ping: Some((venue_bybit::PING, ping)),
            carries_book: true,
        },
        SourceSpec {
            name: "okx",
            resync: resync[2].clone(),
            venue: 3,
            url: venue_okx::PUBLIC_URL.into(),
            subscribe: Some(venue_okx::subscribe(OKX_INSTRUMENT)),
            ping: Some((venue_okx::PING, ping)),
            carries_book: true,
        },
    ];
    let mut handles = Vec::new();
    for spec in specs {
        let (stop, tx) = (stop.clone(), tx.clone());
        let name = spec.name;
        let handle = thread::Builder::new()
            .name(name.into())
            .spawn(move || match name {
                "binance-depth" | "binance-trades" => feeds::run_source(
                    spec,
                    venue_binance::BinanceFutures::<64>::new(BINANCE_SYMBOL, BINANCE, WINDOW)
                        .expect("valid window"),
                    clock,
                    tx,
                    stop,
                ),
                "bybit" => feeds::run_source(
                    spec,
                    venue_bybit::BybitLinear::<256>::new(BYBIT_SYMBOL, BYBIT, WINDOW)
                        .expect("valid window"),
                    clock,
                    tx,
                    stop,
                ),
                _ => feeds::run_source(
                    spec,
                    venue_okx::OkxSwap::<1024>::new(OKX_INSTRUMENT, OKX, WINDOW)
                        .expect("valid window"),
                    clock,
                    tx,
                    stop,
                ),
            });
        handles.push(handle.map_err(|e| e.to_string())?);
    }
    Ok(handles)
}
struct Latency {
    decode: Vec<u64>,
    queue: Vec<u64>,
    engine: Vec<u64>,
}
fn quantiles(name: &str, samples: &mut [u64]) {
    if samples.is_empty() {
        println!("latency {name}: no samples");
        return;
    }
    samples.sort_unstable();
    let q = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
    println!(
        "latency {name} n={} ns: p50={} p90={} p99={} p99.9={} max={}",
        samples.len(),
        q(0.5),
        q(0.9),
        q(0.99),
        q(0.999),
        samples[samples.len() - 1]
    );
}
struct Session {
    engine: Box<LiveEngine>,
    commands: usize,
    frames: [u64; 3],
    resets: [u64; 3],
    dropped: [u64; 3],
    withdrawn: [u64; 3],
    latency: Latency,
    lower_price: i64,
}
/// Engine thread: centre the corridor on the first snapshot, then sequence, apply and record
/// every frame until the deadline. Recording I/O is buffered on this thread (a dedicated
/// recorder thread is a later improvement).
fn session(
    texts: &mut Texts,
    dir: &Path,
    clock: Clock,
    rx: std::sync::mpsc::Receiver<WireFrame>,
    seconds: u64,
    resync: [Arc<AtomicBool>; 3],
) -> Result<Session, String> {
    let (grid, venues) = super::multi::parse(&texts.market)?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let first = loop {
        let f = rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "no snapshot within 30 s; check network access".to_string())?;
        if f.kind == FrameKind::Snapshot {
            break f;
        }
    };
    let v = venues
        .iter()
        .position(|c| c.venue.0 == first.venue)
        .ok_or("unknown venue")?;
    let n = Normalizer::new(venues[v].metadata, grid).map_err(|e| format!("{e:?}"))?;
    let best = |side| {
        first
            .changes()
            .iter()
            .filter(|c| c.side == side)
            .map(|c| c.price)
            .reduce(|a, b| {
                if side == common::Side::Buy {
                    a.max(b)
                } else {
                    a.min(b)
                }
            })
            .ok_or("one-sided first snapshot")
    };
    let mid = (n
        .price(PriceTicks(best(common::Side::Buy)?))
        .map_err(|e| format!("{e:?}"))?
        .0
        + n.price(PriceTicks(best(common::Side::Sell)?))
            .map_err(|e| format!("{e:?}"))?
            .0)
        / 2;
    let lower_price = mid - CORRIDOR / 2;
    texts.structures = texts
        .structures
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("lower_price") {
                format!("lower_price = {lower_price}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (file, text) in [
        ("market.toml", &texts.market),
        ("structures.toml", &texts.structures),
        ("flow.toml", &texts.flow),
        ("engine.toml", &texts.engine),
    ] {
        fs::write(dir.join(file), text).map_err(|e| e.to_string())?;
    }
    let mut engine = build(texts)?;
    let mut sequencer = Sequencer::new(
        venues.map(|c| c.venue),
        venues.map(|c| c.metadata.instrument),
    );
    let mut recorders = Vec::new();
    for c in venues {
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(format!("venue-{}.lre", c.venue.0)))
            .map_err(|e| e.to_string())?;
        recorders.push(
            FrameRecorder::new(BufWriter::new(f), super::multi::metadata(c))
                .map_err(|e| e.to_string())?,
        );
    }
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("inventory.lri"))
        .map_err(|e| e.to_string())?;
    let mut journal = Writer::new(
        BufWriter::new(f),
        recorder::checksum(texts.engine.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    let mut latency = Latency {
        decode: Vec::with_capacity(4_000_000),
        queue: Vec::with_capacity(4_000_000),
        engine: Vec::with_capacity(4_000_000),
    };
    let (mut frames, mut commands, mut withdrawn) = ([0_u64; 3], 0, [0_u64; 3]);
    let mut next = Some(first);
    loop {
        let frame = match next.take() {
            Some(f) => f,
            None => match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(f) => f,
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(_) => break,
            },
        };
        let dequeued = clock.now_ns();
        let v = sequencer
            .venue_index(frame.venue)
            .map_err(|e| format!("{e:?}"))?;
        frames[v] += 1;
        let started = Instant::now();
        let mut io_error = None;
        let mut book_withdrawn = false;
        let result = sequencer
            .sequence(&frame, dequeued, |c| -> Result<(), String> {
                // The engine may have withdrawn this venue (staleness), possibly at THIS frame's
                // own timestamp after a silent gap. Only a snapshot can restore it; anything else
                // would be invalid input, so drop it and resync.
                let first = match c {
                    Canonical::Event(e) => e,
                    Canonical::Batch(b) => b[0],
                };
                let snapshot = frame.kind == FrameKind::Snapshot;
                if !engine.accepts(first.venue, first.receive_ts, snapshot) {
                    book_withdrawn = true;
                    return Ok(());
                }
                let mut sink = |cmd: &InventoryEvent| {
                    commands += 1;
                    if let Err(e) = journal.append(*cmd) {
                        io_error.get_or_insert(e.to_string());
                    }
                };
                match c {
                    Canonical::Event(e) => {
                        engine
                            .apply(&e, &mut sink)
                            .map_err(|e| format!("engine: {e:?}"))?;
                        recorders[v].append_event(&e).map_err(|e| e.to_string())
                    }
                    Canonical::Batch(b) => {
                        engine
                            .apply_depth_batch(b[0].venue, b, &mut sink)
                            .map_err(|e| format!("engine: {e:?}"))?;
                        recorders[v].append_batch(b).map_err(|e| e.to_string())
                    }
                }
            })
            .map_err(|e| format!("{e:?}"))?;
        result?;
        if let Some(e) = io_error {
            return Err(e);
        }
        if book_withdrawn {
            withdrawn[v] += 1;
            sequencer.invalidate(v);
            resync[v].store(true, Ordering::Relaxed);
        }
        if latency.engine.len() < latency.engine.capacity() {
            latency.engine.push(started.elapsed().as_nanos() as u64);
            if frame.kind != FrameKind::Reset {
                latency.decode.push(frame.decoded_ns - frame.socket_ns);
            }
            latency
                .queue
                .push(dequeued.saturating_sub(frame.decoded_ns));
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    for r in recorders {
        r.finish()
            .map_err(|e| e.to_string())?
            .get_ref()
            .sync_all()
            .map_err(|e| e.to_string())?;
    }
    journal
        .finish()
        .map_err(|e| e.to_string())?
        .get_ref()
        .sync_all()
        .map_err(|e| e.to_string())?;
    Ok(Session {
        engine,
        commands,
        frames,
        resets: sequencer.resets,
        dropped: sequencer.dropped,
        withdrawn,
        latency,
        lower_price,
    })
}
fn read_dir_texts(dir: &Path) -> Result<Texts, String> {
    let read = |f: &str| fs::read_to_string(dir.join(f)).map_err(|e| format!("{f}: {e}"));
    Ok(Texts {
        market: read("market.toml")?,
        structures: read("structures.toml")?,
        flow: read("flow.toml")?,
        engine: read("engine.toml")?,
    })
}
/// Replays a session through a fresh engine; `observe` sees the engine after every frame.
fn replay_at(
    dir: &Path,
    t: &Texts,
    mut observe: impl FnMut(&LiveEngine, MergedFrame<'_>),
) -> Result<(Box<LiveEngine>, Vec<InventoryEvent>), String> {
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    let mut journal = Vec::new();
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |c| journal.push(*c)),
            MergedFrame::Batch(b) => {
                engine.apply_depth_batch(b[0].venue, b, &mut |c| journal.push(*c))
            }
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        observe(&engine, frame);
    }
    // The journal file alone must rebuild the identical ledger.
    let file = File::open(dir.join("inventory.lri")).map_err(|e| e.to_string())?;
    let mut reader = Reader::new(
        BufReader::new(file),
        recorder::checksum(t.engine.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    let mut ledger =
        Ledger::<3, 32, 8>::new(engine.ledger().config()).map_err(|e| format!("{e:?}"))?;
    let mut recorded = Vec::new();
    while let Some(c) = reader.next_event().map_err(|e| e.to_string())? {
        ledger.apply(c).map_err(|e| format!("{e:?}"))?;
        recorded.push(c);
    }
    if recorded != journal || ledger != *engine.ledger() {
        return Err("inventory journal differs from the replayed engine".into());
    }
    Ok((engine, journal))
}
pub fn record(
    market: &str,
    structures: &str,
    flow: &str,
    engine: &str,
    directory: &str,
    seconds: &str,
) -> Result<(), String> {
    let seconds: u64 = seconds.parse().map_err(|_| "SECONDS must be an integer")?;
    if !(5..=3600).contains(&seconds) {
        return Err("SECONDS must be between 5 and 3600".into());
    }
    let read = |p: &str| fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    let mut texts = Texts {
        market: read(market)?,
        structures: read(structures)?,
        flow: read(flow)?,
        engine: read(engine)?,
    };
    let (_, venues) = super::multi::parse(&texts.market)?;
    if venues.map(|c| c.venue.0) != [1, 2, 3] {
        return Err("venues must be 1 Binance, 2 Bybit, 3 OKX in that order".into());
    }
    build(&texts)?;
    let dir = Path::new(directory).to_path_buf();
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    feeds::init_tls();
    let clock = Clock::start();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<WireFrame>(4096);
    let resync: [Arc<AtomicBool>; 3] = std::array::from_fn(|_| Arc::new(AtomicBool::new(false)));
    let sources = spawn_sources(clock, &stop, &tx, &resync)?;
    drop(tx);
    let engine_dir = dir.clone();
    let wall = Instant::now();
    let session = thread::Builder::new()
        .name("engine".into())
        .stack_size(256 << 20)
        .spawn(move || {
            session(&mut texts, &engine_dir, clock, rx, seconds, resync).map(|s| (s, texts))
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "engine thread panicked")?;
    stop.store(true, Ordering::Relaxed);
    let reports: Vec<SourceReport> = sources
        .into_iter()
        .map(|h| h.join().map_err(|_| "feed thread panicked".to_string()))
        .collect::<Result<_, _>>()?;
    let (mut s, texts) = session?;
    println!(
        "live session {:.1} s; corridor lower_price={} ({} ticks); frames per venue {:?}; venue resets {:?}; dropped before a live book {:?}; dropped after engine withdrawal (resync requested) {:?}; inventory commands {}",
        wall.elapsed().as_secs_f64(),
        s.lower_price,
        CORRIDOR,
        s.frames,
        s.resets,
        s.dropped,
        s.withdrawn,
        s.commands
    );
    for r in &reports {
        println!(
            "source {} connects={} connect_errors={} disconnects={} resyncs={} refreshes={} decode_errors={} last_error={:?} decoder={:?}",
            r.name,
            r.connects,
            r.connect_errors,
            r.disconnects,
            r.resyncs,
            r.refreshes,
            r.decode_errors,
            r.last_decode_error,
            r.decoder
        );
    }
    quantiles(
        "decode (socket read to canonical frame)",
        &mut s.latency.decode,
    );
    quantiles(
        "queue (frame ready to sequencer dequeue)",
        &mut s.latency.queue,
    );
    quantiles(
        "engine (sequence + apply + record, per frame)",
        &mut s.latency.engine,
    );
    let (replayed, journal) = replay_at(&dir, &texts, |_, _| {})?;
    if *replayed != *s.engine || journal.len() != s.commands {
        return Err("live and replayed engine states differ".into());
    }
    println!(
        "Live engine state and {} inventory commands reproduced exactly by replaying the recording. Fills are simulated; no orders were sent.",
        journal.len()
    );
    super::scenarios::summary(&replayed, "live")
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let (engine, journal) = replay_at(dir, &t, |_, _| {})?;
    println!(
        "replayed {} inventory commands; journal verified",
        journal.len()
    );
    super::scenarios::summary(&engine, "live-replay")
}
/// Policy replay (calibration): replays a recorded session through a FRESH engine built from
/// the recording's market and flow files with OVERRIDE structure and engine files. The corridor
/// (`lower_price`) is the recording's, since it is fixed per session. Input the live loop would
/// refuse (`Engine::accepts`) is skipped and counted, as live. There is no journal comparison:
/// the policy differs from the recorded one by design.
pub fn policy_replay(directory: &str, structures: &str, engine_file: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let recorded = read_dir_texts(dir)?;
    let read = |f: &str| fs::read_to_string(f).map_err(|e| format!("{f}: {e}"));
    let lower = super::structures::parse(&recorded.structures)?
        .0
        .lower_price
        .0;
    let structures = read(structures)?
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("lower_price") {
                format!("lower_price = {lower}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let t = Texts {
        market: recorded.market,
        structures,
        flow: recorded.flow,
        engine: read(engine_file)?,
    };
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(&t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    let (mut refused, mut in_snapshot, mut commands) = ([0_u64; 3], [false; 3], 0_u64);
    let (mut first, mut last) = (None, 0);
    let harvest_ticks = engine.config().policy.harvest_ticks;
    let (mut cycles, mut seen, mut completed) = (Vec::new(), std::collections::HashSet::new(), 0);
    let mut fault_at: Option<u64> = None;
    let mut next_tick = 0_u64;
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        let head = match frame {
            MergedFrame::Event(e) => e,
            MergedFrame::Batch(b) => b[0],
        };
        first.get_or_insert(head.receive_ts.0);
        last = head.receive_ts.0;
        let v = engine.venue_position(head.venue).ok_or("unknown venue")?;
        let start = head.event_type == market_events::MarketEventType::SnapshotStart;
        in_snapshot[v] |= start;
        if !engine.accepts(head.venue, head.receive_ts, in_snapshot[v]) {
            refused[v] += 1;
            continue;
        }
        if head.event_type == market_events::MarketEventType::SnapshotEnd {
            in_snapshot[v] = false;
        }
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |_| commands += 1),
            MergedFrame::Batch(b) => {
                engine.apply_depth_batch(head.venue, b, &mut |_| commands += 1)
            }
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        let elapsed = head.receive_ts.0 - first.unwrap_or(0);
        if elapsed >= next_tick {
            next_tick += 60_000_000_000;
            let m = engine.metrics();
            let vm = engine.research().research().voids().metrics();
            let active = engine
                .research()
                .research()
                .voids()
                .zones()
                .iter()
                .flatten()
                .filter(|z| z.state.active())
                .count();
            println!(
                "timeline minute={} mid_offset_ticks={} revisits={} active_zones={} entries={} maker_fills={} unqualified_steps={} disallowed_steps={} environment={:?} halt={:?}",
                elapsed / 60_000_000_000,
                engine
                    .reference_mid_x2()
                    .map_or("-".into(), |m| (m / 2 - i128::from(lower)).to_string()),
                vm.revisits,
                active,
                m.entries_placed,
                m.maker_fills,
                m.unqualified_revisit_steps,
                m.disallowed_scope_steps,
                engine.environment().state(),
                engine.ledger().halt_reason()
            );
        }
        if fault_at.is_none() && engine.research_fault().is_some() {
            fault_at = Some(head.receive_ts.0 - first.unwrap_or(0));
        }
        // Collect every closed child as it happens: the ledger keeps a bounded window only.
        if engine.ledger().counts().completed_cycles != completed {
            completed = engine.ledger().counts().completed_cycles;
            for c in engine.ledger().closed_lots().iter().flatten() {
                if !seen.insert(c.lot.id) {
                    continue;
                }
                let target = match c.lot.side {
                    common::Side::Buy => c.lot.entry_price.0 + harvest_ticks,
                    common::Side::Sell => c.lot.entry_price.0 - harvest_ticks,
                };
                cycles.push(super::cycles::Cycle {
                    venue: engine.venue_position(c.lot.venue).ok_or("unknown venue")?,
                    side: c.lot.side,
                    entry: c.lot.entry_price.0,
                    exit: c.exit_price.0,
                    hold_ns: c.exited_at.0 - c.lot.entered_at.0,
                    net_atoms: c.net_pnl.0,
                    harvest: c.mode == inventory::CloseMode::Normal && c.exit_price.0 == target,
                });
            }
        }
    }
    println!(
        "policy_replay seconds={:.1} lower_price={lower} refused_frames={refused:?} commands={commands} research_fault_at_s={}",
        (last - first.unwrap_or(last)) as f64 / 1e9,
        fault_at.map_or("-".into(), |t| format!("{:.1}", t as f64 / 1e9))
    );
    for c in &cycles {
        println!(
            "cycle venue={} side={:?} entry={} exit={} hold_ms={} net_atoms={} harvest={}",
            c.venue + 1,
            c.side,
            c.entry,
            c.exit,
            c.hold_ns / 1_000_000,
            c.net_atoms,
            c.harvest
        );
    }
    let open = engine.ledger().lots().iter().flatten().count();
    super::cycles::report(
        "engine_cycles",
        &cycles,
        open,
        engine.ledger().unrealized().0,
    );
    super::scenarios::summary(&engine, "policy-replay")
}
/// Cycle control (calibration): the engine's exit rules on UNGATED passive entries at every
/// venue touch, one long and one short cycler per venue, over a recorded session replayed
/// through its recorded configuration. See `cycles`.
pub fn cycle_control(
    directory: &str,
    harvest_ticks: &str,
    age_s: &str,
    reprice_ticks: &str,
    maker_fee_ppm: &str,
) -> Result<(), String> {
    let number = |s: &str, name: &str| -> Result<i64, String> {
        s.parse::<i64>()
            .ok()
            .filter(|&x| x >= 0)
            .ok_or(format!("{name} must be a non-negative integer"))
    };
    let harvest = number(harvest_ticks, "HARVEST_TICKS")?.max(1);
    let age = u64::try_from(number(age_s, "AGE_S")?).map_err(|e| e.to_string())?;
    let reprice = number(reprice_ticks, "REPRICE_TICKS")?.max(1);
    let fee = u32::try_from(number(maker_fee_ppm, "MAKER_FEE_PPM")?)
        .ok()
        .filter(|&f| f <= 100_000)
        .ok_or("MAKER_FEE_PPM must be at most 100000")?;
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(&t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    if engine
        .normalizers()
        .iter()
        .any(|n| n.price(PriceTicks(1)) != Ok(PriceTicks(1)))
    {
        return Err("the cycle control needs venue price scales equal to the grid".into());
    }
    let size = engine.ledger().config().quantum.unit_quantity().0;
    let mut control = super::cycles::Control::new(
        3,
        super::cycles::ControlConfig {
            harvest_ticks: harvest,
            age_ns: age * 1_000_000_000,
            reprice_ticks: reprice,
            model: execution::queue::CancelModel::Proportional,
            schedule: execution::fees::FeeSchedule {
                maker_fee_ppm: fee,
                maker_rebate_ppm: 0,
                taker_fee_ppm: 0,
            },
            size,
        },
    );
    let (mut in_snapshot, mut first, mut last) = ([false; 3], None, 0);
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        let head = match frame {
            MergedFrame::Event(e) => e,
            MergedFrame::Batch(b) => b[0],
        };
        let (venue, now) = (head.venue, head.receive_ts.0);
        first.get_or_insert(now);
        last = now;
        let v = engine.venue_position(venue).ok_or("unknown venue")?;
        in_snapshot[v] |= head.event_type == market_events::MarketEventType::SnapshotStart;
        if !engine.accepts(venue, head.receive_ts, in_snapshot[v]) {
            continue;
        }
        if head.event_type == market_events::MarketEventType::SnapshotEnd {
            in_snapshot[v] = false;
        }
        let print = head.event_type == market_events::MarketEventType::Trade;
        let before = if print {
            None
        } else {
            Some(control.depths(&*engine, v)?)
        };
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |_| {}),
            MergedFrame::Batch(b) => engine.apply_depth_batch(venue, b, &mut |_| {}),
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        match before {
            None => control.on_trade(
                &*engine,
                v,
                now,
                head.side,
                head.price_ticks,
                head.qty_units,
            )?,
            Some(before) => control.after_depth(&*engine, v, &before)?,
        }
        control.after_frame(&*engine, now)?;
    }
    println!(
        "cycle_control seconds={:.1} harvest_ticks={harvest} age_s={age} reprice_ticks={reprice} maker_fee_ppm={fee} probe_units={size} research_fault={:?}",
        (last - first.unwrap_or(last)) as f64 / 1e9,
        engine.research_fault()
    );
    let (open, mtm) = control.open(&*engine)?;
    for c in &control.cycles {
        println!(
            "cycle venue={} side={:?} entry={} exit={} hold_ms={} net_atoms={} harvest={}",
            c.venue + 1,
            c.side,
            c.entry,
            c.exit,
            c.hold_ns / 1_000_000,
            c.net_atoms,
            c.harvest
        );
    }
    for v in 0..3 {
        let by: Vec<_> = control
            .cycles
            .iter()
            .filter(|c| c.venue == v)
            .copied()
            .collect();
        super::cycles::report(&format!("control_cycles venue={}", v + 1), &by, 0, 0);
    }
    super::cycles::report("control_cycles all", &control.cycles, open, mtm);
    Ok(())
}
/// Replays a recording through its recorded configuration with the live `accepts` gate.
/// `visit(engine, members, applied)` runs before (`false`) and after (`true`) every applied frame;
/// `members` is the frame's events (one for a single event).
pub(super) fn replay_with(
    directory: &str,
    mut visit: impl FnMut(&LiveEngine, &[market_events::MarketEvent], bool) -> Result<(), String>,
) -> Result<(), String> {
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(&t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    if engine
        .normalizers()
        .iter()
        .any(|n| n.price(PriceTicks(1)) != Ok(PriceTicks(1)))
    {
        return Err("replay studies need venue price scales equal to the grid".into());
    }
    let mut in_snapshot = [false; 3];
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        let single;
        let members: &[market_events::MarketEvent] = match frame {
            MergedFrame::Event(e) => {
                single = [e];
                &single
            }
            MergedFrame::Batch(b) => b,
        };
        let head = members[0];
        let v = engine.venue_position(head.venue).ok_or("unknown venue")?;
        in_snapshot[v] |= head.event_type == market_events::MarketEventType::SnapshotStart;
        if !engine.accepts(head.venue, head.receive_ts, in_snapshot[v]) {
            continue;
        }
        if head.event_type == market_events::MarketEventType::SnapshotEnd {
            in_snapshot[v] = false;
        }
        visit(&engine, members, false)?;
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |_| {}),
            MergedFrame::Batch(b) => engine.apply_depth_batch(head.venue, b, &mut |_| {}),
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        visit(&engine, members, true)?;
    }
    Ok(())
}
/// Time series for reports: every `interval_ms` of session time, the composite and venue
/// midpoints as tick offsets from the corridor floor (prices relative to the research corridor,
/// not absolute), the environment, cumulative revisits and active zones. Replays through the
/// recorded configuration, skipping input `Engine::accepts` refuses, as live.
pub fn series(directory: &str, interval_ms: &str) -> Result<(), String> {
    let step = interval_ms
        .parse::<u64>()
        .ok()
        .filter(|&s| s >= 100)
        .ok_or("INTERVAL_MS must be an integer of at least 100")?
        * 1_000_000;
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let lower = i128::from(super::structures::parse(&t.structures)?.0.lower_price.0);
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(&t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    println!("t_s,corridor_ticks,mid,binance,bybit,okx,environment,revisits,active_zones");
    let (mut in_snapshot, mut first, mut next) = ([false; 3], None, 0_u64);
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        let head = match frame {
            MergedFrame::Event(e) => e,
            MergedFrame::Batch(b) => b[0],
        };
        let start = *first.get_or_insert(head.receive_ts.0);
        let v = engine.venue_position(head.venue).ok_or("unknown venue")?;
        in_snapshot[v] |= head.event_type == market_events::MarketEventType::SnapshotStart;
        if !engine.accepts(head.venue, head.receive_ts, in_snapshot[v]) {
            continue;
        }
        if head.event_type == market_events::MarketEventType::SnapshotEnd {
            in_snapshot[v] = false;
        }
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |_| {}),
            MergedFrame::Batch(b) => engine.apply_depth_batch(head.venue, b, &mut |_| {}),
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        let elapsed = head.receive_ts.0 - start;
        if elapsed < next {
            continue;
        }
        next = elapsed - elapsed % step + step;
        let offset = |x2: Option<i128>| {
            x2.map_or(String::new(), |m| {
                format!("{:.1}", m as f64 / 2.0 - lower as f64)
            })
        };
        let market = engine.research().research().market();
        let venue = |id| market.venue_midpoint_x2(common::VenueId(id)).ok().flatten();
        let voids = engine.research().research().voids();
        println!(
            "{:.1},{CORRIDOR},{},{},{},{},{:?},{},{}",
            elapsed as f64 / 1e9,
            offset(engine.reference_mid_x2()),
            offset(venue(1)),
            offset(venue(2)),
            offset(venue(3)),
            engine.environment().state(),
            voids.metrics().revisits,
            voids
                .zones()
                .iter()
                .flatten()
                .filter(|z| z.state.active())
                .count()
        );
    }
    Ok(())
}
/// Fill study (Phase 9) over a recorded session: shadow maker probes at every venue touch
/// under each fill model (see `study`). The recorded engine configuration is replayed
/// unchanged; probes never reach its ledger.
pub fn fill_study(
    directory: &str,
    requote_ticks: &str,
    maker_fee_ppm: &str,
    latency_ms: &str,
    lag_ticks: Option<&str>,
) -> Result<(), String> {
    let lag: Option<i64> = match lag_ticks {
        None => None,
        Some(x) => Some(
            x.parse()
                .ok()
                .filter(|&l: &i64| l >= 0)
                .ok_or("LAG_TICKS must be a non-negative integer")?,
        ),
    };
    let requote: i64 = requote_ticks
        .parse()
        .ok()
        .filter(|&t| t >= 1)
        .ok_or("REQUOTE_TICKS must be a positive integer")?;
    let fee: u32 = maker_fee_ppm
        .parse()
        .ok()
        .filter(|&f| f <= 100_000)
        .ok_or("MAKER_FEE_PPM must be an integer in 0..=100000")?;
    let latency: u64 = latency_ms
        .parse::<u64>()
        .ok()
        .filter(|&l| l <= 10_000)
        .ok_or("LATENCY_MS must be an integer in 0..=10000")?;
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let (grid, _) = super::multi::parse(&t.market)?;
    let units_per_base =
        10_f64.powi(i32::from(grid.quantity.decimals)) / grid.quantity.atoms as f64;
    let files: Vec<_> = (1..=3)
        .map(|v| File::open(dir.join(format!("venue-{v}.lre"))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedFrames::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(&t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    // Probe prices are grid prices looked up in native books.
    if engine
        .normalizers()
        .iter()
        .any(|n| n.price(PriceTicks(1)) != Ok(PriceTicks(1)))
    {
        return Err("the fill study needs venue price scales equal to the grid".into());
    }
    let size = engine.ledger().config().quantum.unit_quantity().0;
    let mut study = super::study::Study::new(
        3,
        requote,
        latency * 1_000_000,
        size,
        engine.config().markout,
        lag,
    );
    let (mut first, mut last) = (None, 0);
    while let Some(frame) = merged.next_frame().map_err(|e| format!("{e:?}"))? {
        let (venue, now) = match frame {
            MergedFrame::Event(e) => (e.venue, e.receive_ts.0),
            MergedFrame::Batch(b) => (b[0].venue, b[0].receive_ts.0),
        };
        first.get_or_insert(now);
        last = now;
        let v = engine.venue_position(venue).ok_or("unknown venue")?;
        let print = matches!(frame, MergedFrame::Event(e) if e.event_type == market_events::MarketEventType::Trade);
        let before = if print {
            None
        } else {
            Some(study.depths(&*engine, v)?)
        };
        match frame {
            MergedFrame::Event(e) => engine.apply(&e, &mut |_| {}),
            MergedFrame::Batch(b) => engine.apply_depth_batch(venue, b, &mut |_| {}),
        }
        .map_err(|e| format!("engine: {e:?}"))?;
        match (frame, before) {
            (MergedFrame::Event(e), None) => {
                study.on_trade(&*engine, v, now, e.side, e.price_ticks, e.qty_units)?
            }
            (_, Some(before)) => study.after_depth(&*engine, v, &before)?,
            (MergedFrame::Batch(_), None) => unreachable!("batches carry no prints"),
        }
        study.after_frame(&*engine, now)?;
    }
    let seconds = (last - first.unwrap_or(last)) as f64 / 1e9;
    println!(
        "fill_study directory={directory} seconds={seconds:.1} requote_ticks={requote} latency_ms={latency} lag_filter_ticks={} probe_units={size} maker_fee_ppm={fee} engine_research_fault={:?}",
        lag.map_or("off".into(), |l| l.to_string()),
        engine.research_fault()
    );
    super::study::report(
        &mut study,
        &["binance", "bybit", "okx"],
        seconds,
        units_per_base,
        fee,
    );
    Ok(())
}
/// Lead/lag (specification §28) on ONE local monotonic clock: per-venue midpoint changes and
/// signed aggressive volume are binned by sequencer receive time, then correlated at lags.
/// This measures lead/lag AS OBSERVED HERE, including each venue's network path; it is not
/// exchange-side causality.
pub fn leadlag(directory: &str, bin_ms: &str, max_lag: &str) -> Result<(), String> {
    let bin: u64 = bin_ms
        .parse::<u64>()
        .map_err(|_| "BIN_MS must be an integer")?
        * 1_000_000;
    let max_lag: usize = max_lag.parse().map_err(|_| "MAX_LAG must be an integer")?;
    if bin == 0 || max_lag == 0 {
        return Err("BIN_MS and MAX_LAG must be positive".into());
    }
    let dir = Path::new(directory);
    let t = read_dir_texts(dir)?;
    let mut origin = None;
    let mut mids: [Vec<Option<i128>>; 3] = Default::default();
    let mut flow: [Vec<i128>; 3] = Default::default();
    replay_at(dir, &t, |engine, frame| {
        let first = match frame {
            MergedFrame::Event(e) => e,
            MergedFrame::Batch(b) => b[0],
        };
        let t0 = *origin.get_or_insert(first.receive_ts.0);
        let k = ((first.receive_ts.0 - t0) / bin) as usize;
        for series in mids.iter_mut().chain([]) {
            if series.len() <= k {
                let last = series.last().copied().flatten();
                series.resize(k + 1, last);
            }
        }
        for f in flow.iter_mut() {
            if f.len() <= k {
                f.resize(k + 1, 0);
            }
        }
        for (v, series) in mids.iter_mut().enumerate() {
            series[k] = engine
                .research()
                .research()
                .market()
                .venue_midpoint_x2(VenueId(v as u16 + 1))
                .ok()
                .flatten()
                .or(series[k]);
        }
        if let MergedFrame::Event(e) = frame {
            if e.event_type == market_events::MarketEventType::Trade {
                let v = usize::from(e.venue.0 - 1);
                let signed = i128::from(e.qty_units.0);
                flow[v][k] += if e.side == common::Side::Buy {
                    signed
                } else {
                    -signed
                };
            }
        }
    })?;
    let diffs: Vec<Vec<f64>> = mids
        .iter()
        .map(|m| {
            m.windows(2)
                .map(|w| match (w[0], w[1]) {
                    (Some(a), Some(b)) => (b - a) as f64,
                    _ => 0.0,
                })
                .collect()
        })
        .collect();
    let flows: Vec<Vec<f64>> = flow
        .iter()
        .map(|f| f.iter().map(|&x| x as f64).collect())
        .collect();
    let names = ["binance", "bybit", "okx"];
    println!(
        "lead/lag on one local clock: {} bins of {} ms; positive lag = row venue leads column venue",
        diffs[0].len(),
        bin / 1_000_000
    );
    for (label, series) in [
        ("midpoint change", &diffs),
        ("signed aggressive volume", &flows),
    ] {
        for a in 0..3 {
            for b in 0..3 {
                if a == b {
                    continue;
                }
                let best = (-(max_lag as i64)..=max_lag as i64)
                    .filter_map(|lag| correlation(&series[a], &series[b], lag).map(|c| (lag, c)))
                    .max_by(|x, y| x.1.total_cmp(&y.1));
                let zero = correlation(&series[a], &series[b], 0);
                match best {
                    Some((lag, c)) => println!(
                        "{label}: {} vs {}: best lag {lag} bins ({} ms) rho={c:.3}; rho at 0 = {:.3}",
                        names[a],
                        names[b],
                        lag * (bin / 1_000_000) as i64,
                        zero.unwrap_or(f64::NAN)
                    ),
                    None => println!(
                        "{label}: {} vs {}: insufficient variation",
                        names[a], names[b]
                    ),
                }
            }
        }
    }
    Ok(())
}
/// Pearson correlation of `x[t]` with `y[t + lag]`; `None` without variation.
fn correlation(x: &[f64], y: &[f64], lag: i64) -> Option<f64> {
    let n = x.len().min(y.len()) as i64;
    let pairs: Vec<(f64, f64)> = (0..n)
        .filter_map(|t| {
            let u = t + lag;
            (0..n).contains(&u).then(|| (x[t as usize], y[u as usize]))
        })
        .collect();
    if pairs.len() < 3 {
        return None;
    }
    let m = pairs.len() as f64;
    let (mx, my) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / m,
        pairs.iter().map(|p| p.1).sum::<f64>() / m,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in pairs {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
        syy += (b - my) * (b - my);
    }
    (sxx > 0.0 && syy > 0.0).then(|| sxy / (sxx * syy).sqrt())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_configuration_parses_and_builds() {
        let texts = Texts {
            market: include_str!("../../../config/phase8-market.toml").into(),
            structures: include_str!("../../../config/phase8-structures.toml").into(),
            flow: include_str!("../../../config/phase8-flow.toml").into(),
            engine: include_str!("../../../config/phase8-engine.toml").into(),
        };
        thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || build(&texts).map(|_| ()))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
    }
    #[test]
    fn phase9_configuration_builds_the_queue_model_with_retail_fees() {
        let texts = Texts {
            market: include_str!("../../../config/phase8-market.toml").into(),
            structures: include_str!("../../../config/phase8-structures.toml").into(),
            flow: include_str!("../../../config/phase8-flow.toml").into(),
            engine: include_str!("../../../config/phase9-engine.toml").into(),
        };
        let config = thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || build(&texts).map(|e| e.config()))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(
            config.fills.model,
            engine::FillModel::Queue(execution::queue::CancelModel::Proportional)
        );
        assert_eq!(
            (
                config.fills.schedule.maker_fee_ppm,
                config.fills.schedule.taker_fee_ppm
            ),
            (200, 500)
        );
        assert_eq!(config.markout, execution::markout::MarkoutConfig::spec());
        assert_eq!(config.funding.interval_ns, 8 * 3_600 * 1_000_000_000);
        assert_eq!(config.objective.map(|o| o.adverse_horizon), Some(8));
        // The Phase 8 file still parses to the strict rule with no schedule or objective.
        let (_, phase8) =
            super::super::scenarios::parse(include_str!("../../../config/phase8-engine.toml"))
                .unwrap();
        assert_eq!(phase8.fills.model, engine::FillModel::StrictTradeThrough);
        assert!(phase8.objective.is_none() && phase8.funding.interval_ns == 0);
    }
    #[test]
    fn correlation_finds_a_known_lead() {
        let x: Vec<f64> = (0..200).map(|i| ((i * 37 % 11) as f64) - 5.0).collect();
        let mut y = vec![0.0; 200];
        y[3..].copy_from_slice(&x[..197]);
        let best = (-5..=5)
            .filter_map(|lag| correlation(&x, &y, lag).map(|c| (lag, c)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        assert_eq!(best.0, 3);
        assert!(best.1 > 0.99);
        assert!(correlation(&[1.0; 10], &x[..10], 0).is_none());
    }
}
