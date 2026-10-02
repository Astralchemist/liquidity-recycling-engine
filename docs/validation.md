# Foundation validation — 2026-10-01

## Environment and reproducibility

Local macOS arm64 host, Rust `1.98.1 (48a229cea 2026-09-01)`, Cargo `1.98.1`, stable toolchain. Release profile uses thin LTO and overflow checks. No CPU affinity, real-time scheduling, network feed, or production load was used. Hardware model was not collected. Cargo.lock is included; builds/tests/benchmarks completed with cached dependencies in offline mode. Runtime dependencies are local workspace crates and std only.

## Correctness

`cargo test --workspace --locked --offline`: **21 tests passed**, zero failures.

- Exact decimal parsing, off-tick/precision rejection, integer limits, quantum divisibility and overflow.
- Exhaustive bounded long/short PnL conservation across 50,000 entry/exit/quantity combinations, signed quantum arithmetic, and 20,001 decimal round trips.
- Snapshot visibility and replacement, empty/one-sided queries, ordered ladders, exact midpoint/spread/depth.
- Duplicate/missing levels, crossed and locked prices, invalid quantities/prices, capacity rejection, wrong-market nonmutation, trade/depth separation, clock regression, sequence gaps/overflow and snapshot recovery.
- **200,000 generated book mutations** over 20 fixed seeds, compared after every event with independently maintained BTreeMaps and a second identical book.
- Binary event round trips over 9,999 generated events; standard CRC vector; single-byte corruption at every fixture byte; all partial-header/partial-record lengths; market identity; valid-CRC unknown tags/versions/metadata; injected partial-write failure and poisoned writer.
- Direct source versus two replays compared after every event; all four paced speeds preserve final state; gaps and clock failures invalidate the same book; unfinished snapshots and zero replay speed fail.

Property tests use explicit bounded exhaustive domains and deterministic generated traces without an extra property-testing dependency. They are reproducible, but do not provide automatic failure shrinking or proofs over all possible inputs. Boundary fixtures supplement the generated domains.

`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: passed.

`cargo fmt --all -- --check`: passed.

## Book benchmark

128 initialized levels per side, capacity 256; quantities and the selected bid level vary deterministically. Event preparation and validation are included. Snapshots are prepared outside the measured update loop. Full resulting book state is exposed to `black_box`, preventing state stores from being optimized away. Criterion: 30 samples, 1 second warmup, 2 second target measurement per case.

| Quantity updates | Median batch time | Criterion central estimate | Estimated throughput |
|---:|---:|---:|---:|
| 1 | 22.650 ns | 22.407 ns | 44.629 million/s |
| 10,000 | 209.622 µs | 209.718 µs | 47.683 million/s |
| 1,000,000 | 20.772 ms | 21.396 ms | 46.739 million/s |

Medians are Criterion sample medians, not individual-event latency quantiles. Central estimates use regression slope where available and mean otherwise. The million-update estimate's 95% interval is 21.054–21.833 ms. Full estimates are retained in [benchmarks](benchmarks/); [raw Criterion output](criterion-output.txt) preserves uncertainty/outliers. Historical comparison lines in raw output compare against an earlier measurement harness and are not an algorithm regression claim.

Additional Criterion estimates:

| Operation | Central estimate |
|---|---:|
| Insert then cancel at top of 128-level bid ladder (two events) | 135.80 ns |
| Checked integer PnL calculation | 1.234 ns |
| Encode 60-byte record with software CRC | 509.46 ns |
| Decode 60-byte record with software CRC | 509.51 ns |

CRC is deliberately simple and auditable; no hardware acceleration claim is made. These timings exclude network parsing, venue snapshot synchronization, strategy, risk, gateway and disk I/O. Book throughput is not full-engine throughput.

## Allocation and individual-call latency profile

A separate executable installs a counting System allocator and enables counting only after preparing measurement storage. It asserts **zero allocations** during:

- The measured 1, 10,000 and 1,000,000 Modify loops.
- 100,000 individually timed Modify calls.
- 100,000 Add/Cancel pairs (200,000 updates).
- Book construction and snapshot staging/commit.

Measured allocation count is 0 per operation for these paths, not a guarantee for future modules.

The separate bulk profile reported 86.82 ns/update over 10,000 events and 37.38 ns/update over 1,000,000 events; its single-update one-shot was 375 ns. This harness includes its own timer/allocator instrumentation and startup effects; Criterion's repeated samples are the throughput estimate above. Raw output is retained, including these differences.

| Individual-call percentile | Observed nanoseconds |
|---|---:|
| p50 | 42 |
| p90 | 42 |
| p95 | 42 |
| p99 | 42 |
| p99.9 | 42 |
| max | 125 |

This is a sample of 100,000 calls. Each sample includes `Instant` timer overhead and the measurement barrier. Quantization near 42 ns dominates the measurement; equal percentiles must not be interpreted as zero jitter or reliable sub-42-ns tail resolution. CPU/OS variation and finite sample size limit conclusions. Do not extrapolate these results to production network-to-ack latency. See [raw profile](profile-output.txt).

## Executable smoke test

The release CLI generated a binary fixture containing one snapshot and 10,000 quantity modifications (10,004 total events), then replayed it at maximum speed and 100x. Both runs ended with the same live book, sequence, BBO quantities, and doubled midpoint. The fixture is in `/private/tmp/lre-phase12-validation-20261001.lre`; [CLI output](cli-output.txt) records the runs. Test/replay calculations do not depend on the file path.

## Unimplemented validation gates

Live native sequence synchronization and atomic batches, cross-venue equivalence, transport overflow, risk limits/kill procedures, fees/rebates, inventory conservation, adverse markout, void revisit statistics, episode simulation, and full latency stages belong to later milestones. There are no profitability results or live execution claims in this delivery.
