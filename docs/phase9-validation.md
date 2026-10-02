# Phase 9 validation — 2026-10-02

## Correctness gates

**152 tests passed** with `cargo test --workspace --locked`. Formatting and `cargo clippy --workspace --all-targets --locked -- -D warnings` passed. The [test log](phase9-tests-output.txt) and [Clippy log](phase9-clippy-output.txt) are retained.

This milestone adds 21 tests:

- **Execution primitives (8).**
  - Fees round up and rebates round down, with exact values at a live price; taker fills get no rebate; out-of-range schedules are refused.
  - Queue: prints consume the queue ahead before filling us, and the depth echo of prints is not a cancellation; a print through our price fills everything; the three cancel models bound the position (40 ahead of 80, 40 cancelled → 40, 20, 0); 2,000 generated sequences keep the models ordered and bounded.
  - Markouts are signed by side and split exactly into spread and drift; a cursor tracker equals the original full-scan algorithm on random fills, missing midpoints and ring overflow; overflow is counted.
- **Engine (11).**
  - Every simulated fill in all six scenarios, under all four models, is justified by its print. It must be a print on its venue from the opposite aggressor, at the order price. Strict fills need a trade-through. Queue fills need a print through, or one at the price that clears the queue ahead plus our size. No entry or rebalance is placed at a price already traded through.
  - Injected depth changes (others join behind, then part of the level is cancelled, with no print) move the queue exactly as each cancel model specifies.
  - Every fill's fee and rebate equal the ppm schedule plus fixed atoms. Each close fills with exactly the charges its reservation assumed. Role-split metrics reconcile with the ledger, and net maker yield equals its definition.
  - Markouts equal an independent oracle built from the per-event midpoint path, at all ten horizons.
  - Funding is checked against the journal replayed into a fresh ledger: the cost formula and rounding direction for longs and shorts, every held child charged at every boundary, and charges only at crossings.
  - J(a): an unreachable edge places nothing. Zero weights keep queue position instead of repricing. J recomputed from public state equals the engine's value at more than 1,000 steps, for new orders and for resting orders at their tracked position.
  - A print through the touch, injected before a step that would quote, defers the quote until the venue's next depth update.
  - The live regression: input arriving after a silent gap is refused by `accepts`. Without the check, the same input faults the engine with `SnapshotRequired`.
  - Invalid execution configurations are refused, including the queue model with a non-identity venue price scale.
  - Phase 9 runs are deterministic, and their journals rebuild the identical ledger.
- **CLI (2).** The fill study keeps every probe at the touch (or deliberately stale or in flight), never fills on depth, and bounds fills by placements, with latency producing in-flight fills and rejects. The Phase 9 live configuration builds the queue model with retail fees, while the Phase 8 file still parses to the strict rule.

The Phase 7 configuration test now also covers the new `[fills]` rule options and the optional sections. Invariant checks after every event now include queue state: present exactly under the queue model, never complete while resting, and never more ahead than displayed.

**Mutation checks.** Each of these was introduced deliberately and caught by at least one test:

- removing queue depth tracking
- truncating funding instead of rounding toward payment
- truncating fees
- valuing a resting order at the back of its level
- removing the stale-touch rule
- checking the current book state instead of `live_at`

One mutation first survived (resting orders valued at the back of the level); the `resting_utility` oracle was added to catch it.

## Defects found by live data

1. **Refilling at a stale touch.** The first fill study refilled a probe at the stale bid on every print of a sweep, about 2,100 fills per minute per venue with a median wait of 0 ms. Depth arrives every 20–100 ms while prints stream continuously. The stale-touch rule now applies to study probes and to engine entry and rebalance quotes.
2. **Staleness at the frame's own timestamp.** The 600 s session faulted the engine at 550.05 s, two seconds into a network stall that silenced all three venues. The first Bybit frame after a 2.04 s gap passed a check of the book's current state, then `apply` advanced the clock, withdrew that very book and failed with `SnapshotRequired`. `Engine::accepts` now evaluates staleness at the frame's receive time ([contracts](phase9.md#live-path-fix-staleness-at-the-frames-own-timestamp)).

## Live sessions

| Session | Length | Config | Notes |
|---|---:|---|---|
| A (Phase 8) | 58.6 s | Phase 8 engine | Rising market: +29 ticks mean control drift at 1 s. |
| B (Phase 8) | 294.6 s | Phase 8 engine | One 8 s OKX gap. |
| C (Phase 9) | 601.1 s | Phase 9 engine | Clean for 548 s, then a network-wide stall: 15–18 gaps of 1.5–11 s on every venue in the last 55 s. Engine fault at 550.05 s (defect 2). |
| D (Phase 9, fixed build) | 600.2 s | Phase 9 engine | One Bybit resync, no fault. Trendless: −0.4 ticks mean control drift at 1 s. |

The [session output](phase9-live-output.txt) for C and D is retained: source health, latency, engine summary and exact-replay confirmation. Session C processed 524,067 canonical events. Replay reproduced the engine state and all 8,339 inventory commands exactly, including the fault. The study's market view ends at the fault, since probes are withdrawn once research stops, so C contributes about 550 s.

Session D processed 409,485 events, and replay reproduced all 8,266 commands exactly. D had no network-wide stall, so the defect 2 fix is verified by its regression test, not yet by a live recurrence.

**Engine.** The live engine placed no entries in any session.

- Sessions A–C registered at most 6 voids and revisited none.
- Session D registered 88 voids and revisited 6 (8 revisits), but still placed no entry. On 58 steps the revisited zone was venue-local, a scope the configuration does not trade. On 62 steps an allowed zone was revisited while the environment was not balanced-active.
- J(a) was therefore never evaluated.

Thresholds remain uncalibrated. The fill study measures the execution models directly, independent of the policy.

## Fill study

Probes are one child (0.001 BTC) per side, venue and model. They re-quote at one tick behind the touch, and the maker fee is 200 ppm. [Raw output](phase9-study-output.txt) covers all four sessions at 0, 50 and 200 ms.

**Unconditional control** (reference midpoint sampled every 100 ms):

| Session | Mean drift, 1 s | σ, 1 ms | σ, 100 ms | σ, 1 s | σ, 5 s |
|---|---:|---:|---:|---:|---:|
| A | +29.0 | 7.7 | 19.5 | 77.0 | 198.3 |
| B | −7.0 | 6.2 | 24.8 | 92.8 | 229.6 |
| C | −5.7 | 6.4 | 23.7 | 98.4 | 256.4 |
| D | −0.4 | 4.6 | 16.5 | 68.9 | 175.7 |

All figures are in ticks of 0.1 USDT. The 6–8 tick σ at 1 ms is composite-reference noise: venues update it at different instants.

**Session C, 50 ms latency, proportional model:**

| Venue | Placed | Fills (at price) | Fills/min | Fill ratio | Rejects | Wait p50 | Ahead at entry |
|---|---:|---:|---:|---:|---:|---:|---:|
| Binance | 1,784 | 915 (628) | 91.8 | 0.513 | 590 | 304 ms | 5.20 BTC |
| Bybit | 2,517 | 998 (531) | 100.1 | 0.397 | 939 | 209 ms | 2.22 BTC |
| OKX | 1,537 | 746 (375) | 74.8 | 0.485 | 439 | 348 ms | 5.25 BTC |

| Venue | Markout 100 ms | Markout 1 s | Markout 5 s | Drift 1 s | Adverse share 1 s |
|---|---:|---:|---:|---:|---:|
| Binance | −36.1 | −68.0 | −75.3 | −55.3 | 0.69 |
| Bybit | −25.5 | −56.6 | −52.9 | −56.9 | 0.69 |
| OKX | −41.3 | −69.4 | −88.8 | −53.6 | 0.76 |

**Model bounds** (session C, 50 ms, Binance):

| Model | Fills | Fill ratio | Drift 1 s |
|---|---:|---:|---:|
| Strict | 687 | 0.442 | −68.7 |
| Pessimistic | 857 | 0.497 | −55.3 |
| Proportional | 915 | 0.513 | −55.3 |
| Optimistic | 1,063 | 0.551 | −46.8 |

The order pessimistic ≤ proportional ≤ optimistic holds for fills on every venue, session and latency. Strict fills least, because it fills only on sweeps, and usually shows the most adverse drift.

**Latency** (session C, proportional, Binance):

| Latency | Fills/min | Fill ratio | Rejects | Markout 1 s |
|---:|---:|---:|---:|---:|
| 0 ms | 176.3 | 0.664 | 0 | −70.8 |
| 50 ms | 91.8 | 0.513 | 590 | −68.0 |
| 200 ms | 55.0 | 0.431 | 560 | −64.0 |

**Robustness** (proportional, 50 ms): drift at 1 s and adverse share.

| Session | Binance | Bybit | OKX |
|---|---|---|---|
| A | −55.8 (0.84) | −59.0 (0.64) | −45.4 (0.78) |
| B | −62.5 (0.71) | −66.0 (0.63) | −50.1 (0.78) |
| C | −55.3 (0.69) | −56.9 (0.69) | −53.6 (0.76) |
| D | −48.0 (0.80) | −51.8 (0.79) | −41.3 (0.71) |

### What the study shows

- **Strong adverse selection at the touch.** A maker that always joins the touch is filled mostly when the price moves through it. The drift at 1 s is −41 to −66 ticks, 0.6–0.7 of the unconditional 1 s σ, against the probe on every venue and in every session. That includes session A's rising market, where sell fills dominated, and session D's trendless one.
- **Negative spread capture.** On Binance and OKX the fill was on average 7–31 ticks worse than the composite midpoint at the moment of the fill, in every session: the venue's touch lags the others. Bybit, which pushes depth every 20 ms, ranged from −1 to +24 ticks.
- **The fee is larger still.** At about 86,600 USDT the 200 ppm maker fee is 173 ticks per BTC. A fill's 1 s markout of about −68 ticks plus the fee is about −241 ticks, roughly **−280 ppm of notional per fill**.
- **No documented rebate closes the gap.** The best maker rebates documented in [providers](providers.md) are about 40–50 ppm (Bybit MM3, OKX VIP9). They do not cover the 1 s drift alone (about 64 ppm).
- **The cancel models matter less than latency.** Optimistic versus pessimistic changes fills by 16–49% and drift by up to 14 ticks. Going from 0 to 200 ms latency cuts fills by 46–82%, and post-only rejects become comparable in number to fills.

These results describe this naive probe: always at the touch, one child, re-quoting at one tick, on about 25 minutes of one asset. They do not describe the engine's revisit-gated policy, which never traded live. They do mean J(a) needs a measured adverse term; the live configuration uses a 69-tick prior from session B.

## Benchmarks

Criterion, release build, Apple M5 base, `sample_size` 20–30. [Raw output](phase9-criterion-output.txt) and [estimates](benchmarks/phase9/).

| Benchmark | Central estimate | Per element |
|---|---:|---:|
| Scenario A, research only | 2.201 ms | 812 ns/event |
| Scenario A, Phase 7 strict engine | 2.880 ms | 1,062 ns/event |
| Scenario A, Phase 9 engine (queue, ppm fees, funding, J(a)) | 2.945 ms | 1,086 ns/event |
| Scenario E, Phase 7 strict engine | 3.104 ms | 1,064 ns/event |
| Scenario E, Phase 9 engine | 3.152 ms | 1,081 ns/event |
| Queue, pessimistic, 1,024 operations | 838 ns | 0.8 ns/op |
| Queue, proportional, 1,024 operations | 2.008 µs | 2.0 ns/op (`i128` division) |
| Queue, optimistic, 1,024 operations | 880 ns | 0.9 ns/op |
| Markouts: 1,024 fills, 102,400 observations, 10 horizons | 820 µs | 8.0 ns/observation |
| Fee cost, one fill | 5.45 ns | |

The Phase 9 configuration adds about **2%** per event: queue tracking around depth changes, the J(a) evaluation and the funding check.

The first markout implementation scanned every pending fill on every observation and cost about 1.8 µs per observation. It was replaced by per-horizon cursors, with identical results by property test and engine oracles.

**Allocation and tail profile.** Both configurations make **10 startup allocations**:

- the engine and research boxes
- the markout ring
- six flow buffers
- one batch scratch

The allocator guard measured **zero allocations** across all 13,888 events of the six scenarios, under both configurations, after an unmeasured warm-up of both. [Raw profile](phase9-profile-output.txt).

| Configuration | p50 | p90 | p99 | p99.9 | Max |
|---|---:|---:|---:|---:|---:|
| Phase 7 strict | 1,208 ns | 1,416 ns | 1,792 ns | 7,458 ns | 41,583 ns |
| Phase 9 queue | 1,125 ns | 1,333 ns | 1,625 ns | 5,791 ns | 12,250 ns |

These latencies include the timer, at 1 ms event spacing.

## Remaining scope

The [Phase 9 contracts](phase9.md) list every limit:

- queues inferred from 20 public levels
- atomic children with discarded partial progress
- zero engine latency
- constant-rate funding
- partly uncalibrated J(a)
- minutes of one asset

Phase 10 adds demo-exchange order lifecycles. Account and jurisdiction prerequisites are in [providers](providers.md). No order is ever sent.
