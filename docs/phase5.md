# Phase 5: deterministic order-flow measurements

## Model and scope

This milestone measures flow and queue imbalance. It does not forecast direction, choose orders, estimate trading edge, or simulate inventory. All execution-path prices, quantities, rates and probabilities use integer arithmetic. Floating-point reference calculations exist only in numerical tests and benchmark reporting.

The best-quote OFI definition follows [Cont, Kukanov and Stoikov, §2.1](https://arxiv.org/html/1011.6402). For previous/current best bid `(b0,qb0)/(b1,qb1)` and best ask `(a0,qa0)/(a1,qa1)`:

```text
e = I(b1 >= b0)*qb1 - I(b1 <= b0)*qb0
  - I(a1 <= a0)*qa1 + I(a1 >= a0)*qa0
OFI(window) = sum(e)
```

The implementation requires two valid, positive, uncrossed best quotes. Quote-price improvements and deteriorations use the appropriate new or old queue sizes. Equal prices reduce to bid-size change minus ask-size change. This is an accounting statistic; the paper's empirical price-impact findings are not imported as a trading rule.

Separately, `DepthDelta` records the signed size change at the affected displayed level: bid change is positive, ask change is negative. A non-best update can change this measure while best-quote OFI remains zero. Neither measure is silently substituted for the other.

## Component ownership and integration

`FlowResearchEngine` owns the unchanged Phase 4 `ResearchEngine` plus an independent `FlowEngine` per venue. It captures pre-event quotes and affected-level quantity, calls the same production research `apply`, then records the normalized contribution after book validation. Formation and revisit calculations retain their original event ordering; a regression test compares the nested Phase 4 state after every event against an independently run Phase 4 engine.

Before assigning an event's distance bucket, a nonmutating freshness query computes the pre-event midpoint from fresh positive-weight sources. The subsequent market update performs actual stale-source withdrawal. This avoids attributing flow to a stale midpoint or advancing formation sampling ahead of the depth update.

Each venue retains its own flow epoch, rolling windows and exposure. Snapshot building, staleness, or loss of either book side ends the usable flow epoch. A newly valid two-sided book seeds a new epoch; snapshot quantities and transitions across missing observations do not become arrivals or OFI. Other venues keep their own flow history. A trade does not rejuvenate stale depth. The public `advance_time` expires windows and enforces book freshness during idle periods. The caller must supply monotonic time; there is no background scheduler.

Errors latch at the wrapper: clock regression, bad sequence, normalization/book errors, or flow capacity exhaustion stop further events and all flow queries. The nested research accessor remains available for audit, and must not be used to bypass a wrapper fault. It may already contain the triggering market event if a downstream flow calculation failed; recovery requires reconstructing from valid input, not resuming the partial event.

## Contributions and attribution

Canonical Add/Modify quantities are absolute sizes. The contribution is `new_size - old_size`; Cancel sets new size to zero. Quantities are converted exactly to common-grid units before flow arithmetic.

| Metric | Definition |
|---|---|
| `Ofi` | Best-quote contribution above |
| `DepthDelta` | Signed displayed-level quantity change |
| `AddedQty` / `AddEvents` | Positive quantity changes and count of such changes |
| `RemovedQty` / `RemovalEvents` | Absolute negative quantity changes and count of such changes |
| `BuyQty` / `BuyEvents` | Reported aggressive-buy quantity and print count |
| `SellQty` / `SellEvents` | Reported aggressive-sell quantity and print count |
| `CancelQty` / `CancelEvents` | Assumption-attributed removals, usable only under the explicit policy below |

Canonical trade side means aggressor side. Trade prints update aggression statistics and contribute zero OFI/depth change. Their later depth updates supply the displayed-book effect. Prints are never also applied as implicit book decrements. An aggregated print counts as one event, not its unknown number of component fills.

Default `removal_attribution = "unknown"` preserves removals without calling them cancellations. `"assume_cancellation"` explicitly treats every reduction as cancellation, including reductions that could be executions. It is a research assumption, unsuitable as verified cancellation data. Neither timestamp proximity nor quantity similarity is used to guess execution/cancellation matches.

`FlowSnapshot::cancellation_rate`, `time_rate` for cancellation metrics, and `bucket_time_rate` return `None` for unknown attribution. Raw cancellation accumulator storage is zero in that mode and must not be interpreted as an observed zero cancellation rate. Standalone contributions must match the configured attribution policy.

## Exact bounded rolling windows

Each source has two independent circular buffers:

- Event window: last `event_window` eligible canonical depth/trade events from that source, including zero-change updates and trade prints. Snapshot rows/markers are excluded. Oldest entries are evicted at the configured count.
- Time window: every eligible event in `(now-time_window_ns, now]`. Age exactly equal to the horizon expires. Equal timestamps preserve canonical input order and can consume multiple slots.

Totals and per-bucket totals update by adding the new contribution and subtracting expired/evicted contributions. There is no history-wide rescan. Event-window eviction is O(1); time expiration is O(1) amortized per recorded event, with a bounded O(W) burst after an idle gap. `advance_time` changes the time window but leaves the count window intact.

The compile-time capacity W bounds each buffer; `1 <= event_window <= W <= 4096`. CLI W is 128 per source per window. A full, unexpired time buffer produces `Capacity` and a latched failure. It never drops an in-window event or silently changes the estimator. Choose capacity and horizon using peak message rate plus burst margin; the example one-second window is for the small synthetic fixture, not a high-rate live-feed configuration.

Buffers are fixed boxed arrays allocated at startup (two per source). They never grow. Reset clears and reuses the buffers, avoiding allocations on snapshot/stale lifecycle changes. This avoids placing large history arrays on thread stacks. All processing is single-owner and synchronous; no lock, database, RPC, JSON serialization or async scheduling is introduced. Explicit engine clones allocate and are for tests/control paths, never per-event processing.

Inputs are bounded by `i64` quantity, at most four such magnitudes per OFI contribution, W <= 4096, and at most 64 venues. Aggregate arithmetic uses `i128`. Public queue/rate helpers check multiplication/addition where arbitrary inputs could overflow. Counts use integer accumulators and quantities never use floating point.

## Intensities and probability

For a continuously usable source epoch starting at s:

```text
exposure_ns = min(now - s, time_window_ns)
lambda_micro_events_per_second = floor(count * 10^15 / exposure_ns)
quantity_rate_micro_units_per_second = floor(quantity * 10^15 / exposure_ns)
```

No exposure means `None`, including a burst at the exact start timestamp. A mature, empty window yields an observed zero rate. Time-window estimates include idle time; counts are not divided by the span between first and last observed arrivals. Event-window totals do not imply a time rate. Signed rates truncate toward zero.

Arrival intensity here refers to observed positive displayed-size updates or reported trade prints, depending on the selected metric. L2 does not reveal individual order arrival counts. No stationarity or Poisson goodness-of-fit claim is made; the rolling count/exposure estimate is a baseline for research. Exponential weighting and alternate estimators can be added behind these explicit definitions later.

`poisson_event_probability_ppm(lambda, horizon)` approximates `1-exp(-lambda*horizon)`. It uses scale 10^12, halves the exponent until <= 1/8, sums 12 alternating Taylor terms, then squares back. At exponents >=20 it returns 999,999 ppm; the output never promises certainty. Tests compare 20,001 exponent values with floating-point `exp`, enforce monotonicity and <=2 ppm error on that grid, and cover zero/maximal inputs. This empirical numerical check is not a formal all-input error proof.

This is the probability of at least one counted event under a constant-rate Poisson assumption. It is not a maker-fill estimate: queue ahead, trade size, cancellations ahead and priority are not yet modelled. An event probability must not be passed to an execution decision as a fill probability.

## Queue imbalance and distance buckets

```text
QI_ppm = trunc_toward_zero((bid_quantity - ask_quantity) * 10^6
                          / (bid_quantity + ask_quantity))
```

Empty combined depth returns `None`; negative depths are invalid. Per-venue queries support best level and configured top N levels on each side. They scan at most N levels when explicitly requested, outside automatic per-event updates. The native quantity scale cancels in a same-venue ratio. Global bucket queries use weighted common-grid depths from Phase 4; empty or out-of-corridor buckets return `None`. Corridor inclusion alone is not a certificate of complete native-feed coverage; Phase 4's coverage assumption still applies.

Flow bucket attribution uses the **pre-event** fresh consolidated midpoint and the configured half-open price-distance boundaries. Depth changes use resting side; trade prints use the opposite side of the aggressor (the consumed side). Events inside the spread, on the wrong side of midpoint, beyond the outer edge, or without a valid midpoint have no bucket. Their source-wide totals remain intact. Bucket metrics describe where events occurred at their event-time midpoint; historical contributions do not migrate when the midpoint changes. Current bucket QI is a different, present-depth observation.

The weighted OFI summary is `sum(weight_ppm_v * OFI_time_v)`, with an active-source mask. It is in common quantity times ppm. It is neither consolidated-best-quote OFI nor a sum of comparable rates: source epoch starts/exposures may differ and must be inspected before comparison. Weights affect the global summary, not raw per-venue intensities.

## Configuration and executable

```sh
cargo run -p cli --release --locked -- flow-demo config/phase4-market.toml config/phase5-structures.toml config/phase5-flow.toml /tmp/lre-phase5-demo
cargo run -p cli --release --locked -- flow-replay /tmp/lre-phase5-demo
```

Use a fresh output directory. The demo retains market, structure and flow TOML configuration with checksum-protected binary records. It reuses the Phase 4 semantic fixture and verifies complete direct versus replay flow/research state. The Phase 5 structure configuration widens percentage buckets for the coarse synthetic price grid; it is not a venue calibration.

Output includes both OFI definitions, counts and quantity rates, cancellation availability, per-source epochs/exposure, best/top-N and bucket QI, event probability, bucket additions/removals/executions and weighted OFI source mask. Rates labelled `micro_per_second` must be divided by 1,000,000 for human event/quantity rates. Global OFI divides by the same scale to express weighted common quantity.

Native atomic-batch boundaries are still absent from v1 recordings. Do not record a multi-level native message as independently valid single events and claim equivalent research state. Live native adapters, batch-aware recording, queue attribution, inventory/risk/markout and execution remain later phases. Phase 6 will build the exact child-unit ledger and portfolio accounting before any order submission.
