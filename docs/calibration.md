# Calibration and strategy test — 2026-10-02

This document records how the engine's void detector and policy were calibrated on recorded public sessions, and how the strategy was then tested. Session E's test was **pre-registered**: this section and the variant files were committed before session E was replayed.

## Tools

- `lre policy-replay DIRECTORY STRUCTURES ENGINE` replays a recording through a fresh engine with override structure and engine files. It keeps the recording's market, flow and corridor. Input the live loop would refuse (`Engine::accepts`) is skipped, as live. It prints every closed child (`cycle …`) and cycle statistics (`engine_cycles …`).
- `lre cycle-control DIRECTORY HARVEST_TICKS AGE_S REPRICE_TICKS MAKER_FEE_PPM` is the **control**. It applies the engine's exit rules to **ungated** entries: one long and one short cycler per venue, each entering passively at the touch whenever flat. It uses the same proportional queue model, fees and stale-touch rule, and reports in the same format.

Comparing the two on one session isolates what the void-revisit entry signal adds.

## Sessions

| Session | Length | Role |
|---|---:|---|
| A | 58.6 s | Training |
| B | 294.6 s | Training |
| C | 601.1 s, fault at 550 s; replays cleanly with `accepts` | Training |
| D | 600.2 s | Exploratory: the only session with multi-venue revisits; looked at before E |
| E | 3,600 s, recorded 18:22–19:22 EAT | **Pre-registered test.** Not replayed before this section was committed. |

## Stage 1: detector sensitivity (training sessions only)

The sweep covered 72 configurations:

- `low_depth_ppm` ∈ {250k, 400k, 500k}, with `minimum_score_ppm` set to match
- `refill_depth_ppm` ∈ {800k, 950k}
- `minimum_persistence_ns` ∈ {100, 250, 500 ms}
- `minimum_covered_venues` ∈ {1, 2}
- `coverage_loss` ∈ {invalidate, suspend}

The thresholds hardly matter. Coverage decides the outcome:

| Covered venues | Registered per hour | Revisits per hour | Tradable under the shipped scopes |
|---:|---:|---:|---:|
| 2 | 45–64 | 0 | 0 |
| 1 | 98–136 | 11 | 0: every revisited zone was venue-local |

**Why.** Each venue publishes only its top 20 levels, about ±2 USDT around the touch on BTC. Three things end a void before it can be revisited:

- **Flicker.** A forming candidate is invalidated the moment its depth rises above the formation threshold.
- **Snapshot rebuilds.** Every snapshot after a reconnect bumps the consolidator's structural revision, and research then invalidates all zones (the OKX reconnect in B, the stall storm in C).
- **Coverage loss.** Under Phase 4 semantics a registered zone that leaves the visible window is invalidated, and an engine holding inventory on it halts with `StructureInvalidated`. In session D the shipped policy's single trade was forced out this way after 281 ms, and the engine stayed halted for the rest of the session.

### Model changes from calibration (defaults unchanged)

- **`void.coverage_loss = "suspend"`.** A registered zone out of view is unobserved, not invalidated. No depth judgement is made while it is unseen, and age expiry still applies. Price side is still tracked, so a revisited zone the price leaves returns to `Exited`. On return to view, refill is judged before any revisit. Candidates still forming are always invalidated out of view.
- **Unobserved levels.** `Engine::displayed` reports a level beyond the deepest visible level as unknown, not empty. A resting order there, such as a distant take-profit, now joins behind the displayed size when the level comes into view. Before, it sat at the front: optimistic.
- **`policy.entry_environments`.** Default balanced-active only (§13); other masks are diagnostics.

## Stage 2: exits from first principles, not fitted

- **Take-profit `harvest_ticks = 450`:** round-trip maker fees of about 346 ticks, plus about 55 ticks of measured 1 s adverse drift, plus a margin.
- **Exit after 60 s (`rebalance_age_ns`), at the touch.** Unchanged from the Phase 8/9 live configuration.
- **Fees and fills:** retail fees (maker 200 ppm, taker 500 ppm) and the proportional queue model.
- **No J(a).** It holds from flat at retail fees, so the objective would test nothing.

The training sessions produced at most two engine cycles per variant, so exits **cannot** be calibrated from data here; they are not.

Variants (files under `config/calibration/`):

| Variant | Covered venues | Coverage loss | Scopes | Entry environments | Fees |
|---|---:|---|---|---|---|
| P0 | 2 | invalidate | multi-venue, market-wide | balanced-active | retail |
| P1 | 2 | suspend | multi-venue, market-wide | balanced-active | retail |
| **P2 (primary)** | 1 | suspend | all, including venue-local | balanced-active | retail |
| P3 (diagnostic) | 1 | suspend | all | all non-chaotic | retail |
| P3-nofee | 1 | suspend | all | all non-chaotic | none |

## Exploratory result (session D, seen before E)

| Session D | Cycles | Take-profit exits | Mean net per cycle | Std. error | Mean gross per unit |
|---|---:|---:|---:|---:|---:|
| Control, ungated | 68 | 34 | −4,580 atoms | 801 | −116 ticks |
| Engine P2, revisit-gated | 6 | 4 | −975 atoms | 1,356 | +245 ticks |

The gate looked helpful (difference about +3,600 atoms per cycle, t ≈ 2.3). But that rests on 6 cycles from the one session examined before the test, so it is a hypothesis, not a finding.

## Pre-registered test (session E)

Each command is run once on E with the files committed alongside this document:

- `lre policy-replay E config/calibration/p2-structures.toml config/calibration/p2-engine.toml`, plus the same for P0, P1 and P3
- `lre cycle-control E 450 60 5 200`

| Question | Measure | Pass |
|---|---|---|
| H1. Does the revisit gate select better cycles than ungated entries with identical exits? | Mean net per cycle, P2 minus control; Welch t | t > 2 |
| H2. Is the strategy viable at retail fees? | P2 mean net per cycle | Above 0 with t > 2 |
| H3. Does it hold beyond one session? | The H1 difference has the same sign in D and E | Same sign |

All variants and both sessions are reported whatever they show. A cycle is one child from fill to close. The control's cycles are serially correlated (one price path, six cyclers), so its standard error is optimistic.

## Results

Pending: session E is still recording.
