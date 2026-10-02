# Phase 8: native public feeds, atomic batches, recording v2 and exact replay

## Scope

Phase 8 connects three live public feeds to the unchanged engine:

- Binance USDⓈ-M BTCUSDT
- Bybit linear BTCUSDT
- OKX BTC-USDT-SWAP

Each session records the canonical stream in a batch-preserving format and requires replay to reproduce the live engine **exactly**. It also adds cross-venue lead/lag tooling and stage latency measurement.

All of this is public market data only. No account or key is used, fills remain **simulated** (Phase 7 strict trade-through), and no order is ever sent.

| Crate | Content |
|---|---|
| `wire` | Allocation-free JSON scanner, exact decimal parsing, top-K window canonicalizer, shared decoder vocabulary. |
| `venue-binance`, `venue-bybit`, `venue-okx` | Pure decoders and documented sync rules; no network. |
| `feeds` | WebSocket feed threads (tungstenite + rustls/ring), the shared monotonic `Clock`, and the `Sequencer`. |
| `consolidator`, `liquidity`, `orderflow`, `engine` | Incremental atomic batch path; composite price reference. |
| `recorder`, `replay` | Recording format v2 frames; frame-aware `MergedFrames`. |
| `cli` | `live-record`, `live-replay`, `leadlag`. |

## Live data path

```text
4 feed threads (one per WebSocket)          single engine thread
binance-depth  ─┐  decode → top-K diff     ┌─ Sequencer: strictly increasing receive_ts,
binance-trades ─┤  → fixed WireFrame  ──►  │  contiguous per-venue canonical sequences
bybit          ─┤   bounded channel        │  → Engine::apply / apply_depth_batch
okx            ─┘   (4096 frames)          └─ FrameRecorder per venue + inventory journal
```

Binance routes depth and trades on different paths (`/public`, `/market`), so it needs two connections. Each feed thread connects, subscribes and decodes, then sends frames with a **blocking** send: a slow consumer back-pressures the socket instead of dropping data. On any fault it sends a Reset frame (book sources only) and reconnects after 500 ms. The faults are:

- a decode error
- a sync gap
- a crossed or one-sided window
- a socket close or error

**Keepalive.** Bybit is sent `{"op":"ping"}` and OKX `ping`, both every 20 s. Binance's protocol pings are answered automatically. Socket reads time out after 500 ms so the thread can check its stop flag.

**Canonical receive time is the sequencer's stamp.** The consolidator has one global monotonic clock, and three threads stamping independently would race. The sequencer therefore stamps each frame at dequeue from one process-wide monotonic clock, bumping it to stay strictly increasing. All events of a frame share that stamp.

Merged replay orders by `(receive_ts, venue, sequence)`, so replay order equals live processing order exactly. The socket read time and decode completion time travel in the frame for latency measurement only.

**Gating.** The sequencer tracks whether each venue's book is live: a Snapshot sets it, a Reset clears it. A trade or batch for a venue without a live book is **dropped and counted**. Live OKX sent prints before its first book snapshot; forwarding them would be invalid canonical input, and the Phase 4 wrapper latches any error for the whole engine.

**Resets emit no event.** A venue that loses continuity keeps its last book only until the consolidator's staleness rule withdraws it (2 s), and the snapshot after resubscription rebuilds it. Because this happens in receive time, replay reproduces it without a new event kind.

**Quiet windows refresh freshness.** A valid native message that leaves the top-K window unchanged emits nothing, and Bybit's 20 ms deltas often change only levels beyond the top 20. The first 5-minute session showed that a healthy venue whose visible window stayed unchanged for 2 s was withdrawn as stale. The next real change then reached a withdrawn book, and the Phase 4 wrapper faulted the whole engine.

A book source therefore sends at most one **refresh** every 250 ms while its messages are valid but unchanged. A refresh is a zero-change `Modify` of the best bid at its current size: a legitimate canonical "book confirmed" event, recorded and replayed like any other. It is the same form the synthetic generator's idle steps use, and Phase 5 counts it as an eligible event with zero contribution.

**Withdrawal triggers resynchronization.** Before applying a trade or batch, the engine thread checks the venue's engine book. If staleness has withdrawn it, the step is dropped and counted, the sequencer marks the venue not-live, and the feed thread is told to reconnect. Resubscription yields a fresh snapshot, which is always applied. Dropped steps are never recorded, so replay sees the same canonical stream.

The engine thread also performs buffered recording I/O. A dedicated recorder thread is a known improvement.

## Native sync rules

These rules are taken from [providers](providers.md) and verified against live traffic on 2026-10-02.

| Venue | Stream | Rule |
|---|---|---|
| Binance | `btcusdt@depth20@100ms` | Every message is a **complete top-20 snapshot**. It replaces the native book and is diffed against the emitted view. `u` must strictly increase; older or repeated messages are dropped. `pu ≠ previous u` is counted as skipped; live, `pu` chained message to message. |
| Binance | `btcusdt@aggTrade` | `m = true` means the buyer was maker, so the aggressor sold. |
| Bybit | `orderbook.50.BTCUSDT` | Snapshot on subscribe, or `u = 1` after a restart; deltas carry absolute sizes and 0 deletes. A delta `u` must be `previous + 1`; `≤` is a duplicate, `>` is a gap and forces a resync. A backwards `seq` is dropped. |
| Bybit | `publicTrade.BTCUSDT` | `S` is the taker side. Block (`BT`) and Retail Price Improvement (`RPI`) prints are excluded. Up to 1024 prints arrive per message; they are split into frames of at most 128 prints, and none is lost. |
| OKX | `books` (400 levels, 100 ms, public) | The snapshot has `prevSeqId = -1`. An update must satisfy `prevSeqId == previous seqId`; `seqId` may decrease after maintenance. Empty with `prevSeqId == seqId` is a heartbeat. The checksum is deprecated (always 0) and not validated. Sizes are in **contracts**. |
| OKX | `trades` | `side` is the taker side; `source = 1` (RPI) is excluded. |

**Window choice.** OKX tick-by-tick L2 needs login and VIP4, so `books` is used.

**Message handling.** Every message is parsed completely into a preallocated buffer and validated (symbol, fields, exact decimals) before the native book changes. A price that is not on the tick grid fails decoding visibly. Trailing fractional zeros are accepted exactly (`"86779.90"` at 0.1 tick).

## Top-K window canonicalization

Each venue keeps a bounded native book: up to 64, 256 or 1024 levels per side, larger than the venue's window. After each message the canonicalizer emits the change of the **top-K view** (K = 20) since the last emission as one atomic batch, ordered cancels, then modifies, then adds. The first emission after a reset is a complete snapshot.

The engine therefore holds exactly each venue's top 20 levels per side: a complete, contiguous window. The Phase 4 coverage contract ("every price between the best and worst retained level is present or truly empty") holds within it.

Limits of the window:

- **Window exits.** A level leaving the window appears as a removal, which is an artifact of the window, not a cancellation. Removal attribution stays `unknown`.
- **Depth.** K = 20 is the Binance partial-stream maximum, so live structure is measured within roughly 2–5 USDT of the touch.
- **Deeper books** would need Binance's diff stream with a REST snapshot and periodic re-snapshotting. Not implemented.

A generated test applies 5,000 random native updates and checks that applying every emitted batch to an independent mirror reproduces the top-K view exactly.

## Atomic batches through every layer

Native messages change several levels at once, so the engine needs an incremental atomic batch path. Phase 3's `apply_depth_batch` withdrew and reinserted the venue: correct but structural. On live data every message would have rebuilt research state and invalidated every void. The path is now incremental at every layer:

- **Consolidator.** Validates and normalizes every member and computes its pre-batch weighted delta into an inline scratch array of G entries. The venue book then commits the batch atomically through its staging copy, and only then does the consolidated ladder receive one delta per member. The structural revision is unchanged. A level repeated within a batch is rejected (its old size would be ambiguous), and a rejected batch withdraws and invalidates only that venue.
- **Research (Phase 4).** Updates every changed grid level, then samples formation and observes zones **once** against the committed state.
- **Flow (Phase 5).** Computes best-quote OFI once from the touches before and after the batch, carried by the first member's contribution. Every member records its own depth change and add or remove, bucketed by the pre-batch fresh reference.
- **Engine (Phase 7).** `apply_depth_batch` observes, marks, assesses and runs policy once per batch. Batches carry no prints, so there are no fills.

Tests:

- A batch gives the same books and consolidated depth as its members applied singly, with an unchanged structural revision.
- Scenario A delivered as more than 100 batches keeps zero invalidations, the same final books and the per-event invariants.
- Batched scenario E, recorded in format v2 and replayed through `MergedFrames`, reproduces the complete engine state and journal.

## Composite price reference

Live, the consolidated touch is crossed or locked across venues most of the time, through basis and unequal feed delays. The first 60 s session spent 59.8 s in `LiquidityShock` because Phases 4–5 suspend research unless the consolidated market is open. A venue midpoint differed from the consolidated midpoint by up to 290 ticks (29 USDT).

`price_reference = "composite"` (`PriceReference::Composite`) changes three things:

- **Reference midpoint.** The weight-averaged doubled midpoint of every included, positive-weight venue with a two-sided book, floored. Each venue's own book is never crossed.
- **Spread.** The widest of those venues' spreads, used for conservative shock detection.
- **Gating.** Research suspends only when no venue has a valid book. Phase 5 bucket attribution and the Phase 7 environment use the same reference.

`midpoint` and `last_trade` behave exactly as before, so every Phase 4–7 test is unchanged. A test shifts venue 1 three ticks above the others in scenario A, crossing the consolidated touch throughout:

| Reference | Formation samples | Void registered | Liquidity-shock time |
|---|---|---|---|
| Midpoint | 65 | none | > 90% |
| Composite | 2,648 | multi-venue void registered, 14 revisits | < 10% |

## Recording format v2

Version 2 keeps the v1 header with version 2 and stores **frames**. Each frame is a 16-byte header followed by its member records in the unchanged 60-byte v1 event layout:

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 4 | `LRFR` |
| 4 | 1 | 0 single event, 1 atomic depth batch |
| 5 | 3 | reserved, zero |
| 8 | 4 | member count (1..=4096; exactly 1 for a single event) |
| 12 | 4 | CRC32 of bytes 0–11 |

**Batch contract,** enforced by both writer and reader:

- same venue and instrument as the header
- same receive time and native update ID
- depth events only
- contiguous canonical sequences

**Readers** accept v1 (implicit single-event frames) and v2. `next_event` refuses v2 so batches are never silently flattened, and `next_frame` returns either an event or a batch written into the caller's buffer. Any header byte flip, a buffer too small, truncation inside a frame, or a contract break with valid CRCs is rejected; a cut between frames is a clean end.

**Replay.** `MergedFrames` merges version 1 or 2 recordings with one 4096-member buffer per reader, allocated at construction.

## Live commands

```sh
cargo run -p cli --release --locked -- live-record config/phase8-market.toml config/phase8-structures.toml config/phase8-flow.toml config/phase8-engine.toml <fresh-dir> 300
cargo run -p cli --release --locked -- live-replay <fresh-dir>
cargo run -p cli --release --locked -- leadlag <fresh-dir> 50 10
```

`live-record` (5 to 3,600 s) runs these steps:

1. Waits for the first book snapshot.
2. Centres the 1,024-tick (102.4 USDT) research corridor on its midpoint and saves the effective `lower_price` in `structures.toml`.
3. Runs the session.
4. Writes `venue-1/2/3.lre` (v2) and `inventory.lri`.
5. Replays them through a fresh engine and requires identical complete engine state, an identical command count, and the journal file rebuilding the identical ledger.

It prints per-source connection, decoder and drop statistics, three stage latencies, and the full engine summary.

**Common grid.** Price tick 0.1 USDT and quantity unit 0.0001 BTC; every venue maps exactly onto it.

- Binance and Bybit native steps are 0.001 BTC, or 10 units.
- An OKX native lot of 0.01 contracts, at `ctVal` 0.01 BTC, is 1 unit.

**Engine thresholds** in `config/phase8-*.toml` are first estimates and **not calibrated**. Fees model retail economics (makers pay about 0.02%, takers about 0.05%).

**Corridor.** It is fixed per session; BTC moving more than about 51 USDT from the start leaves it, and research outside it stops (a Phase 4 limitation). Sessions are therefore limited to an hour.

**Data terms.** Recordings contain exchange market data. Keep them out of the repository and do not redistribute them (OKX API Agreement §9.4; Binance Vision terms). The tests use synthetic messages in each venue's documented format, never stored exchange data.

## Lead/lag (§28)

`leadlag DIR BIN_MS MAX_LAG` replays a session and bins two series per venue by **sequencer receive time**: midpoint changes and signed aggressive volume. It reports the Pearson correlation ρ(Δx_A(t), Δx_B(t+τ)) for τ in ±MAX_LAG bins; a positive best lag means the row venue leads. Floating point is used here only, in research tooling outside the engine.

This measures lead/lag **as observed at this machine**, on one clock. That includes each venue's network path and **push cadence**: Bybit's book updates every 20 ms, Binance's and OKX's every 100 ms. A cadence advantage looks like a lead, so it is not exchange-side price discovery.

OFI, queue-imbalance and depth-change lead/lag, and significance testing, are not implemented.

## Latency stages (§30)

Measured per frame:

| Stage | From | To |
|---|---|---|
| decode | socket read returned | canonical frame ready (feed thread) |
| queue | frame ready | sequencer dequeue (channel and wake-up) |
| engine | sequencing start | sequencing, every engine call and buffered recording done |

Stages not yet separated are book versus statistics versus decision inside the engine, order construction, gateway and acknowledgement (no orders exist).

## Limits

- **Coverage.** Top-20 windows only; one instrument per venue; three fixed venues.
- **Corridor.** Fixed per session, not recentred.
- **Recording.** The I/O runs on the engine thread. The channel is one multi-producer, single-consumer `std::sync::mpsc::sync_channel`, not per-venue single-producer, single-consumer rings. There is no CPU pinning.
- **Allocation.** Feed threads allocate inside the WebSocket library (one buffer per message). The sequencer and engine path allocate nothing after startup, and the latency sample vectors are preallocated.
- **Network failures.** Reconnect, resync and staleness handling are implemented and unit-tested at the decoder level. Network failure itself was not fault-injected in Phase 8.
- **Not implemented:**
  - REST instrument-metadata verification at startup (scales are configured and checked by exact decoding)
  - Tiingo reference feed
  - exchange clock-offset measurement
