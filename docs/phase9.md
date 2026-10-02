# Phase 9: simulated execution — queue model, fees, markouts, funding and the objective

## Scope

Phase 9 replaces the single Phase 7 fill rule with a configurable execution simulator and measures it on recorded live sessions. All fills remain **simulated**: no account or key is used and no order is ever sent.

| Crate | Content |
|---|---|
| `execution` | `fees` (exact ppm schedule), `queue` (L2 queue position with three cancellation models), `markout` (§18 horizons with a spread/drift split). The `OrderGateway` trait remains an interface only. |
| `engine` | `FillModel`, fee schedule, funding, markouts, the J(a) entry gate, the stale-touch rule, maker-economics metrics and net maker yield. |
| `cli` | Optional TOML sections, execution lines in every engine summary, and `fill-study`. |
| `config/phase9-engine.toml` | The live engine with the queue model, retail fees, funding, §18 horizons and J(a). |

Every new section is optional. Without them an engine behaves exactly as in Phase 8: the strict rule, no proportional fees, no funding, no objective, with markouts measured at the §18 horizons. Phase 8 recordings replay to the identical engine state and journal.

## Fill models

`FillConfig::model` selects one of two rules. In both, a resting order fills **only on a print on its own venue, at the order price, as maker**.

**Strict trade-through** is the Phase 7 rule: a print strictly beyond the resting price fills it, and print size and queue position are ignored.

**Queue** (`execution::queue::QueuedOrder`). The simulated order is not in the displayed book; displayed sizes are everyone else's.

- Placement joins the **back** of the level: `ahead` is the displayed size at that price.
- A print at our price consumes `ahead` first. Any remainder fills us, and anything beyond our size went to orders behind us. All of it except our own fill is remembered as `pending_execution`.
- A later decrease of the displayed size is matched against `pending_execution` first: that is the depth echo of those prints. Only the unexplained remainder is cancellation, attributed by the cancel model.
- A print strictly **through** our price fills the remaining size, because by price priority the whole level traded first.
- `ahead` never exceeds the displayed size, and a vanished level leaves nobody ahead.

| Cancel model | Unexplained decrease at our level | Role |
|---|---|---|
| `Pessimistic` | All behind us: `ahead` shrinks only through prints | Lower bound on fills |
| `Proportional` | Spread evenly: `ahead` shrinks by `ahead × cancels / level`, rounded down | Base case |
| `Optimistic` | All ahead of us | Upper bound on fills |

The engine captures the displayed size at every queued order's price on a venue immediately before and after each depth event or atomic batch, and feeds the difference to the model. Prints go to `on_trade`.

### Assumptions and known biases

- **Children fill completely or not at all.** A ledger child is one quantum. Queue progress short of a complete fill is tracked but not booked. If the order is then cancelled, the simulated partial quantity is discarded and counted (`abandoned_partial_qty`). This overstates the cost of cancelling and understates exposure; both counters are reported.
- **Venue price scales must equal the grid's.** Queue and objective prices are grid prices looked up in native books, so `Engine::new` rejects the queue model or an objective when any venue's price map is not the identity.
- **Unobservable queues.** An order placed while its book is not live starts behind everything, and the first observable depth clamps it to the displayed size. A level first observed after the book was unobservable only clamps; it is never treated as a cancellation.
- **Cross-stream ordering.** If a venue publishes the depth decrease before the print, the decrease is first treated as cancellation and the print then consumes `ahead` again. The pessimistic model is immune to that double count.
- **Marketable harvest closes.** A harvest close placed at entry ± `harvest_ticks` can already be through the market. It rests and fills on the next print through it, at its own price, booked as maker. Post-only exchanges would reject it instead. This is conservative on price but books a maker fee where a re-quote would have been needed.
- **Engine latency is zero.** Engine orders are live the moment they are decided. The fill study measures latency separately; engine order lifecycles belong to Phase 10.

## Stale-touch rule

Depth arrives in snapshots or deltas every 20 to 100 ms per venue, while prints stream continuously. Between depth updates, a print **through** the displayed touch proves that price is gone.

The engine therefore tracks, per venue since its last depth change, the lowest sell-aggressor and highest buy-aggressor print. No entry or rebalance quote is **placed or moved** at a price traded through. Any resting order on that side and level stays where it is, and the skip is counted (`stale_touch_skips`). The next depth event or batch on that venue clears the condition. Harvest closes, at fixed take-profit prices, are not affected.

The first live fill study ran without this rule and found its necessity: during a sweep, a filled probe was refilled at the stale bid on every print, about 2,100 times a minute per venue. Synthetic scenarios never print through a touch before the next depth event, so the rule changes no Phase 7 result. An injection test exercises it.

## Fees

`FeeSchedule` holds maker fee, maker rebate and taker fee in **ppm of fill notional**. Fixed atoms per fill from Phase 7 are added on top.

- Fees round **up** and rebates round **down**, so rounding never flatters results.
- Charges are computed at the actual fill or reservation price for one quantum.
- A normal close's `ReserveClose` assumes exactly the charges its maker fill will book, so the central rule sees the true cost.
- Emergency exits pay the taker schedule.
- Each value is limited to 100,000 ppm (10%).

The ledger keeps only total fees and rebates. The engine also splits them by role: `maker_fees`, `taker_fees`, `rebates`, `maker_notional` and `taker_notional`.

The live base case is retail: makers **pay** 200 ppm (0.02%) and takers 500 ppm (0.05%; Bybit charges 0.055%), with no rebate unless a programme grants one (see [providers](providers.md)). At the recorded prices of about 86,600 USDT per BTC, the maker fee alone is about **17.3 USDT per BTC, or 173 ticks of 0.1 USDT**, per fill.

## Funding

`FundingConfig { rate_ppm, interval_ns }` charges a constant rate to every child held across each boundary. Boundaries are the multiples of `interval_ns` on the **engine clock**, and zero disables funding.

- The notional is taken at the venue's last mark midpoint, or the entry price if the venue is unmarked.
- Positive rates make longs pay. Payments round up and receipts round toward zero.
- Crossing several boundaries in one step charges all of them in one `Funding` command per child.

Live receive times are session-relative, so a session's funding boundaries have an **arbitrary phase** against the exchanges' UTC schedule. Real funding rates vary every interval, and they are not recorded.

## Markouts (§18)

Every simulated maker fill is recorded with its side, price, quantity and the **reference midpoint at the fill**. The reference is the configured one: composite on live data.

The markout at horizon h is `mid(t + h) − p` for a buy and `p − mid(t + h)` for a sell. It is sampled at the **first event at or after** `t + h` (event-time sampling). Because the midpoint at the fill is known, each markout splits exactly into:

- the **spread captured** at the fill: `mid(t) − p` for a buy
- the side-signed **drift** `mid(t + h) − mid(t)`

Negative drift is adverse selection. The markout itself is the realized spread.

| Rule | Value |
|---|---|
| Horizons | Configurable, 1 to 10, strictly increasing. The default is the §18 set: 1, 5, 10, 25, 50, 100, 250, 500 ms, 1 s and 5 s. |
| Missing midpoint | A horizon whose sample has no midpoint counts as `missing`. |
| Pending fills | A fixed ring of 1,024 (engine) or 4,096 (study). Overflow drops the oldest pending fill and is counted. |
| Cost | One cursor per horizon: fills are recorded in time order, so each horizon's due fills are a prefix. An observation costs O(horizons + samples due), about 8 ns in the benchmark. |
| Aggregates | Exact integers in doubled ticks: quantity-weighted sums, the sum of squares per fill, the adverse count, and the drift sums. |

## Objective J(a) (§24)

An optional `ObjectiveConfig` values one entry quantum in **doubled money atoms**, with integer weights in permille:

```text
J = wR·R + wS·S + wB·B − wA·A − wI·I − wQ·Q
```

| Term | Definition |
|---|---|
| R, rebate | Maker rebate minus maker fee at the candidate price. Negative under retail fees. |
| S, spread capture | `mid − 2p` (buy) or `2p − mid` (sell) against the reference midpoint, times the quantum. |
| B, rebalance benefit | If the fill moves net toward target: the avoided aggressive exit, which is the taker fee plus half the venue spread. |
| A, adverse selection | Mean adverse drift per unit at `adverse_horizon`, rounded up and never negative, once `adverse_min_samples` engine fills exist; otherwise `adverse_prior_x2`. |
| I, inventory risk | If the fill moves net away from target: `inventory_risk_x2` × \|net − target\| after the fill × quantum. |
| Q, queue cost | `queue_cost_x2` × the displayed quantity ahead: better prices plus our level. A resting order uses its **tracked** position; a new one joins behind the displayed size. |

The action set per entry slot is hold, place, cancel and replace:

- With no order resting, place only if `J ≥ min_edge`.
- With an order resting, take the arg-max of:
  - keep, valued at its own queue position
  - cancel, valued at `min_edge`
  - replace at the rule-based price, if it moved by at least `reprice_ticks`
- **Ties keep the current state.** Since moving forfeits queue position through Q, this is §26's rule that an order moves only when the benefit exceeds the queue loss.
- Every action remains subject to the ledger's hard limits.
- Without a reference midpoint or a usable touch, J is undefined: the slot holds or cancels, and the event is counted.

Only entries are gated; closes follow the central inventory rule. All arithmetic is checked `i128`.

## Maker economics and net maker yield (§25)

The ledger already counts:

- maker fills
- completed cycles
- maker→maker cycles
- maker→taker cycles
- taker escapes

In this engine every entry is passive, and the only taker exit is the emergency path, so maker→taker cycles and taker escapes coincide. The maker completion ratio is maker→maker cycles over completed cycles.

**Net maker yield** in ppm is:

```text
(realized + rebates − fees − slippage − funding + open marked P&L) / maker notional
```

The §25 numerator terms map onto ledger accounts as follows:

| §25 term | Where it is booked |
|---|---|
| Realized spread, adverse selection, inventory loss | Inside realized and marked P&L |
| Taker costs | Inside fees |
| Slippage | Its own account |

The markout split reports realized spread and adverse drift separately per horizon, for diagnosis.

## Fill study

```text
lre fill-study DIRECTORY REQUOTE_TICKS MAKER_FEE_PPM LATENCY_MS
```

The command replays a recorded session through the recorded engine configuration. Each venue carries **shadow maker probes**: one child per side for each of the four fill models (strict and the three cancel models). Probes never reach the ledger or the policy. They answer a narrower question than the engine: how often a passive order at the touch fills, how long it waits, where it sits in the queue, and what happens to the price afterwards.

Rules, identical for every model:

- A side without a probe, or whose probe is `REQUOTE_TICKS` or more **behind** the touch, decides to place one child at the touch. A touch that moved **through** a probe leaves it resting, because it is then the best price.
- A decision takes effect `LATENCY_MS` later, at the first frame at or after that time. Until then the old probe stays live and can still be filled (counted as `while_replacing`).
- On arrival the order is **post-only**. It is rejected if it would cross the touch, or if a print has since traded through its price; otherwise it joins behind the size displayed at that moment.
- The stale-touch rule applies to decisions.
- An unusable venue touch withdraws that venue's probes at once.

Per venue and model, the report gives:

- placements, fills (at price, through, buys) and fills per minute
- fill ratio (fills over placements), re-quotes, rejects and stale blocks
- partial prints and abandoned partial quantity
- wait to fill at p50 and p90
- mean queue ahead at entry, in base units
- the maker fee per unit in ticks
- markouts at every horizon: samples, mean, drift and adverse fraction

## Configuration

`[fills]` keeps the Phase 7 keys and adds:

- `rule`: `strict_trade_through` or `queue`
- `cancel_model`: `pessimistic`, `proportional` or `optimistic`, required with `queue` and refused without it
- `maker_fee_ppm`, `maker_rebate_ppm` and `taker_fee_ppm`, each defaulting to 0

The new optional sections are `[funding]` (`rate_ppm`, `interval_ns`), `[markout]` (`horizons_ns`) and `[objective]` (the six weights, `adverse_horizon`, `adverse_min_samples`, `adverse_prior_x2`, `inventory_risk_x2`, `queue_cost_x2`, and `min_edge_x2` as a signed string). Unknown keys are refused.

`config/phase9-engine.toml` is the Phase 8 live engine with:

- the proportional queue model
- retail fees
- 100 ppm funding per 8 h
- the §18 horizons
- J(a) with all weights at 1,000 and an adverse prior of 69 ticks per unit

The prior is the 1 s proportional-model drift on the Binance touch in the 300 s session at zero latency. Inventory risk and queue cost are **not calibrated**.

From flat inventory this configuration **holds** at retail fees. R alone is −3,472 doubled atoms per child (a 1,736-atom maker fee), against spread capture of tens to a few hundred, and any entry from flat adds inventory risk. Only an entry that moves net toward target earns the rebalance benefit (twice the 4,340-atom taker fee plus half the spread).

## Live-path fix: staleness at the frame's own timestamp

The 600 s Phase 9 session faulted the engine at 550.05 s, two seconds into a network stall that silenced all three venues. Phase 8's live loop checked the venue book's CURRENT state before applying a frame. `apply` first advances the consolidator clock to the frame's receive time, and that advance withdraws any venue silent for more than `stale_after_ns`. The first frame after a long enough silence therefore passed the check, withdrew its own book, and reached it as `SnapshotRequired`.

Phase 8's sessions never had all venues stall together, so another venue's event always advanced the clock first.

- `Consolidator::live_at(venue, now)` answers whether a book is live and stays live once the clock reaches `now`, mirroring the withdrawal rule exactly.
- `Engine::accepts(venue, receive_ts, snapshot)` uses it. A snapshot is always accepted, and so is input after a research fault, which the engine ignores anyway.
- The live loop drops refused input and resynchronizes the venue, as before.

A regression test reproduces the exact live error without the check.

## Limits

- **Simulation only.** Simulated queues are inferred from public L2 data at 20 levels. Hidden and iceberg liquidity, self-trade effects and our own market impact are absent.
- **Partial fills are not booked.** Children are atomic.
- **Engine latency is zero.** Only the fill study models order latency, and as one constant.
- **Funding uses a constant rate** on an arbitrary session phase.
- **J(a) is partly uncalibrated.** Its weights are configuration; only the adverse prior comes from data, and the queue cost is not modelled in the live file.
- **The evidence is thin.** The fill study covers the recorded sessions only: one asset, three venues, minutes of data.
