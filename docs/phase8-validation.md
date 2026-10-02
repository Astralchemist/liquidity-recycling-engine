# Phase 8 validation — 2026-10-02

## Correctness gates

**131 tests passed** with `cargo test --workspace --locked`. Formatting and `cargo clippy --workspace --all-targets --locked -- -D warnings` passed. The [test log](phase8-tests-output.txt) and [Clippy log](phase8-clippy-output.txt) are retained.

This milestone adds 26 tests:

- **Atomic batches (3).**
  - A consolidator batch equals its members applied singly (cancels, then modifies, then adds), with an unchanged structural revision; a repeated level withdraws only that venue.
  - Scenario A delivered as more than 100 batches keeps zero invalidations, the same final books and the per-event engine invariants.
  - Batched scenario E recorded in format v2 replays to the identical engine state and journal.
- **Composite reference (1).** Venue 1 is shifted three ticks so the consolidated touch is crossed throughout. With the midpoint reference: 65 formation samples, no void registered, more than 90% liquidity-shock time. With composite: 2,648 samples, a multi-venue void registered and revisited, under 10% shock time, and the composite midpoint and spread are exact. Only unregistered candidates are invalidated.
- **Recording v2 (4).**
  - Mixed event and batch round-trip, and `next_event` refuses v2.
  - v1 files read as single-event frames.
  - Every batch-contract violation is rejected on write, and a rejected write leaves nothing behind.
  - On read, every frame-header byte flip, a too-small buffer, truncation inside a frame, and a crafted contract break with valid CRCs are rejected; a cut between frames is a clean end.
- **Wire (5).**
  - The JSON scanner walks nested documents.
  - Malformed input, escapes, excess depth, trailing data and inexact decimals are rejected; trailing zeros are exact.
  - The window emits a snapshot then ordered diffs, rejects crossed, one-sided and over-capacity states, and refreshes.
  - 5,000 random native updates keep an independent mirror equal to the top-K view.
- **Venues (10).**
  - Binance: snapshot then diffs, stale `u` dropped, skipped `pu` counted, maker flag mapped to aggressor side, envelope and control messages, wrong symbol, off-tick price, crossed window resyncs.
  - Bybit: snapshot, contiguous deltas, duplicates, backwards `seq`, gaps resync, restart with `u=1`, taker side, block and RPI prints excluded, acks and failures, a 300-print burst split across frames with none lost.
  - OKX: snapshot, linked updates, heartbeat, `seqId` decrease, broken link resyncs, contract sizes, RPI excluded, `pong`, subscribe and error events.
- **Sequencer (1).** Strictly increasing receive times even when clock readings repeat or regress; contiguous per-venue sequences, snapshot markers, batches; prints before a snapshot and batches after a reset are dropped.
- **CLI (2).** The live configuration builds an engine; the correlation tool recovers a known three-bin lead.

The tests use synthetic messages in each venue's documented format. No recorded exchange data is stored in the repository.

## Defects found by live data

Each of these was invisible in synthetic data, and each was fixed and re-verified against live feeds:

1. **Prints before a snapshot.** OKX sent prints before its first book snapshot. Forwarding them is invalid canonical input, and the Phase 4 wrapper faulted the whole engine after about 1.8 s. The sequencer now gates trades and batches on a live venue book.
2. **Large trade bursts.** Bybit sent more trades in one message than a 128-change frame holds; the decode error forced a reconnect. Prints now leave in chunks via `drain`.
3. **Crossed consolidated touch.** A venue midpoint differed from the consolidated midpoint by up to 290 ticks (29 USDT). The consolidated market was not open most of the time, so the environment spent 59.8 of 60 s in liquidity shock and research was suspended. The composite price reference was added; `midpoint` and `last_trade` are unchanged.
4. **Quiet windows withdrawn as stale.** A healthy Bybit feed whose top-20 window stayed unchanged for 2 s was withdrawn as stale, and the next change faulted the engine about halfway through a 5-minute session. The fixes are refresh heartbeats (at most one zero-change `Modify` per 250 ms) and resynchronization when the engine finds a venue withdrawn.

## Live sessions

**Setup:** BTC/USDT perpetuals on Binance USDⓈ-M, Bybit linear and OKX swap; public endpoints from this machine; release build; the configs in `config/phase8-*.toml`. No account was used and no order was sent. The [raw output](phase8-live-output.txt) is from the final 300 s session.

| 300.2 s session | Binance depth | Binance trades | Bybit | OKX |
|---|---:|---:|---:|---:|
| Messages | 2,881 | 13,301 | 19,790 | 17,176 |
| Frames forwarded | 2,881 | 13,301 | 18,508 | 17,011 |
| Unchanged inside the window | 0 | — | 1,267 | 8 |
| Refresh heartbeats | 0 | — | 7 | 0 |
| Connects / disconnects | 1 / 0 | 1 / 0 | 1 / 0 | 2 / 1 |
| Decode errors | 0 | 0 | 0 | 0 |
| Sync gaps / resyncs | 0 | — | 0 | 0 gaps, 1 engine-requested resync |
| Excluded prints (RPI) | — | 0 | 0 | 140 |

**OKX disconnect.** OKX dropped its connection once and was recovered automatically:

- The feed sent a Reset frame.
- 3 prints arrived before the new snapshot and were dropped.
- 1 step reached the withdrawn engine book and was dropped.
- A resync was requested and the snapshot rebuilt the book.

**Exact replay.** The engine processed **265,294 canonical events** (51,704 frames) without a research fault or halt. Replaying the three v2 recordings through a fresh engine reproduced the **complete engine state and all 4,134 inventory commands exactly**, and the journal file rebuilt the identical ledger. A separate `live-replay` reproduced the summary.

| Environment, 300 s | Dead | Balanced-active | Trending | Liquidity shock | Chaotic |
|---|---:|---:|---:|---:|---:|
| Seconds | 12.3 | 160.0 | 122.1 | 5.6 | 0 |

**Voids** (top-20 windows, 4-tick cells, uncalibrated thresholds):

- 277 candidates and 6 registered.
- **0 revisited**; 1 refilled before any revisit.
- 276 invalidated: candidates whose depth recovered, plus registered voids censored by the structural rebuild after the OKX reconnect.

No revisit means no entry was placed and the simulated inventory stayed flat. This answers nothing about the strategy. It shows that at a 2–5 USDT window, these thresholds rarely produce persistent voids within 5 minutes.

**Cross-venue.** Maximum venue divergence was 381 doubled ticks (19 USDT), with 59,090 divergent events.

## Lead/lag (§28)

Each session was analysed on one local clock. ρ is the Pearson correlation of midpoint changes; "lead" is the best lag within ±10 bins.

| Session | Bin | Bybit vs Binance | Bybit vs OKX | Binance vs OKX |
|---|---|---|---|---|
| First 60 s | 50 ms | Bybit leads 100 ms, ρ 0.355 (0.152 at 0) | Bybit leads 100 ms, ρ 0.317 | 0 ms, ρ 0.316 |
| Earlier 300 s (later faulted) | 50 ms | Bybit leads 50 ms, ρ 0.325 (0.217 at 0) | Bybit leads 50 ms, ρ 0.337 | 0 ms, ρ 0.226 |
| Final 300 s | 50 ms | 0 ms, ρ 0.387 | 0 ms, ρ 0.251 | 0 ms, ρ 0.358 |
| Final 300 s | 100 ms | 0 ms, ρ 0.563 | 0 ms, ρ 0.382 | 0 ms, ρ 0.483 |

The apparent Bybit lead **did not persist** across sessions. It is also confounded by push cadence (Bybit's book updates every 20 ms, Binance's and OKX's every 100 ms) and by each venue's network path to this machine.

Signed aggressive volume shows no consistent lead either. One session paired Binance and OKX at 500 ms (ρ 0.467), and the final session did not reproduce it. Establishing any lead/lag needs many sessions and significance testing, which is not implemented.

## Latency (final session, nanoseconds)

| Stage, per frame | p50 | p90 | p99 | p99.9 | max |
|---|---:|---:|---:|---:|---:|
| Decode (socket read to canonical frame) | 2,459 | 22,042 | 101,667 | 322,375 | 672,792 |
| Queue (frame ready to sequencer dequeue) | 36,667 | 153,125 | 314,500 | 492,417 | 4,342,458 |
| Engine (sequence, apply, buffered record) | 5,167 | 54,875 | 163,292 | 253,041 | 2,910,583 |

How to read these:

- Every stage is measured on the session clock, on macOS, without pinning or real-time scheduling.
- **Queue** latency is dominated by thread wake-up on a blocking channel and by OS scheduling, not by backlog. The engine p50 is about 5 µs against frames arriving roughly every 6 ms.
- **Engine** frames include batches of up to 80 changes and buffered recording I/O, and the tails include file writes.
- Decode tails include socket-buffer bursts, where several messages are decoded back to back.

These are ingest-to-state latencies on public data, not order latencies; no gateway exists.

## Decode benchmarks

Criterion, release build, 30 samples. Each figure is one message: decode, validation, and canonical window diff or print chunking.

| Message | Central estimate | Sample median | 95% interval |
|---|---:|---:|---:|
| Binance complete top-20 snapshot (40 levels) | 4.020 µs | 4.041 µs | 3.9996–4.0452 µs |
| Bybit delta, 12 levels | 1.579 µs | 1.590 µs | 1.5717–1.5864 µs |
| OKX `books` update, 12 levels | 1.708 µs | 1.714 µs | 1.6992–1.7160 µs |
| Bybit trade message, 5 prints | 1.303 µs | 1.308 µs | 1.2984–1.3079 µs |

Cost is about 100 ns per native level, dominated by exact decimal-to-tick parsing. A specialised fixed-scale parser is the obvious optimisation. Messages are pre-built synthetic text in each venue's documented format; Bybit and OKX cycles re-send a snapshot once per 10,000 messages to keep continuity. [Raw output](phase8-criterion-output.txt) and [estimates](benchmarks/phase8/).

## Remaining scope

The [Phase 8 contracts](phase8.md) list every limit:

- top-20 windows only
- a fixed corridor per session
- recording I/O on the engine thread
- uncalibrated thresholds
- lead/lag that is observational, with no significance testing
- no fault injection of the network itself

Phase 9 replaces strict trade-through with a calibrated queue, size and fee simulator, using recorded live sessions as input. No order submission exists.
