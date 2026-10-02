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

## Next: Phase 8 native public feeds

Binance, Bybit and OKX public adapters following [providers](providers.md):

- documented snapshot, sequence and keepalive rules
- atomic depth batches through `apply_depth_batch`
- a versioned batch envelope in the recording format
- bounded threaded ingress with SPSC transport
- local monotonic receive stamps
- exact live/replay comparison
- lead/lag cross-correlation tooling and stage latency measurement

No account or contact is required, except OKX tick-by-tick L2 (VIP4); start with `books` and `bbo-tbt`.

## Following deliveries

- Phase 9: configurable maker/queue/size simulator replacing strict trade-through. Explicit fees with positive retail maker fees as the base case, rebates only by programme, slippage, funding, markout horizons, net maker yield, and the objective J(a).
- Phase 10: demo exchange routing after risk gates, using the kill executor, order lifecycle and fault injection. Account and jurisdiction prerequisites are in [providers](providers.md).

Full latency stages, lead/lag research, realistic fills and real exchange connectivity remain unimplemented. No real-money gateway is enabled.
