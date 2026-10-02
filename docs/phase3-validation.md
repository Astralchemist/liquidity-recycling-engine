# Phase 3 validation — 2026-10-02

## Correctness

`cargo test --workspace --locked --offline`: **35 tests passed**. `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` and `cargo fmt --all -- --check` passed.

New coverage includes:

- Exact native/common-grid price, quantity and notional conversion; settlement conversion; incompatibility, off-grid and overflow rejection.
- Unequal venue weights, shared price aggregation, zero-weight behavior, cached depth totals, local/global midpoint divergence and cross-venue crossed quotes.
- 40,000 generated three-venue mutations across eight seeds. Independent BTreeMaps recompute the expected weighted ladders and totals after every event; a second engine must have identical full state.
- Staleness threshold boundary, trade non-rejuvenation, disconnects, sequence gaps, withdrawal of a single contribution and snapshot-only recovery.
- Snapshot replacement visibility and wrong-market nonmutation.
- Global clock/normalization fault latching and blocked aggregate queries.
- Exact unweighted per-price population variance and integer square-root result.
- Atomic batches with intermediate crossings, final validation, preserved committed state on failure, and aggregate withdrawal after a bad batch.
- Native snapshot seeding, overlapping/contiguous ranges, gaps, duplicates, previous-range mismatch, invalid ranges and sequence exhaustion.
- Three input-file permutations produce the same merged event order and state; exchange timestamps deliberately run backward while local clocks remain monotonic.
- Mandatory shared-clock assertion, source identity/scale checks, duplicate input rejection and unfinished snapshot rejection.
- TOML rejects unsupported contracts, excessive weights, unknown fields and absent clock-domain agreement before file creation.

The earlier foundation tests remain in the workspace. Generated properties use fixed seeds and explicit reference models; they are not exhaustive proofs or automatic shrinking tests.

## Executable verification

The release demo saved three synthetic streams and configuration to `/private/tmp/lre-phase3-20261002`, then asserted exact equality between direct and recorded/replayed full engine state. A separate `multi-replay` invocation produced the same summary:

```text
events=3012
state=Open
midpoint_x2=20000
spread_ticks=200
weighted_bid_depth_microunits=6025500000
weighted_ask_depth_microunits=5850000000
```

All three venues were included with weights 1,000,000 / 750,000 / 500,000. [Raw CLI output](phase3-cli-output.txt) is retained. Scales, prices and weights are synthetic test data.

## Release benchmark

Local macOS arm64, Rust stable 1.98.1, release thin LTO with overflow checks; no pinning or real-time scheduling. Criterion uses 30 samples, a 1-second warmup and a 2-second target measurement. There are 128 levels per side per venue, three venue books and a 384-level aggregate capacity per side.

| Component | Central estimate | 95% interval |
|---|---:|---:|
| Venue update + normalization + freshness scan + incremental consolidation | 77.615 ns | 75.338–79.827 ns |
| Exact price normalization | 7.320 ns | 7.271–7.379 ns |
| Atomic two-level book update, 128 initialized levels/side, capacity 256 | 442.30 ns per batch | 440.72–445.91 ns |
| Generic native sequence-range validation | 1.550 ns | 1.531–1.584 ns |

The consolidation estimate is **12.884 million updates/second**; its Criterion sample median is **76.167 ns**. The benchmark varies the venue, selected level and quantity, and exposes complete resulting state to a compiler barrier. All benchmark source is retained.

[Raw consolidation/normalization results](phase3-criterion-output.txt), [atomic batch/native sequence results](phase3-batch-output.txt), and [machine-readable estimates](benchmarks/phase3/) record details. The atomic path's bounded scratch copy is visibly more expensive than a single level update. No optimization claim is based on comparing unrelated paths.

These are in-memory synthetic results. They exclude sockets, native JSON parsing, disk I/O, strategy, risk, gateway and acknowledgment latency. Network-to-order performance and profitability remain unmeasured.

## Allocation and tail profile

A separate executable guards System allocator calls. It measured **zero allocations** during initialization and snapshots, the measured update loops, individual-call timing, delete/reinsert operations, a disagreement query and venue withdrawal. Input state and sample buffers are prepared outside timed update loops.

| Update count | Elapsed | ns/update | Throughput |
|---:|---:|---:|---:|
| 1 | 166 ns | 166.00 | 6.024 million/s |
| 10,000 | 1.491 ms | 149.05 | 6.709 million/s |
| 1,000,000 | 83.821 ms | 83.82 | 11.930 million/s |

The allocation/profile executable has a different harness from Criterion. One-shot measurements and warmup effects should not replace Criterion's repeated-sample throughput estimate.

For 100,000 individually timed core calls:

| Quantile | Nanoseconds |
|---|---:|
| p50 | 83 |
| p90 | 84 |
| p95 | 84 |
| p99 | 84 |
| p99.9 | 125 |
| max | 12,250 |

Timer and compiler-barrier overhead are included. Clock quantization, OS interruptions and sample size limit tail conclusions. The observed maximum is retained rather than removed as an outlier. No latency deadline is guaranteed. See [raw profile](phase3-profile-output.txt).

## Remaining gates

Native adapters and real feeds are unimplemented. v1 recordings do not preserve atomic batch boundaries; native batch feeds need a versioned envelope and round-trip tests first. Thread transport, overflow policy, clock-origin metadata, full latency stages, strategy, risk/kill procedures and inventory simulation remain later work. The [Phase 3 contracts](phase3.md) document these boundaries.
