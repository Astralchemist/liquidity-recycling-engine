# Phase 4: liquidity structures and observed revisits

## Ownership and event path

One synchronous `ResearchEngine` owns the Phase 3 `Consolidator`, a `LiquidityEngine`, and a bounded `VoidEngine`. Canonical events follow: venue book → normalized consolidated depth → changed grid level → sampled formation statistics → per-event zone lifecycle. Direct inputs and binary merged replay call the same `apply` method. There are no threads, locks, network calls, file writes, dynamic collections, or schedulers inside this path.

`advance_time` applies freshness and lifecycle checks using a supplied monotonic timestamp. Faults latch; callers must stop using the research state for decisions and reconstruct from valid inputs. Read access remains available for audit. No orders are generated in this phase.

## Layout, units, and bounds

Prices are positive integer common-grid ticks; midpoints use doubled ticks to preserve half ticks. Depth is integer common-grid quantity multiplied by venue weight in parts per million (ppm), without rounding each contribution. Divide weighted depths by 1,000,000 only for reporting common quantity units. Ratios and scores use ppm; durations use monotonic nanoseconds.

`PriceGrid<V,P>` contains per-venue dense bid/ask arrays, cell totals, two weighted Fenwick sum arrays, and displayed-price birth timestamps. Capacities are fixed before processing: `V <= 64`, `P <= 4096`. Quantity is nonnegative `i64`; weights are at most 1,000,000. These bounds keep aggregate depth and intermediate ratio products within `i128`. Grid updates and range sums cost O(log P); source-cell depth access costs O(1). Formation sampling visits C cells and V venues; active-zone observation visits at most Z slots and V sources per zone. Book maintenance retains the bounded Phase 3 algorithms.

CLI capacities: 3 venues, 128 levels per book side, 384 consolidated levels per side, 64 corridor ticks, 6 distance buckets, 16 zone slots. Arrays reserve maximum cell storage at P; only C=P/cell_width cells are used. The corridor is `[lower_price, lower_price+P-1]`. Out-of-corridor changes update the full market book but do not enter this research grid. Snapshot activation or source withdrawal rebuilds the grid from committed books, resets baselines, and invalidates active zones; ordinary events update one affected level.

## Coverage contract

Missing input is not zero liquidity. `assume_contiguous_l2_coverage` must be explicitly enabled for this prototype to infer that omitted interior prices have zero displayed depth. With that assumption, a valid two-sided venue covers the interval from its worst retained bid to worst retained ask. A cell must fit entirely inside that interval. This requires a complete snapshot and ordered absolute L2 updates inside the retained range. Truncated, sampled, or selectively published feeds must not enable this assumption without an adapter establishing the contract.

Only valid, included, positive-weight venues count. At least `minimum_covered_venues` must cover a cell. A source losing coverage loses its baseline. A covered-venue-set change restarts pool persistence. Crossed/locked/one-sided global markets suspend research and invalidate active zones. Book/source membership changes conservatively censor existing observations. Per-source sequence, freshness, metadata and shared local-clock requirements remain those of Phase 3.

## Two region representations

Historical structures use fixed absolute cells: cell j contains `lower+j*width` through `lower+(j+1)*width-1`, inclusive. Geometric zone width is `upper-lower`, so four included integer prices span three ticks. Cells never recenter or merge; this avoids moving a historical zone as price changes. Grid origin and width affect counts and must be retained with results.

Reporting buckets use relative distance from the current consolidated midpoint. Each bucket is half-open `[inner_ppm, outer_ppm)`, measured below midpoint for bids and above midpoint for asks. Bounds are derived by integer inequalities, including half-tick midpoints. Queries return weighted bid/ask depth, displayed-price count, mean displayed-price age, and whether the requested range fits within the configured corridor. Corridor completeness alone does not certify source coverage. L2 does not supply individual order counts; `order_count` is `None`.

Age starts when weighted aggregate depth at a price changes from zero to positive. It ends at zero and resets after structural reconciliation. It is an observed displayed-price age, not order lifetime or queue priority. Reporting age queries scan the relevant fixed ranges; they are outside the per-event update path. The supplied narrow percentage buckets can be empty on the deliberately coarse synthetic tick grid.

## Explicit mathematical definitions

Each cell/source maintains a baseline B of unweighted normalized depth D (bid + ask). The first covered sample seeds B. Subsequent samples update:

```text
B_next = B + trunc_toward_zero((D - B) * alpha_ppm / 1_000_000)
DepthNorm = floor(D * 1_000_000 / max(B, 1))
```

A baseline is mature after `warmup_samples` and only if B reaches `minimum_baseline_units`. A mature baseline freezes when `DepthNorm <= low_depth_ppm`, preserving a reference while a void persists. Otherwise the EWMA continues. Statistics compare against the baseline before that sample's update. Evidence freezes the resulting reference when a candidate is created. This is a configurable event-sampled EWMA, not a time-weighted integral; sample cadence therefore affects the estimates. Samples happen on the first event at or beyond each configured interval. There are no fabricated catch-up observations during idle periods.

Pool depth ratio compares weighted depth and baselines from mature covered sources. Stability and persistence are independent visible terms:

```text
Stability = 1 - abs(D - D_previous) / max(D, D_previous, 1)
Persistence = min(1, continuous_above_threshold_ns / pool_persistence_ns)
PoolScore = (w_depth*DepthNorm + w_persistence*Persistence + w_stability*Stability)
            / (w_depth + w_persistence + w_stability)
```

All terms are computed in integer ppm, with truncation. Ratios saturate at `u32::MAX`; stability/persistence are capped at 1,000,000. Activation requires sufficient coverage, mature baseline, depth ratio threshold, minimum duration, and minimum score. Bid/ask/two-sided describes displayed cell depth; the venue mask identifies sources individually exceeding the pool threshold. This is displayed liquidity evidence, not proof that orders will remain executable. `pool_observed_ns` sums sampled intervals between consecutive active observations, across cells; it is not continuous-time certainty.

The general void-score function exposes depth deficit, executed-volume deficit and cancellation terms with configurable weights. A nonzero unavailable-flow weight is rejected. The Phase 4 pipeline explicitly uses depth deficit alone (`[1,0,0]`); execution and cancellation components await Phase 5. Both low-depth and score thresholds gate formation.

## Void lifecycle

1. A mature cell falls below the low-depth threshold on one or more covered sources. Store region, source mask, weighted depth/reference, score and creation time.
2. It must remain below the threshold for the minimum persistence duration. A pending source-mask change restarts this duration and freezes new evidence.
3. Registration requires an observed inside-to-outside price transition after persistence. An early exit alone does not qualify: reentry and a later exit are required.
4. A registered zone activates a revisit immediately when the configured price reference enters the inclusive bounds, including an exact boundary touch.
5. Entry from below measures `(price-lower)/(upper-lower)`; entry from above measures `(upper-price)/(upper-lower)`. Doubled prices preserve half ticks. Penetration is capped at 100%.
6. Remaining inside is the same visit. Leaving and returning creates another visit. Each visit contributes exactly one histogram count at its greatest penetration so far: touch, (0,25%), [25,50%), [50,75%), [75,100%), or full. Historical maximum is tracked separately.

A second full traversal is never required. Jumping directly between opposite outside observations is counted as `skipped_crossings`, not an observed revisit; the intervening path is unknown. Once a revisit has been observed, an exit through the opposite boundary records full penetration.

`price_reference` is either consolidated midpoint or most recent included-source trade price. It must be fixed for a research run. Last trade is held between prints and has no independent trade-age gate in this phase; book freshness still applies. `last_touched_at` records observed in-zone timestamps during revisits, not formation occupancy.

Every event checks active zones, even between formation samples. Coverage loss, expiry, and refill take precedence over a simultaneous revisit. Refill uses current depth on the frozen formation sources against the frozen baseline, at `refill_depth_ppm > low_depth_ppm`. Refilling a registered zone terminates it; refill before its first revisit is counted separately. A pre-registration loss of thinness invalidates the candidate. Maximum age is measured from registration, or creation if unregistered.

`VenueLocal` means one depleted source; `MarketWide` means all configured positive-weight sources (at least two); other subsets are `MultiVenue`. Source masks are retained, and local depletion can be detected even when other venues keep the consolidated region liquid.

## Capacity, reproducibility, and interpretation

Overlapping active regions with different bounds are rejected. An identical pending region updates its source cohort as described; registered source masks remain frozen. A new zone takes an empty slot, then the oldest terminal ID if necessary. Active zones are never evicted. Capacity/overlap rejections and terminal evictions are counted. Retained objects are bounded; cumulative counters survive terminal eviction. Complete long-history zone export is future work.

Observed revisit rate is `revisited_zones / registered`, undefined at zero registrations. Mean first-revisit time is the accumulated first-revisit delay divided by revisited zones, undefined if none revisited. Counts include observed windows of different lengths; expired, invalidated, refilled-before-revisit, and still-active zones must be considered before interpreting a rate statistically. This is not an unbiased eventual-revisit probability. Histogram totals equal revisit events, not unique zones.

The CLI persists both TOML configurations beside checksum-protected v1 event recordings. Direct/replay full-state equality is asserted. Native exchange depth messages may update many levels atomically: v1 has no batch envelope, and the research entry point currently accepts single canonical events. Do not flatten native batches and claim equivalent research replay; batch-aware recording/integration remains a Phase 8 gate.

## Executable fixtures and next scope

`structure-demo MARKET_CONFIG STRUCTURE_CONFIG DIRECTORY [revisit|local|refill|continuation]` generates stable depth, a pool, depletion, persistence, exit, and scenario-specific revisit/refill behavior. `structure-replay DIRECTORY` uses the saved inputs. Fixtures use common tick/lot scales and deterministic print prices to isolate lifecycle behavior. Prints do not consume the synthetic book; these are semantic tests, not an exchange matching engine, executable-price model, or profitability simulation.

Next is Phase 5: incremental OFI, queue imbalance, arrival/cancellation/aggression estimators and bucket flow rates. Inventory, toxicity/markout, risk/kill actions, live native feeds, queue simulation and exchange routing remain later milestones.
