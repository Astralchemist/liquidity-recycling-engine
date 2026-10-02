# Liquidity & Inventory Recycling Engine

Rust-only deterministic research engine. Completed: Phase 1–2 arithmetic and bounded L2 books; Phase 3 exact multi-venue normalization and weighted consolidation, with synthetic recording/replay; Phase 4 configurable liquidity pools, historical voids, touch/partial revisits and refill statistics; Phase 5 bounded OFI, queue imbalance and flow intensity; Phase 6 exact one-quantum inventory ledger, hard limits, recovery accounts and episodes; Phase 7 one engine with environment classification, a rule-based recycling policy, simulated strict trade-through fills, the kill procedure and specification scenarios A–F; Phase 8 live public feeds from Binance, Bybit and OKX with atomic batches, recording format v2 and exact live-versus-replay comparison; Phase 9 execution simulation with an L2 queue model under three cancellation assumptions, exact ppm fee schedules, funding, §18 markouts split into spread and drift, net maker yield, the objective J(a) as an entry gate, and a fill study on recorded live sessions. Order submission remains a later milestone; no order is ever sent.

## Run Phase 9 (execution simulation)

```sh
cargo run -p cli --release --locked -- live-record config/phase8-market.toml config/phase8-structures.toml config/phase8-flow.toml config/phase9-engine.toml /tmp/lre-live9 600
cargo run -p cli --release --locked -- fill-study /tmp/lre-live9 1 200 50
```

`config/phase9-engine.toml` runs the live engine with the proportional queue model, retail fees (maker 0.02%, taker 0.05%), funding, the §18 markout horizons and J(a).

`fill-study DIRECTORY REQUOTE_TICKS MAKER_FEE_PPM LATENCY_MS` replays any recorded session, including Phase 8 recordings. It places shadow maker probes at each venue's touch under every fill model and reports:

- fill rates, waits and queue position at entry
- post-only rejects under the given order latency
- markouts at every horizon, split into captured spread and adverse drift

Probes never reach the ledger. See [Phase 9 contracts](docs/phase9.md).

## Run Phase 8 (live public data)

```sh
cargo run -p cli --release --locked -- live-record config/phase8-market.toml config/phase8-structures.toml config/phase8-flow.toml config/phase8-engine.toml /tmp/lre-live 300
cargo run -p cli --release --locked -- live-replay /tmp/lre-live
cargo run -p cli --release --locked -- leadlag /tmp/lre-live 50 10
```

`live-record` connects to the public BTC/USDT perpetual feeds of Binance, Bybit and OKX. No account or key is needed, and **no order is ever sent**; fills are simulated.

It runs the full engine for the given seconds (5 to 3,600), records batch-preserving streams and the inventory journal, then requires replay to reproduce the live engine exactly. It prints source health, decode, queue and engine latency, and the engine summary.

Recordings contain exchange data: keep them out of the repository and do not redistribute them. Engine thresholds are uncalibrated. See [Phase 8 contracts](docs/phase8.md).

## Run Phase 7

```sh
cargo run -p cli --release --locked -- scenario-suite config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml
cargo run -p cli --release --locked -- scenario-demo config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml /tmp/lre-phase7-e e
cargo run -p cli --release --locked -- scenario-replay /tmp/lre-phase7-e
```

The suite runs scenarios A–F in memory and prints one row each: halt, flat, cycles, fills, peak liability, PnL, recovery yield and zone scope.

`scenario-demo` takes a letter `a`–`f` and a fresh directory. It records three venue streams and the inventory journal, then requires the replay to reproduce the complete engine state and the identical command stream.

Fills are simulated by one rule (strict trade-through), so PnL is not evidence of edge. See [Phase 7 contracts](docs/phase7.md) and the [provider documentation review](docs/providers.md).

## Run Phase 6

```sh
cargo run -p cli --release --locked -- inventory-demo config/phase6-inventory.toml /tmp/lre-phase6-demo recovery
cargo run -p cli --release --locked -- inventory-replay /tmp/lre-phase6-demo
```

Use a fresh directory. Scenarios: `recovery` (a losing child recycled through a profitable rebalance child) and `forced` (liability breach, then an emergency taker exit). The demo prints Recovery = harvest − liability after every command, saves the TOML and a checksum-protected command journal, and asserts complete direct-versus-replay ledger equality. Fills are stipulated accounting fixtures, not a market or queue simulation. See [Phase 6 contracts](docs/phase6.md).

## Run Phase 5

```sh
cargo run -p cli --release --locked -- flow-demo config/phase4-market.toml config/phase5-structures.toml config/phase5-flow.toml /tmp/lre-phase5-demo
cargo run -p cli --release --locked -- flow-replay /tmp/lre-phase5-demo
```

Use a fresh directory. Reports include per-venue best-quote OFI, signed depth changes, exact count/time windows, best/top-N and bucket QI, and arrival/aggression rates. Cancellation attribution defaults to unknown. Fixed flow buffers are allocated before processing and reused on resets. The Poisson output is event-arrival probability; queue fills remain a later phase. See [Phase 5 contracts](docs/phase5.md).

## Run Phase 4

```sh
cargo run -p cli --release --locked -- structure-demo config/phase4-market.toml config/phase4-structures.toml /tmp/lre-phase4-demo revisit
cargo run -p cli --release --locked -- structure-replay /tmp/lre-phase4-demo
```

Use a fresh directory. Scenarios: `revisit`, `local`, `refill`, `continuation`. The demo saves both configurations and three binary streams, then asserts full research-state equality with replay. Exact boundary touches activate revisit analysis. See [Phase 4 contracts](docs/phase4.md) for coverage, fixed-corridor, sampling and synthetic-print assumptions.

## Run Phase 3

From this directory, with Rust stable:

```sh
cargo run -p cli --release --locked -- multi-demo config/phase3.toml /tmp/lre-phase3-demo
cargo run -p cli --release --locked -- multi-replay /tmp/lre-phase3-demo
```

Use a fresh output directory. The demo records three synthetic venue streams with different tick/lot scales and weights, then verifies full-state equality between direct input and replay. It saves the TOML configuration with the recordings. The example is synthetic; it does not connect to exchanges.

## Validate and benchmark

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo bench -p feeds --bench decode --locked
cargo bench -p engine --bench engine --locked
cargo bench -p engine --bench profile --locked
cargo bench -p inventory --bench ledger --locked
cargo bench -p inventory --bench profile --locked
cargo bench -p orderflow --bench flow --locked
cargo bench -p orderflow --bench profile --locked
cargo bench -p liquidity --bench structures --locked
cargo bench -p liquidity --bench profile --locked
cargo bench -p consolidator --bench consolidation --locked
cargo bench -p consolidator --bench profile --locked
```

The profile executable measures allocations and latency percentiles. Book-only benchmarks remain available with `cargo bench -p book --bench book` and `cargo bench -p book --bench profile`. Timing reports use floating point outside execution code; prices and money remain integer/fixed point.

## Single-venue tools

```sh
cargo run -p cli --release --locked -- demo /tmp/lre-demo.lre
cargo run -p cli --release --locked -- replay /tmp/lre-demo.lre max
```

The single-venue demo refuses to overwrite a file. Replay modes: `max`, `step`, `1x`, `2x`, `10x`, `100x`. The merged replay currently supports maximum throughput and a library event-step API.

## Documentation

- [Phase 9 execution simulation: queue model, fees, funding, markouts, J(a), fill study](docs/phase9.md)
- [Phase 9 validation, live fill study and benchmarks](docs/phase9-validation.md)
- [Phase 8 live feeds, sync rules, atomic batches, composite reference, recording v2](docs/phase8.md)
- [Phase 8 validation, live sessions and benchmarks](docs/phase8-validation.md)
- [Provider documentation review: Binance, Bybit, OKX, Tiingo access and sync rules](docs/providers.md)
- [Phase 7 engine, environment, policy, fills, kill procedure and scenarios](docs/phase7.md)
- [Phase 7 validation and benchmarks](docs/phase7-validation.md)
- [Phase 6 inventory ledger, limits, accounts and episodes](docs/phase6.md)
- [Phase 6 validation and benchmarks](docs/phase6-validation.md)
- [Phase 5 flow definitions and integration](docs/phase5.md)
- [Phase 5 validation and benchmarks](docs/phase5-validation.md)
- [Phase 4 structure models and lifecycle](docs/phase4.md)
- [Phase 4 validation and benchmarks](docs/phase4-validation.md)
- [Phase 3 architecture, mathematical contracts and limitations](docs/phase3.md)
- [Phase 3 validation and benchmarks](docs/phase3-validation.md)
- [Original Phase 1–2 architecture](docs/architecture.md)
- [Versioned binary format](docs/recording-format.md)
- [Original foundation validation](docs/validation.md)
- [Milestones and remaining work](docs/milestones.md)

Core libraries use std and local crates. Serde/TOML are used only by CLI startup configuration; Criterion is a development dependency. No AI/ML inference, database, cloud service or async decision scheduler is included. Interface-only crates are explicitly marked; their presence does not imply working strategy or exchange integrations.
