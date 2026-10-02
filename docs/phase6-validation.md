# Phase 6 validation — 2026-10-02

## Correctness gates

**88 tests passed** with `cargo test --workspace --locked`. Formatting and `cargo clippy --workspace --all-targets --locked -- -D warnings` passed. The [test log](phase6-tests-output.txt) and [Clippy log](phase6-clippy-output.txt) are retained.

This milestone adds 19 tests.

**Recovery and forced exit.**
- Recovery fixture: a losing child is recycled by a profitable rebalance child. Exact realized PnL, rebates, cash, peak liability and the 466,666 ppm recovery yield are checked, along with first recovery and maker→maker counts.
- Forced-exit fixture: liability breach, then funding, then an emergency taker exit. Realized loss, fee, funding, the forced reason, the taker escape and the refusal of new opens after the halt are checked.

**Central rule (§20).**
- A balanced, underwater child is denied a normal close and can only leave by emergency after a halt.
- A loss on a confirmed fill is booked even when approval used a profitable expected price.
- Regression for the flaw described below: two pending losing closes cannot both claim the same rebalance.

**Capacity and reservations.**
- An in-flight open that fills after a halt is accounted for, and a cancelled reservation is released.
- Pending closes consume net budget and never release gross capacity. A cancelled close restores the child's previous role.
- All 24 fill orders of four reservations stay inside the net and gross limits.

**Targets, charges and halts.**
- A nonzero target with residual mark-to-market completes its episode explicitly.
- Funding credits and distinct rebate, fee and slippage charges conserve cash.
- Stale marks, position age, episode duration and drawdown each latch the correct halt.
- Arithmetic failure is atomic: cash, accounts, children and sequence are unchanged and `UnhandledState` latches. Sequence gaps and clock regression latch their reasons.

**History and configuration.**
- Bounded history evicts with exact counters, and three consecutive adverse episodes halt.
- Malformed configuration, unqualified evidence and duplicate fills are rejected without a state change.

**Generated commands against an independent model.** 20,000 generated commands run against a model that tracks each child's venue, side, entry and carry and the committed closes. After every command the test checks:
- cash, equity, realized PnL and liability
- net and gross exposure, and per-venue gross and pending
- that history rings plus evictions equal the cycle and episode counters
- that unassigned PnL is zero, so episode PnL partitions portfolio PnL
- that a flat book has no active episode
- that worst-case reserved net and gross stay within limits

Every accepted normal close must satisfy the reference statement of the central rule, and every `NoBenefit` denial must violate it. The test asserts that both losing rebalance approvals and `NoBenefit` denials actually occurred. It also requires that the episode ring evicted.

**Journal.**
- All ten command variants round-trip, and every single-bit corruption of a record is rejected.
- Nonzero reserved bytes are rejected even when the CRC is valid.
- Replay matches the directly applied ledger after every fixture command. A different configuration checksum and a truncated final record are both rejected.

**Supporting changes.**
- The CLI parses configuration strictly: inexact quanta, duplicate venues, decimal money limits and unknown fields are rejected.
- The table-driven CRC32 is compared against the bit-at-a-time definition for 301 prefix lengths and all 256 single bytes.

Generated tests use fixed seeds and high LCG bits. They are reproducible finite checks, not exhaustive proofs. The central-rule assertion was mutation-checked: reverting the fix makes the generated test fail at that assertion. Other assertions were not mutation-checked.

## Defects found and fixed during validation

- **Central rule ignored committed closes.** With net +1 and one losing long close pending, a second losing long close was approved as a "rebalance", although once both filled net would be −1. Approval now projects committed closes: `q = net + closing_buys - closing_sells`. Profitable closes are unaffected.
- **Episode order attribution.** Outstanding close orders (not only reservations) at episode start are now attributed to the episode. This changes results only when residual children exist between episodes.
- **Per-command cost.** Each command copied the whole ledger, including append-only history, out to scratch and back. History is now committed separately after success, and commands run in place against one rollback image.
- **Journal checksum cost.** CRC32 is now table-driven, and decode no longer recomputes the CRC for its canonical-form check. Checksum output and both binary formats are unchanged. No earlier benchmark covered CRC time, so earlier published numbers are unaffected.

Measured in this session on the same machine. The earlier raw output was not retained.

| Measurement | Before | After |
|---|---:|---:|
| Mixed command, 32 slots, 1M batch | 731.71 ns | 288.89 ns |
| Mixed p99.9, 32 slots | 917 ns | 417 ns |
| Mixed p50, 256 slots | 9,458 ns | 1,375 ns |
| Journal encode + decode | 4.754 µs | 854.5 ns |

## Release demo and replay

```sh
cargo run -p cli --release --locked -- inventory-demo config/phase6-inventory.toml <fresh>/lre-phase6-recovery recovery
cargo run -p cli --release --locked -- inventory-replay <fresh>/lre-phase6-recovery
cargo run -p cli --release --locked -- inventory-demo config/phase6-inventory.toml <fresh>/lre-phase6-forced forced
cargo run -p cli --release --locked -- inventory-replay <fresh>/lre-phase6-forced
```

In both scenarios, the complete ledger state from direct input matched the binary replay. A separate replay invocation reproduced every summary line, and re-running the demo into an existing directory failed with `File exists`. The [raw output](phase6-cli-output.txt) includes the per-command Recovery trace.

| Scenario | Commands | Realized | Rebates | Fees | Funding | Net PnL | Peak liability | Recovery yield | Halt |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| `recovery` | 11 | +10 | 4 | 0 | 0 | **+14** | 30 | 466,666 ppm | none |
| `forced` | 8 | −110 | 1 | 2 | 1 | **−112** | 100 | −1,120,000 ppm | `PnlBreach` |

Units are price-tick × quantity-unit atoms; a quantum is 10 quantity units. In `recovery`, Recovery fell to −29 after the adverse mark and returned to 0 or above 6 ms after the episode began. The losing first child (−10) was closed as a rebalance once the profitable second child (+20) had been harvested. Both cycles were maker→maker.

In `forced`, the mark at 90 pushed liability to 100, which is at or above the 50 limit, so `PnlBreach` latched before the caller's explicit `StructureInvalidated`; the first reason is kept. Only an emergency close was then permitted. It filled as a taker escape at 89.

These are stipulated accounting fixtures. They demonstrate exact bookkeeping and rule enforcement, not trading edge.

## Criterion benchmarks

Machine and build: Apple M5 (base, 16 GB), macOS, Rust stable 1.98.1, release thin LTO with integer overflow checks, no pinning or real-time scheduling. Criterion settings: 30 samples, 1-second warmup, 2-second target measurement.

The workload holds six resting children (one long and one short per venue; gross 6, net 0) with one active episode. Age, staleness, episode and loss thresholds are disabled by maximum values so measurement never halts.

| Operation | Central estimate | Sample median | 95% interval |
|---|---:|---:|---:|
| Mark one venue (2 of 6 children), 32 slots | 259.67 ns | 259.21 ns | 258.71–261.07 ns |
| Tick (transaction + two assessments + episode update), 32 slots | 213.28 ns | 212.88 ns | 212.74–213.91 ns |
| Recycle cycle, 4 commands (reserve, fill, close reservation, close fill), 32 slots | 1.0258 µs | 1.0227 µs | 1.0230–1.0287 µs |
| Mark one venue, 256 slots / 128 episodes | 1.3902 µs | 1.3960 µs | 1.3819–1.3982 µs |
| Tick, 256 slots / 128 episodes | 1.1615 µs | 1.1581 µs | 1.1335–1.1975 µs |
| Journal encode + decode, one 160-byte record | 854.54 ns | 855.58 ns | 852.72–856.28 ns |

The recycle cycle runs at **3.90 million commands per second** (256 ns per command). Tick is the fixed per-command floor: the rollback copy, two risk assessments and the episode update. At 32 slots it is most of the cost of every command.

Going from 32 to 256 slots multiplies the cost by about 5.4×. That reflects the 6.4× larger rollback image (48,880 vs 7,664 bytes) and the O(L) scans. History rings are excluded from the copy.

[Raw Criterion output](phase6-criterion-output.txt) and [machine-readable estimates](benchmarks/phase6/) are retained.

## Allocation and tail profile

The System allocator guard measured **zero allocations** in each of these:

- ledger construction and setup
- 1, 10,000 and 1,000,000 command batches
- every individually timed sample run

The ledger uses fixed inline arrays only. Sample storage and printing are outside the guard. The only unsafe code is the allocator forwarding in the benchmark executable; the inventory library forbids unsafe code.

The mixed workload is four round-robin venue marks per recycle-cycle command, with 1 ns timestamp increments, at 32 slots and 8 episodes.

| Commands | Elapsed | ns/command | Throughput |
|---:|---:|---:|---:|
| 1 | 666 ns | 666.00 | 1.502 million/s |
| 10,000 | 2.903 ms | 290.29 | 3.445 million/s |
| 1,000,000 | 288.889 ms | 288.89 | 3.462 million/s |

Each row below is 100,000 individually timed commands, in nanoseconds:

| Workload | p50 | p90 | p95 | p99 | p99.9 | max |
|---|---:|---:|---:|---:|---:|---:|
| Mixed, 32 slots | 292 | 334 | 334 | 375 | 417 | 38,959 |
| Mark only, 32 slots | 292 | 292 | 333 | 334 | 417 | 11,375 |
| Recycle-cycle commands, 32 slots | 292 | 333 | 333 | 334 | 375 | 11,208 |
| Mixed, 256 slots / 128 episodes | 1,375 | 1,459 | 1,500 | 1,750 | 5,708 | 61,167 |

The [raw profile](phase6-profile-output.txt) is retained.

How to read these numbers:
- Timer and barrier overhead are included, and the macOS monotonic clock quantizes at about 42 ns. Maxima include OS interruptions.
- No halt and no episode completion occurred during measurement, so the episode-commit path appears only in tests.
- These are ledger-command costs, not network-to-order latency or a guaranteed deadline.

A mark costs roughly half of the Phase 5 complete research update (467 ns). That matters if every BBO change is forwarded as a `Mark` with a large L; the undo-log path in [Phase 6 contracts](phase6.md) addresses it.

## Remaining scope

The [Phase 6 contracts](phase6.md) specify:

- units and command semantics
- transaction ordering and hard limits
- the central rule and account identities
- episode partitioning
- the journal layout

Phase 7 connects revisit evidence and environment qualification to the ledger. It also runs synthetic scenarios A–F through the shared engine and executes the kill procedure. Simulated fills, queues, markouts and net maker yield remain Phase 9. No order submission is enabled.
