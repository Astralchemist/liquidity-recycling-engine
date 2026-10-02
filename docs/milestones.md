# Delivery sequence

## Completed: Phase 1–2 foundation

Integer dimensional primitives, exact decimal/quantum conversion, checked PnL, canonical events, one generic bounded L2 book, snapshots and incrementals, deterministic sequence processing, fail-closed validity, versioned binary recorder, single-venue replay pacing/stepping, CLI, property/invariant tests and release benchmarks.

## Completed: Phase 3 synchronous multi-venue core

Exact native/common-grid scale conversion and settlement arithmetic; explicit contract equivalence; independent venue books; weighted incremental consolidated depth; BBO, midpoint and divergence; per-price depth disagreement; stale/disconnect withdrawal; generic native sequence range validation and atomic book batches; TOML startup configuration; deterministic merged synthetic recording/replay; tests and allocation/latency benchmarks. See [Phase 3 contracts](phase3.md).

The three exchange adapter crates are still interfaces: these are synthetic venue streams. v1 recording cannot preserve native atomic batch boundaries. SPSC transport and CPU pinning are deferred until threaded ingress exists.

## Completed: Phase 4 liquidity structures

Fixed corridor with incremental weighted depth, configurable midpoint distance buckets, explicit sampled depth baselines, pool persistence/stability, bounded historical void lifecycle, local/multi-venue/market-wide source masks, immediate boundary-touch and partial-penetration revisits, refill/expiry/coverage censoring, deterministic semantic fixtures and full-state binary replay. Tests and structure/allocation/latency benchmarks are retained. See [Phase 4 contracts](phase4.md) and [validation](phase4-validation.md).

## Completed: Phase 5 order flow

Standard best-quote OFI and separately signed displayed-depth changes; bounded exact event/time windows with per-source epochs; best/top-N and percentage-bucket QI; count/exposure arrival and aggressive-trade rates; explicit cancellation-attribution policy; integer Poisson event probability; event-time bucket flow; normalized weighted OFI with source mask; shared research/replay integration and executable reporting. Fixed buffers allocate only at startup and are reused on resets. Tests and allocation/tail benchmarks are retained. See [Phase 5 contracts](phase5.md) and [validation](phase5-validation.md).

## Completed: Phase 6 inventory accounting

Exact one-quantum child ledger with configurable quantum, target, tolerance and completion gross. Reservation-based hard limits checked against worst-case fill ordering (net, gross, venue, target deviation, open orders). Latched kill state for inventory and PnL breaches, position age, episode duration, stale marks, adverse-episode streaks and caller-supplied faults.

The central inventory rule is enforced: a normal close must contribute net profit or improve the projected balance, while emergency exits after a halt can always realize a loss. The ledger also keeps:

- harvesting, liability and recovery accounts
- inventory episodes that partition portfolio PnL
- maker→maker, maker→taker and taker-escape counts
- a checksum-protected inventory command journal with exact replay

Commands are all-or-nothing transactions; history is committed only after success. The ledger performs zero allocations. Tests, the CLI demo/replay and allocation/latency benchmarks are retained. See [Phase 6 contracts](phase6.md) and [validation](phase6-validation.md).

Revisit evidence is still a caller attestation, fills are stipulated, and no executor yet performs the kill procedure.

## Completed: Phase 7 synthetic scenarios

One synchronous engine connects the Phase 4–5 research path, an explicit five-state environment classifier, the Phase 6 ledger, a rule-based recycling policy and the kill executor:

- **Classifier:** dead, balanced-active, trending, liquidity shock and chaotic, with transparent toxicity components.
- **Policy:** zone-gated entries, harvest, age- or threshold-triggered rebalance, and quote hysteresis.
- **Kill executor:** cancels entries, cancels normal closes, then emergency-flattens from the majority side.

Revisit evidence now comes from observed void revisits. Specification scenarios A–F are scripted on a synthetic three-venue L2 generator.

Tests check engine-wide invariants after every event. Recorded-market replay reproduces the complete engine state and inventory journal, a seeded policy sweep preserves the invariants, and zero allocations occur after startup. See [Phase 7 contracts](phase7.md), [validation](phase7-validation.md) and the [provider documentation review](providers.md).

Fills follow one simulated rule (strict trade-through); scenario PnL is not evidence of edge. The engine quotes one venue per zone and supports one active episode at a time.

## Completed: Phase 8 native public feeds

Live public feeds from Binance USDⓈ-M, Bybit linear and OKX swap (BTC/USDT perpetuals) are connected to the unchanged engine:

- **Decoding.** Allocation-free JSON scanning, exact decimal parsing and documented per-venue sync rules with resync on gaps.
- **Top-20 windows.** Each venue is canonicalized into atomic top-20 window batches.
- **Ingress.** Bounded threaded ingress with a single sequencer that stamps strictly increasing receive time and contiguous canonical sequences.
- **Atomic batches.** An incremental batch path through consolidation, research, flow and engine, with no structural rebuild.
- **Composite reference.** A cross-venue price reference, because live venues cross each other routinely.
- **Recording.** Format v2 with batch frames, a frame-aware merged replay, and exact live-versus-replay comparison of the complete engine state and journal.
- **Measurement.** Lead/lag tooling on one local clock, decode, queue and engine stage latencies, and decode benchmarks.

See [Phase 8 contracts](phase8.md) and [validation](phase8-validation.md).

Limits: windows are 20 levels per side; the corridor is fixed per session; recording I/O runs on the engine thread; engine thresholds are uncalibrated; lead/lag reflects arrival at this machine and venue push cadence.

## Next: Phase 9 execution simulation

Replace strict trade-through with a configurable maker queue and size simulator:

- queue position estimated from displayed size ahead
- partial fills
- size-aware prints
- retail fee schedules as the base case, with rebates only by programme
- slippage, funding and markout horizons (§18)
- net maker yield (§25)
- the objective J(a) (§24)

Calibrate on recorded live sessions rather than synthetic paths.

## Following deliveries

- Phase 10: demo exchange routing after risk gates, using the kill executor, order lifecycle and fault injection. Account and jurisdiction prerequisites are in [providers](providers.md).

Realistic fills, in-engine stage separation, order gateways and real-money connectivity remain unimplemented. No order is ever sent.
