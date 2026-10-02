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

## Follow-up studies (exploratory; no holdout remains)

Both studies were requested after E's results, so they use every session and have no untouched test data. They describe these recordings; they do not confirm anything. Graphs: `docs/evidence/index.html`.

### Loosening the two-sided entry

The rungs, each with identical exits, fees and risk limits (files in `config/calibration/`):

- **L0** is P1.
- **L1** is P2.
- **L2** is P3.
- **L3** loosens the detector to a 50% depth threshold and 100 ms persistence, with 5 s revisit evidence.
- **L4** adds two quote levels per side and net inventory up to 4.
- The **ungated dual** is the control.

Pooled over sessions B–E (1.42 h):

| Rung | Round trips | Per hour | Mean net per trip | ± SE |
|---|---:|---:|---:|---:|
| L0 | 1 | 0.7 | +1,079 atoms | — |
| L1 | 13 | 9.2 | −4,580 | 1,501 |
| L2 | 20 | 14.1 | −5,139 | 1,162 |
| L3 | 26 | 18.4 | −4,970 | 1,088 |
| L4 | 59 | 41.7 | −5,055 | 547 |
| Ungated dual | 569 | 402.4 | −4,693 | 263 |

**Loosening is detrimental.** It multiplies trades without changing the loss per trip, so losses scale with activity.

### Momentum after a mega pump (`scripts/momentum.py`)

- **Candles:** each venue's trade prints by receive time.
- **Pump:** a candle move of at least k standard deviations of the previous 120 candles.
- **Continuation:** the next candle moves in the pump's direction. The base rate is the same probability over every pair of consecutive candles.
- **Taker test:** enter at the pump's close and leave one candle later, paying the taker fee both ways. This is an upper bound: real entries cross the spread after a latency.

Excursion and the net taker result are in USDT per BTC.

| Venue | Candle | Pump | Pumps | Per hour | Continue / reverse / flat | Continues | Base | z | Run lasts | Excursion | Next candle, net of taker fees |
|---|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|
| Binance | 1s | ≥3σ | 106 | 74.9 | 50 / 39 / 17 | 56.2% | 58.2% | -0.39 | 2.1 s | 16.1 | -85.3 |
| Binance | 1s | ≥5σ | 17 | 12.0 | 6 / 11 / 0 | 35.3% | 58.2% | -1.92 | 1.0 s | 13.0 | -88.4 |
| Binance | 5s | ≥3σ | 11 | 7.8 | 5 / 6 / 0 | 45.5% | 51.2% | -0.38 | 9.0 s | 56.6 | -79.9 |
| Binance | 5s | ≥5σ | 2 | 1.4 | 2 / 0 / 0 | 100.0% | 51.2% | +1.38 | 7.5 s | 34.0 | -63.9 |
| Bybit | 1s | ≥3σ | 95 | 67.1 | 50 / 39 / 6 | 56.2% | 58.0% | -0.35 | 2.0 s | 15.1 | -93.5 |
| Bybit | 1s | ≥5σ | 13 | 9.2 | 6 / 6 / 1 | 50.0% | 58.0% | -0.56 | 1.5 s | 8.8 | -94.1 |
| Bybit | 5s | ≥3σ | 11 | 7.8 | 5 / 6 / 0 | 45.5% | 52.8% | -0.49 | 14.0 s | 77.5 | -86.4 |
| Bybit | 5s | ≥5σ | 1 | 0.7 | 1 / 0 / 0 | 100.0% | 52.8% | +0.95 | 10.0 s | 54.2 | -58.5 |
| Okx | 1s | ≥3σ | 115 | 81.2 | 56 / 38 / 21 | 59.6% | 55.7% | +0.77 | 1.8 s | 11.1 | -85.8 |
| Okx | 1s | ≥5σ | 18 | 12.7 | 6 / 9 / 3 | 40.0% | 55.7% | -1.22 | 1.0 s | 1.5 | -90.5 |
| Okx | 5s | ≥3σ | 8 | 5.7 | 3 / 5 / 0 | 37.5% | 56.7% | -1.10 | 15.0 s | 77.4 | -89.5 |
| Okx | 5s | ≥5σ | 1 | 0.7 | 0 / 1 / 0 | 0.0% | 56.7% | -1.14 | — | — | -188.5 |

**No momentum edge.**

- **Frequency.** 3σ one-second pumps occur about 67–81 times an hour per venue.
- **Continuation.** The next candle continues 56–60% of the time, the same as the 56–58% base rate.
- **Duration.** A continuation lasts about 2 s, with a mean excursion of 11–16 USDT per BTC.
- **5σ pumps.** They continue less often than the base rate.
- **Cost.** The best possible exit at the run's peak does not cover the roughly 87 USDT per BTC of round-trip taker fees.

Moves of 15 s and 60 s were too rare to measure in 1.4 hours. On BTC these are 15–30 USDT spikes, not the multi-fold pumps of small DEX tokens.

