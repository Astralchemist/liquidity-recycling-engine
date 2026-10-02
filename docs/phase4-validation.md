# Phase 4 validation — 2026-10-02

## Correctness gates

**54 tests passed** with `cargo test --workspace --locked --offline`. `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` and `cargo fmt --all -- --check` passed. [Test log](phase4-tests-output.txt) and [Clippy log](phase4-clippy-output.txt) are retained.

Phase 4 adds 19 tests, covering:

- 20,000 deterministic generated grid changes versus independent brute-force weighted range sums, including extreme range bounds.
- Bucket half-open inequalities at 125 integer/half-tick midpoints, both sides; exact age aggregation and the largest positive tick corridor.
- Explicit weighted score components, rejection of enabled-but-unavailable inputs, baseline warmup/freezing and coverage loss.
- Pool persistence reset when the covered source cohort changes.
- Boundary-touch activation, mirrored penetration paths, partial penetration, histogram conservation, early exits, repeated visits, and skipped outside-to-outside crossings.
- Refill/expiry/coverage precedence, bounded retention, terminal eviction, overlap rejection, mask changes and invalid observations.
- Market-wide versus local voids, refill-before-revisit, and one-way continuation fixtures.
- Disabled coverage assumptions, stale-source invalidation, fault latching, and strict configuration parsing.
- Complete research-state equality after every direct/replayed event, including permuted recording inputs.

Generated properties use fixed deterministic input generation and independent reference calculations; they are not exhaustive proofs or automatically shrinking property tests. Earlier arithmetic, book, consolidation and binary integrity tests remain in the workspace.

## Release execution

The `revisit` demo wrote both configurations and three v1 recordings into `/private/tmp/lre-phase4-final-20261002`, then asserted full-state equality with merged replay. A separate `structure-replay` command reproduced the same summary. [Raw CLI output](phase4-cli-output.txt):

```text
events=58
registered_voids=1
revisited_zones=1
revisit_events=1
pool_activations=1
zone=[100,103], scope=MarketWide, mask=7, state=Refilled
first_revisit_delay=1,000,000 ns
maximum_penetration_ppm=333333
penetration_distribution=[0,0,1,0,0,0]
```

The test asserts revisit activation on the exact upper-bound touch before the later one-third penetration. No full traversal is required. These semantic fixtures use explicit prints without simulated depth consumption; the one observed revisit supplies no estimate of real-market revisit probability or profitability.

## Criterion results

Local macOS arm64, Rust stable 1.98.1, release thin LTO with integer overflow checks. No CPU pinning or real-time scheduler. Criterion: 30 samples, 1-second warmup, 2-second target measurement. [Raw results](phase4-criterion-output.txt) and [machine-readable estimates](benchmarks/phase4/) are retained.

| Operation | Central estimate | Sample median | 95% interval |
|---|---:|---:|---:|
| Weighted grid level update, 64 ticks | 13.176 ns | 13.114 ns | 13.128–13.226 ns |
| Formation sample, 16 cells × 3 sources | 650.02 ns | 649.261 ns | 648.81–652.13 ns |
| Void touch/exit state update | 15.050 ns | 15.054 ns | 15.009–15.114 ns |
| Full research event, rare formation sampling | 160.81 ns | 160.877 ns | 160.17–161.96 ns |
| Full research event, formation sample every event | 988.69 ns | 985.056 ns | 984.96–995.33 ns |

Full research-event throughput is **6.219 million/s** on the mostly nonsampling path and **1.011 million/s** when every event triggers formation sampling. Both include venue-book maintenance, normalization, consolidation, incremental grid update and active-zone observation. Benchmarks expose resulting state to compiler barriers.

The full-path fixture has three books with capacity 128 per side, only 3–4 occupied levels per side, a 384-level aggregate capacity, a 64-tick corridor, 16 cells, 16 zone slots and one active zone. It varies venue/quantity at an existing bid. The rare-sampling case advances recorded time by 1 ns per event with a 1 ms formation interval; the every-event case advances by 1 ms. Staleness/zone expiry are set to `u64::MAX` for measurement. These results do not represent dense 128-level books, all active zone slots, structural rebuilds, or real traffic mixes. The standalone sample uses covered cells with only one occupied cell per source.

## Allocations and latency profile

The separate System allocator guard observed **zero allocations** during engine initialization, synthetic snapshots/formation, 1/10,000/1,000,000 update batches, and individually timed steady events. The million-event batch includes a formation sample. Measurement sample buffers are allocated before the guard; printing is outside it. Unsafe allocator forwarding exists only in this measurement executable; the runtime liquidity library forbids unsafe code.

| Events | Elapsed | ns/event | Throughput |
|---:|---:|---:|---:|
| 1 | 333 ns | 333.00 | 3.003 million/s |
| 10,000 | 2.200 ms | 219.97 | 4.546 million/s |
| 1,000,000 | 162.220 ms | 162.22 | 6.164 million/s |

For 100,000 individually timed steady calls:

| Quantile | Nanoseconds |
|---|---:|
| p50 | 166 |
| p90 | 167 |
| p95 | 167 |
| p99 | 208 |
| p99.9 | 291 |
| max | 10,458 |

[Raw profile](phase4-profile-output.txt). These quantiles include timer/barrier overhead. This specific timing window does not cross a formation-sample boundary; use the separate every-event-sampled Criterion result to assess that work. Clock quantization, OS interruptions and sample count limit tail conclusions. The observed maximum remains included.

All measurements are in-memory synthetic work. Socket ingress, native message parsing, disk writes, inventory/strategy, risk, gateway and acknowledgment stages are excluded. There is no network-to-order latency guarantee or demonstrated trading edge.

## Remaining scope

Phase 5 adds explicit flow estimators and bucket rates. The [Phase 4 contracts](phase4.md) detail fixed corridor/cell granularity, sampled baselines, coverage assumptions, source censoring, bounded terminal retention, last-trade behavior and unavailable order counts. Native atomic batch recording and live feeds, complete latency stages, inventory episodes, queue/fill simulation and emergency execution remain later milestones.
