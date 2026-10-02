# Phase 5 validation — 2026-10-02

## Correctness gates

**69 tests passed** with `cargo test --workspace --locked --offline`. Formatting and `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` passed. [Test log](phase5-tests-output.txt) and [Clippy log](phase5-clippy-output.txt) are retained.

This milestone adds 15 tests:

- All nine bid/ask price-relation combinations for best-quote OFI, extreme sizes, and invalid quote rejection.
- 10,000 generated QI inputs checking bounds and sign symmetry, plus empty/negative/overflow cases.
- 10,000 generated flow events checked against independent brute-force count/time-window and bucket totals for every metric.
- Exact time-boundary expiry, equal-timestamp bursts, zero exposure, capacity failure and latched clock errors.
- Explicit cancellation assumptions, mismatched attribution rejection, and unknown-versus-zero rate handling.
- 20,001 Poisson exponent comparisons against `exp`, monotonicity, <=2 ppm error on that grid, zero/maximal inputs.
- Half-open bucket edges, trade-side attribution and malformed configuration/contribution rejection.
- Complete direct/recorded replay equality after every event; independent Phase 4 equality verifies that flow observation preserves pool/void event ordering.
- Non-best depth changes versus best-quote OFI; trade aggression without duplicate OFI; top-N QI and bucket trade-volume deltas.
- Idle expiry, stale source removal, sequence failure and snapshot epoch replacement without counting snapshot quantities as arrivals.
- Exact quantity-scale conversion, weighted OFI, and whole-engine failure on a full time buffer.
- Strict CLI parsing of names, capacities and unknown fields.

Generated tests use fixed seeds and independent reference calculations. They are reproducible finite property checks, not exhaustive proofs or automatically shrinking tests. Float arithmetic is used only as a numerical test reference, outside the runtime libraries.

The initial integration test exposed debug-thread stack exhaustion from inline rolling histories. The final implementation allocates fixed history buffers at startup and reuses them on reset. Tests pass with the standard test-thread stack; no stack-size environment override was used.

## Release demo and replay

```sh
cargo run -p cli --release --locked --offline -- flow-demo config/phase4-market.toml config/phase5-structures.toml config/phase5-flow.toml /private/tmp/lre-phase5-final-20261002
cargo run -p cli --release --locked --offline -- flow-replay /private/tmp/lre-phase5-final-20261002
```

Both complete Phase 4 state and complete flow/research state matched direct input versus replay. A separate replay invocation reproduced the summary. [Raw output](phase5-cli-output.txt) includes all three source exposures and bucket rates.

| Source | Time-window events | Exposure | Time OFI | Signed depth change |
|---|---:|---:|---:|---:|
| Venue 1 | 22 | 48 ms | 0 | +400 |
| Venue 2 | 3 | 38 ms | 0 | +400 |
| Venue 3 | 3 | 28 ms | 0 | +400 |

Each source added 500 and removed 100 normalized units. The +400 change came from a non-best pool level; removing and restoring the best bid netted to zero best-quote OFI. Venue 1 had 17 aggressive-buy prints and 2 aggressive-sell prints. Cancellation rates remained `None` under the default unknown-attribution policy. Best/top-two QI was zero at the final state; one broader current-depth bucket had QI +500,000 ppm.

These are deterministic semantic fixtures with explicit prints and no simulated depth consumption or queue fills. The event probability reported by the CLI is not evidence of execution probability or profitability.

## Criterion benchmarks

Local macOS arm64, Rust stable 1.98.1; release thin LTO with integer overflow checks. No pinning or real-time scheduling. Criterion: 30 samples, 1-second warmup, 2-second target measurement.

| Operation | Central estimate | Sample median | 95% interval |
|---|---:|---:|---:|
| Best-quote OFI | 3.084 ns | 3.082 ns | 3.078–3.091 ns |
| Integer QI | 4.951 ns | 4.936 ns | 4.930–4.989 ns |
| Integer Poisson event probability | 162.086 ns | 162.655 ns | 161.762–162.436 ns |
| Count + time windows with bucket attribution | 132.086 ns | 130.784 ns | 131.604–132.753 ns |
| Complete research update including flow | 467.557 ns | 469.689 ns | 466.641–468.862 ns |

The complete path achieved **2.139 million events/second**. It includes book maintenance, exact normalization, consolidation, liquidity grid/active void observation, pre-event fresh-midpoint calculation, per-source window expiry and the affected source's flow update. It does not automatically compute report-only top-N QI or Poisson probability on each event.

The full fixture has three books with capacity 128 levels per side, 3–4 occupied levels per side, consolidated capacity 384, 64 corridor ticks, 16 cells, 16 zone slots, and one active zone. It varies the venue and quantity of an existing bid. Flow windows retain 16 events and 32 ns, with 128 allocated slots each. The 32 ns horizon is deliberately synthetic, forcing steady expiry during 1 ns timestamp increments. Liquidity formation sampling remains at 1 ms; most events do not sample. Staleness and void expiry are disabled via maximum thresholds for measurement.

The standalone window benchmark assigns every event to one of three buckets and exercises both eviction and expiry. The complete benchmark uses realistic side/range checks for its fixture and may leave events unbucketed. These are distinct workloads. Full-capacity expiry bursts, dense ladders, many active voids, and snapshot rebuilds are not represented by the central estimates. See [Phase 4 validation](phase4-validation.md) for a separately measured formation-sampling path.

[Raw Criterion output](phase5-criterion-output.txt) and [machine-readable estimates](benchmarks/phase5/) are retained.

## Allocation and tail profile

The System allocator guard measured exactly **six startup allocations**, two fixed flow buffers per venue. It measured **zero allocations** during 1/10,000/1,000,000 event batches, individual-call timing, idle expiry, and snapshot-triggered flow reset. No buffer resizing occurs. Sample storage and printing are outside the measured guard. The only unsafe code is System allocator forwarding in the benchmark executable; the runtime orderflow library forbids unsafe code.

| Events | Elapsed | ns/event | Throughput |
|---:|---:|---:|---:|
| 1 | 666 ns | 666.00 | 1.502 million/s |
| 10,000 | 4.492 ms | 449.18 | 2.226 million/s |
| 1,000,000 | 449.494 ms | 449.49 | 2.225 million/s |

For 100,000 individually timed steady calls:

| Quantile | Nanoseconds |
|---|---:|
| p50 | 458 |
| p90 | 459 |
| p95 | 459 |
| p99 | 500 |
| p99.9 | 584 |
| max | 15,125 |

[Raw profile](phase5-profile-output.txt). Timer/barrier overhead and the maximum observed interruption remain included. Clock quantization, OS interruptions, sample size and workload constrain interpretation. This individually timed window does not cross a formation-sample boundary, though the million-event batch does. These results are not network-to-order latency or a guaranteed deadline; sockets, native parsing, disk writes, inventory, risk, gateways and acknowledgments are absent.

## Remaining scope

The [Phase 5 contracts](phase5.md) specify epoch resets, cancellation ambiguity, bounded-buffer failure, rate exposure, event-time buckets and probability limitations. Exact native atomic-batch recording, live feeds and full stage-by-stage latency remain future work. Phase 6 adds the exact inventory child ledger, liability/harvesting accounts, episodes and hard bounds. No order submission is enabled.
