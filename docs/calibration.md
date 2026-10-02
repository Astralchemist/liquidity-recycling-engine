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

## Results (session E, recorded 18:22–19:22 EAT)

Session E ran 3,600.4 s with 4–6 reconnects per venue and no fault. Its live replay reproduced all 49,159 commands exactly. [Raw output](calibration-output.txt).

### As registered

The commands were run exactly as committed (build `94dbd07`). Variants P2 and P3 **faulted at 714.4 s**: `Research(Voids(InvalidObservation))`.

This was a bug in the `suspend` change. While a zone was out of view, the price inside it was recorded as the side it was last seen on. A revisit then had no valid entry side.

| Session E, as registered | Cycles | Take-profit exits | Mean net per cycle | Std. error |
|---|---:|---:|---:|---:|
| Control | 388 | 128 | −4,774 atoms | 279 |
| P2 (to 714 s) | 7 | 1 | −7,144 atoms | 1,769 |

- **H1: fails.** P2 minus control is −2,370 atoms, t = −1.32: the wrong direction.
- **H2: fails.** P2's mean is negative.
- **H3: fails.** The difference was +3,605 in D and −2,370 in E.

### Deviation: crash fix, then rerun

The fix records only an outside side while a zone is unobserved. It changes no strategy parameter. Two regression tests, a targeted case and 2,000 generated sequences, fail without it.

With the fix, all variants were rerun on all sessions. D is unchanged.

| Session E, fixed build | Cycles | Take-profit exits | Mean net per cycle | Std. error | Mean gross per unit |
|---|---:|---:|---:|---:|---:|
| Control | 388 | 128 | −4,774 atoms | 279 | −136 ticks |
| P0, P1 (spec scopes) | 0 | — | — | — | — |
| **P2 (primary)** | 7 | 1 | −7,669 atoms | 1,900 | −425 ticks |
| P3 (diagnostic) | 10 | 2 | −6,646 atoms | 1,567 | −323 ticks |
| P3, no fees | 10 | 2 | −3,227 atoms | 1,567 | −323 ticks |

**H1, H2 and H3 all fail**; H1's t is −1.51.

### Why E produced only seven engine cycles

The research corridor is fixed at session start to 1,024 ticks (102.4 USDT). In E, BTC fell below it from about minute 12, and was at times 290 USDT under its floor. Outside the corridor no structure is visible, so no void formed.

- All 28 revisits happened in the first 15 minutes.
- Active zones were 0–2 for the remaining 45 minutes.
- No risk limit, halt or denial stopped the engine; see the timeline in the raw output.

The strategy was therefore tested on about 15 minutes of E. The ungated control trades on venue books and ran the full hour.

### Pooled (post hoc, not pre-registered)

Sessions B to E. A is excluded because positions still open at its 60 s end dominate it.

| | Cycles | Mean net per cycle |
|---|---:|---:|
| Control, ungated | 569 | −4,693 atoms |
| P2, revisit-gated | 13 | −4,580 atoms |
| P3, diagnostic | 20 | −5,139 atoms |

### Conclusion

- **The revisit gate adds no detectable value.** Within what top-20 public windows can observe, gated cycles are indistinguishable from ungated entries with the same exits.
- **The economics are negative.** Both lose about 4,600–4,700 atoms per 0.001 BTC cycle: about 47 USDT per BTC, roughly 0.054% of notional per round trip, at retail fees.
- **There is no gross edge before fees either.** P3 without fees lost 3,227 atoms per cycle in E and was positive only in D. Session D's encouraging result did not replicate.

What this does **not** rule out is equally specific:

- deeper books (top-200 or full depth)
- a re-centring corridor
- other exits, which were deliberately not fitted
- other assets or longer horizons
- programme rebates

Each would need its own pre-registered test.
