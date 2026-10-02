# Phase 7: synthetic scenarios through one engine

## Scope

Phase 7 connects the Phase 4 void engine, the Phase 5 flow measurements and the Phase 6 ledger into one synchronous engine. It adds three things:

- an explicit environment classifier
- a transparent rule-based recycling policy
- a kill executor

It drives all of this from synthetic three-venue markets that implement specification §39 scenarios A–F.

Fills are **simulated by one stated rule**. Profit and loss here measure bookkeeping and rule behaviour under that rule. They are not evidence of executable edge.

There is no machine learning, no forecasting, no floating point in engine paths, no allocation after startup, no I/O in `apply`, and no order submission.

## Components

| Crate | Content |
|---|---|
| `toxicity` | `EnvironmentTracker<W>`: bounded exact window, five-state classifier. `ToxicityComponents` with an optional explicit weighted mean. |
| `simulation` | `SyntheticMarket` operations (sweeps, refills, bands, idle) and `scenarios::generate` for A–F. |
| `engine` | `Engine<V,N,G,P,B,Z,W,L,E>`: shared research path, environment, ledger, policy, fill rule and kill executor. `fixtures` holds the synthetic configuration. |
| `risk` | New `KillReason::RegimeChange`, journal code 14. |
| `cli` | `scenario-demo`, `scenario-replay` and `scenario-suite`. |

`Engine::apply(&MarketEvent, sink)` is the single entry point for direct input and recorded replay. Each accepted inventory command goes to the caller's sink for journaling. Denied commands are counted, never journaled.

## Event order

For every market event, `apply` runs these steps in order:

1. **Research.** The Phase 5 `FlowResearchEngine::apply`, which wraps Phase 4. A research error latches `research_fault` and issues `Halt`:
   - a sequence gap maps to `SequenceGap`
   - a clock regression maps to `ClockAnomaly`
   - a stale book maps to `StaleData`
   - a disconnect maps to `Disconnect`
   - anything else maps to `CorruptBook`
2. **Environment.** Update with the consolidated midpoint and spread (only while the market is open), top-of-book quantity, and any print's aggressor side and common quantity. A tracker error halts with `UnhandledState`.
3. **Divergence.** Record each venue's midpoint minus the global midpoint (`max_abs_divergence_x2`, `divergent_events`).
4. **Mark.** Mark the event's venue when its normalized touch changed, or `mark_refresh_ns` has passed since the last mark.
5. **Fill rule.** Applied on trades only.
6. **Tick.** A ledger `Tick` when no command ran and `assess_interval_ns` has passed since the last one, so age and staleness limits are evaluated during quiet periods.
7. **Policy.** Runs if not halted and healthy.
8. **Kill procedure.** Runs if halted. It re-runs on every later event until the book is flat.

## Environment state machine (§13)

One window over `(now − window_ns, now]` holds an entry for every event that changed the midpoint, changed the spread, or printed. Sums update incrementally:

- moves
- `path = Σ|Δmid|` and `net = ΣΔmid`, both in doubled ticks
- spread changes
- aggressive buy and sell quantity
- prints

Rules for the window:

- A move is measured only between two open-market midpoints; reopening is not a move.
- Capacity W is fixed. A full window of unexpired entries returns `Capacity` **before** any state changes, and the engine halts. Data is never silently dropped.

Classification is first match:

| State | Condition |
|---|---|
| `LiquidityShock` | market not open, or spread `> shock_spread_ticks`, or top bid + ask quantity `< shock_min_top_qty` |
| `Chaotic` | spread changes `≥ chaotic_spread_changes` |
| `Trending` | (`\|net\| ≥ 2·trend_min_move_ticks` and `\|net\|·10⁶ ≥ trend_efficiency_ppm·path`), or (`buy+sell ≥ trend_min_aggressive_qty` and `\|buy−sell\|·10⁶ ≥ trend_aggression_ppm·(buy+sell)`) |
| `Dead` | moves `< active_min_moves` and prints `< active_min_prints` |
| `BalancedActive` | otherwise |

The initial state is `LiquidityShock` until the first observation. Transitions and nanoseconds spent in each state are reported. There is no hysteresis or dwell time: the state is a pure function of the window. If flapping proves to be a problem, a dwell rule can be added explicitly later.

`ToxicityComponents` exposes these components, computed on demand and not per event:

- aggression imbalance ppm
- depletion ppm (removed / (added + removed), from Phase 5 time windows)
- cancellation ppm, which is `None` under unknown attribution
- spread
- spread changes

`score_ppm(weights)` is a weighted mean over the **available** components. It is reported only; no decision uses it.

## Recycling policy

All parameters are in `PolicyConfig`.

**Regime exit.** If inventory is held and the environment's bit is in `exit_on_environment`, the engine issues `Halt{RegimeChange}`. The default exits on `Chaotic` only. Exiting on `LiquidityShock` would contradict the thesis that toxic fills can be recovered after replenishment; scenario E's variant shows that configuration explicitly.

**Structure invalidation.** If inventory is held and the active episode's void is `Invalidated`, the engine issues `Halt{StructureInvalidated}`. `Refilled` is not an invalidation: it is the replenishment the thesis expects.

**Entries.** The target is the lowest-id zone in state `Revisited` whose scope bit is in `allowed_scopes`; the default excludes venue-local voids.

- Entries are placed only while the environment is `BalancedActive`. Otherwise every resting entry is cancelled.
- The engine quotes `entry_levels` quanta per side on the zone's lowest-index venue: bids at `bid − level`, asks at `ask + level`.
- A side is skipped when its worst-case fills would take `|net|` above `entry_max_abs_net`, counting pending and closing orders.
- Revisit evidence carries the zone id, `last_touched_at` and `now + evidence_ttl_ns`. The ledger rechecks it.
- **Quote hysteresis:** a resting entry is cancelled and re-placed only when the desired price has moved at least `reprice_ticks`.

**Rebalance.** One rebalance close at a time, on the excess side of `net − target`. It is triggered when `|net − target| > rebalance_threshold_units`, or when an excess-side child is at least `rebalance_age_ns` old.

- Price: the child's venue touch, improved by up to `rebalance_improve_ticks` while staying strictly inside the spread. It is always passive.
- Candidate order: least-losing child first, then id.
- If the child has a harvest close, that close is cancelled first. A mirror of the ledger's central rule decides beforehand whether the close can be approved, which avoids cancel-and-deny churn.
- The ledger remains authoritative, so a loss is realized only when it improves the projected balance.

**Harvest.** Every child without a pending close gets a take-profit close at `entry ± harvest_ticks`.

## Fill rule (simulated)

A trade print on venue v at common price p, with an aggressor side, fills every resting simulated order on v that it trades **strictly through**:

- an aggressive sell fills resting buys priced above p
- an aggressive buy fills resting sells priced below p

The fill is at the resting price, as maker, with `maker_rebate` atoms. Print size and queue position are ignored.

The rule is pessimistic about adverse selection: a touch order fills only after price has moved through it. It is optimistic about size, since any through-print fills every eligible order. Phase 9 replaces it with a configurable queue and size simulator; `fills.rule` accepts only `"strict_trade_through"`.

**Fee model.** Fixed atoms per one-quantum fill: `maker_rebate` and `taker_fee`. Realistic schedules are Phase 9 work. The researched venue schedules are in [providers](providers.md).

## Kill procedure (§41)

When the ledger is halted:

1. Cancel every resting entry (`CancelOpen`).
2. Cancel every normal close (`CancelClose`).
3. Emergency-flatten one child at a time from the majority side, so the net checks always admit an exit.
   - Price: the venue touch minus `emergency_slippage_ticks` (for longs at the bid), or plus for shorts at the ask.
   - Fallback: if the research path is faulted, the **ledger's last mark** for that venue.
   - Each exit is `ReserveClose{Emergency}` followed immediately by `FillClose` as taker with `taker_fee`.

Stopping new inventory is the ledger's halt latch. Persisting state is the command journal.

An exit that cannot be priced increments `unpriced_emergency_steps` and is retried on the next event. The tests assert that a priced halt is flat at the end of the same event.

## Scenarios (§39)

The market is a 64-tick corridor [80, 143] at 100 units per level, with 1 ms per event on three venues of identical scale. Each venue has a contiguous ladder with a two-tick spread and an empty midpoint level.

- A sweep prints at the best level and removes it.
- A refill adds a level one tick inside the spread.
- A step is one sweep plus one refill. Venues move in index order.

**Shared prelude.**

1. Snapshot, then warm up.
2. The band [100,103] (one Phase 4 cell) thins to 5 units on the masked venues.
3. Price steps from 118 down to 96, traversing the band and exiting below, which **registers** the void.
4. Price chops away from the band.
5. Price climbs back to 101, which is the **revisit**: a lower-boundary touch followed by partial penetration.

| | Script after the prelude | Purpose |
|---|---|---|
| A | 12 in-zone oscillations, choppy drift up (+2/−1), long two-tick oscillation | recycling during two-way movement |
| B | 4 oscillations, then 28 ticks straight up | violent continuation |
| C | as A, but the band thins on venue 1 only | venue-local void |
| D | as A, with a staircase lead: venue 1 moves one tick first, the others follow | cross-venue movement without locking the consolidated book |
| E | 3 oscillations, a 5-level sell sweep, asks replenished, two-way recovery, drift, oscillation | toxic fill, then replenishment and recovery |
| F | 3 oscillations, a 5-level sweep, then 6 more 2-level sweeps with no replenishment | toxic fill without replenishment |

A two-tick lead would lock the two-tick consolidated book and invalidate every zone (Phase 4 suspends on locked or crossed markets). That is why D uses a one-tick staircase.

## Configuration

These four files mirror `engine::fixtures` exactly, and a CLI test asserts equality:

- `config/phase4-market.toml`
- `config/phase7-structures.toml`: midpoint price reference, 30 s void age
- `config/phase7-flow.toml`: 100 ms windows, so 128 slots per venue are never exhausted at 1 ms spacing
- `config/phase7-engine.toml`: sections `[environment]`, `[policy]`, `[fills]`, `[inventory]`, `[risk]`

Scope and environment names are validated strings. Money values are atom strings. Unknown fields are rejected.

**Validation.** `Engine::new` validates the policy, including:

- refresh interval below the ledger's stale limit
- ledger venue order equal to the market venue order, because zone masks index it
- entry net at or below the hard net limit

```sh
cargo run -p cli --release --locked -- scenario-suite config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml
cargo run -p cli --release --locked -- scenario-demo config/phase4-market.toml config/phase7-structures.toml config/phase7-flow.toml config/phase7-engine.toml /tmp/lre-phase7-e e
cargo run -p cli --release --locked -- scenario-replay /tmp/lre-phase7-e
```

`scenario-demo` writes the four TOML files, three binary venue recordings and the inventory journal. It then replays the recordings through a fresh engine and requires the identical complete engine state and command stream. Finally it rebuilds the ledger from the journal file alone and requires equality.

## Assumptions and limits

- **Synthetic grid.** The market uses common native scales. Book depth is a profile, not an empirical distribution. Divergence is half a tick at most, and is frequent in every scenario because venues update sequentially.
- **Gridlock.** Offsetting long and short children whose take-profits lie outside the current range can coexist at net 0. The central rule correctly refuses to close a losing child that does not improve balance, so entries pause until price moves. This is a property of the specification's rule, not a defect. Its frequency is a research question.
- **Single quote venue.** The engine quotes only the zone's lowest-index venue, with one entry ladder per side and one active episode at a time. Multi-venue quoting and cross-venue hedging are not implemented.
- **Environment.** Thresholds are fixed and there is no dwell rule. The same window feeds trend and aggression tests.
- **Latched halts.** Halts are permanent for the life of the engine. Scenario runs are single sessions.
- **Performance.** Engine overhead on top of the research path is about 400 ns per event at these capacities; see [validation](phase7-validation.md). The rebalance candidate scan sorts at most L children.
- **Deferred:**
  - **Phase 8:** native feeds and batch-aware recording ([providers](providers.md) lists the documented sync rules), lead/lag cross-correlation (§28) and stage latencies (§30).
  - **Phase 9:** markouts (§18), queue position (§26) and the objective J(a) (§24).
  - **Phase 10:** demo gateways.
