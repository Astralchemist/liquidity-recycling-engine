# Phase 7 validation — 2026-10-02

## Correctness gates

**105 tests passed** with `cargo test --workspace --locked`. Formatting and `cargo clippy --workspace --all-targets --locked -- -D warnings` passed. The [test log](phase7-tests-output.txt) and [Clippy log](phase7-clippy-output.txt) are retained.

This milestone adds 17 tests:

**Environment (3).** Classification precedence and definitions are checked across all five states: oscillation versus a run diluted by the window, the window rolling off, wide spread, closed market, spread flapping, and one-sided aggression. Toxicity components are checked with unknown cancellation. A capacity failure must leave state unchanged; clock regression and time-in-state accounting are also covered.

**Generator (1).** Every scenario stream is accepted by a real `VenueBook`. Local time strictly increases, so direct order and merged-replay order are identical. Per-venue sequences are contiguous and books are never crossed.

**Scenarios A–F (7, plus the E variant).** The per-scenario assertions are listed in the table below. After **every** market event of every run, the engine-wide invariants are checked:

- net, gross, venue and open-order limits
- each simulated resting order corresponds to exactly one ledger reservation, or to one pending normal close on an opposite-side child, and the counts match
- unassigned PnL is zero, so episode PnL partitions portfolio PnL
- a priced halt is flat with no orders by the end of the same event

**Determinism (1).** Every scenario run twice produces identical engine state and command journal.

**Fault (1).** Dropping one venue-1 event while inventory is held faults the research path and halts with `SequenceGap`. The engine flattens using the ledger's last marks.

**Configuration (1).** Six invalid policy or fee configurations, and a ledger venue order different from the market order, are rejected.

**Replay (1).** All six scenarios are recorded to binary market format, merged and replayed through a fresh engine: the complete engine state and the inventory command stream are identical. The journal alone then rebuilds an identical ledger.

**Sweep (1).** Twelve seeded policy configurations are run on all six scenarios. They vary entry levels, entry net, harvest distance, threshold, price improvement, reprice ticks, rebalance age, and shock exit. All per-event invariants hold. Coverage: 16 forced exits and 390 completed cycles.

**CLI (1).** The TOML files equal `engine::fixtures` exactly. Unknown rule, environment and scope names and unknown fields are rejected, and so are scenario letters other than a–f.

Generated scripts and the sweep use fixed seeds. They are reproducible finite checks, not proofs.

## Scenario outcomes

Run with `scenario-suite`, release build, default configuration. Money is in tick × quantity-unit atoms; one child is 10 units.

| | Halt | Flat | Cycles | Maker / taker fills | Peak liability | Net PnL | Recovery yield |
|---|---|---|---:|---:|---:|---:|---:|
| A oscillation | none | yes | 26 | 52 / 0 | 150 | **+182** | 1,213,333 ppm |
| B continuation | `PnlBreach` | yes | 10 | 18 / 2 | 310 | **−238** | −767,741 ppm |
| C local void | none | yes | 0 | 0 / 0 | 0 | 0 | — |
| D lead/lag | none | no, residual | 8 | 18 / 0 | 160 | **−32** (incl. MTM) | 550,000 ppm |
| E toxic recovery | none | yes | 24 | 48 / 0 | 150 | **+48** | 320,000 ppm |
| F no replenishment | `PnlBreach` | yes | 7 | 12 / 2 | 310 | **−274** | −883,870 ppm |

What each scenario test asserts:

- **A:** zone 1 is [100,103], market-wide, registered and revisited. Every entry happened during an allowed revisit while balanced-active. No halt, flat at the end, at least 10 cycles, and every cycle maker→maker.
- **B:** `PnlBreach`, then flat. No reservation and no normal close after the halt. Taker escapes recorded. Loss below the liability limit plus 200, with time spent trending.
- **C:** a venue-local zone on venue 1 was revisited and never traded. Its disallowed-scope counter is positive, and the journal contains only marks and ticks.
- **D:** the market-wide zone was never invalidated (zero invalidations), half-tick divergence was observed, and trading happened. The residual is reported through episode mark-to-market.
- **E:** inventory was held through a liquidity shock after the sweep. Liability then peaks with Recovery negative, and Recovery returns to zero or above. No forced exit, flat at the end.
  - The **E variant** with `exit_on_environment ∋ liquidity_shock` halts with `RegimeChange` and exits as taker.
- **F:** `PnlBreach`, flat, taker escapes. Further voids formed below the price but were never traded after the halt.

### Sensitivity

The two rebalance knobs were isolated on scenario A, with every other setting at its default:

| `rebalance_improve_ticks` | Rebalance age trigger | Cycles | Final equity | Flat |
|---:|---|---:|---:|---|
| 0 | off | 24 | +180 | no (residual short) |
| 1 | off | 7 | −55 | no |
| 0 | 600 ms | 26 | **+182** | yes |
| 1 | 600 ms | 8 | −54 | yes |

An earlier setting with threshold 0 and no improvement ended A flat at −336.

Under strict trade-through, every entry fill is adversely selected by at least one tick. A prompt rebalance, or one improved inside the spread, locks that tick in. Harvest-first waits for two-way movement. The defaults join the touch with a 600 ms age trigger, and both knobs are retained.

These are properties of the synthetic paths and of the fill rule. They are not calibrated evidence about live markets, and they show why the Phase 9 fill model matters.

## Release demo and replay

```sh
cargo run -p cli --release --locked -- scenario-suite config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml
cargo run -p cli --release --locked -- scenario-demo config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml <fresh>/lre-phase7-a a
cargo run -p cli --release --locked -- scenario-replay <fresh>/lre-phase7-a
```

For A, E and F, recorded-market replay matched direct input: complete engine state and 1,678, 1,821 and 524 inventory commands respectively. The journal file rebuilt an identical ledger, and a separate `scenario-replay` invocation reproduced every summary line. The [raw output](phase7-cli-output.txt) includes zones, environment time-in-state, policy counters, toxicity components and per-episode metrics.

## Criterion benchmarks

Machine and build: Apple M5 (base, 16 GB), macOS, Rust 1.98.1, release thin LTO with overflow checks, no pinning. Criterion settings: 20 samples, 1 s warmup, 3 s measurement. Each iteration starts from a freshly built engine, built outside the timed region, and processes the whole scenario, including snapshots.

| Scenario | Path | Central estimate | Sample median | 95% interval | Per event |
|---|---|---:|---:|---:|---:|
| A (2,712 events) | Phase 5 research only | 4.1803 ms | 4.1599 ms | 4.1619–4.2028 ms | 1,541 ns |
| A | Full Phase 7 engine | 5.2862 ms | 5.2561 ms | 5.2587–5.3190 ms | 1,949 ns |
| E (2,916 events) | Phase 5 research only | 4.4981 ms | 4.4782 ms | 4.4784–4.5234 ms | 1,543 ns |
| E | Full Phase 7 engine | 5.7432 ms | 5.6952 ms | 5.6961–5.7866 ms | 1,970 ns |

**Phase 7 adds about 410–430 ns per event (27%)** on top of the shared research path. That covers the environment, three divergence queries, marks, the fill scan, policy and about 0.6 ledger commands per event.

The research path itself costs about 1.54 µs per event here, against 467 ns in Phase 5's benchmark. These books are full contiguous ladders of up to 63 levels per side on three venues, where Phase 5 used 3–4 levels, and these timings include snapshot builds. The figures are not comparable workloads.

[Raw Criterion output](phase7-criterion-output.txt) and [estimates](benchmarks/phase7/) are retained.

## Allocation and tail profile

The engine box plus six fixed flow buffers are **7 startup allocations**. The allocator guard measured **zero allocations** across all 13,888 events of the six scenarios. The command sink only counts. See the [raw profile](phase7-profile-output.txt).

| Events (all six scenarios) | p50 | p90 | p99 | p99.9 | max |
|---|---:|---:|---:|---:|---:|
| All, n = 13,888 | 2,000 | 2,250 | 2,875 | 10,417 | 14,584 |
| No ledger command, n = 6,376 | 1,709 | 1,833 | 1,958 | 2,167 | 11,625 |
| With commands, n = 7,512 | 2,125 | 2,291 | 3,167 | 12,125 | 14,584 |

Values are in nanoseconds and include timer overhead (about 42 ns quantization).

**The tail is snapshot rebuilds.** The slowest 0.1% (14 events) were almost entirely `SnapshotEnd`. That event triggers the Phase 4 structural rebuild of the liquidity grid from every committed level. Steady-state events without commands have a p99.9 of 2.2 µs.

These are engine costs on synthetic input. They exclude:

- sockets and native parsing
- recording I/O
- gateways and acknowledgements

## Remaining scope

[Phase 7 contracts](phase7.md) document the environment definitions, policy rules, fill rule, kill procedure, scenario scripts and limits. Next:

- **Phase 8:** native public feeds for Binance, Bybit and OKX, using the sync rules in [providers](providers.md). Batch-aware recording, lead/lag tooling and stage latencies.
- **Phase 9:** queue, size and fee simulation with markouts.

No order submission is enabled.
