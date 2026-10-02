# Phase 3 — deterministic multi-venue consolidation

## Implemented scope

Three independent synthetic venue books feed a single-owner `Consolidator<V,N,G>`. Native prices and quantities remain in each venue book; only contributions to the consolidated ladders are normalized. The CLI provides exact TOML startup configuration, three binary recordings, and deterministic replay through the same consolidator. Network feeds, venue-specific decoding, trading, and liquidity/void detection remain later milestones.

Run from the project directory:

```sh
cargo run -p cli --release --locked -- multi-demo config/phase3.toml /tmp/lre-phase3-demo
cargo run -p cli --release --locked -- multi-replay /tmp/lre-phase3-demo
cargo bench -p consolidator --bench consolidation --locked
cargo bench -p consolidator --bench profile --locked
```

Use a new directory for each demo. Its `config.toml` and three `venue-ID.lre` files form the replay input. The sample venue IDs reserve Binance/Bybit/OKX slots; scales and weights are synthetic, not current exchange specifications. The CLI currently requires three venues; the core uses const-generic venue/depth capacities. Configuration parsing/allocations happen before event processing. Serde and TOML are dependencies of the control CLI only; the core uses std and workspace crates.

## Exact normalization model

`UnitScale { atoms, decimals }` denotes `atoms / 10^decimals` physical units. Both atoms and scale denominators are positive; decimals are limited to 18.

- A price tick is a quantity of quote currency per base asset.
- A quantity unit is a quantity of base asset; for linear products this must already include the contract multiplier.
- `MarketIdentity` contains base, quote, settlement asset, contract kind, and explicit equivalence group. Every field must match the common grid. Spot and linear products cannot be combined. An equivalence group asserts matching expiry/payoff semantics; it is supplied by research configuration, not inferred from symbols.
- Inverse and quanto conversion, currency exchange and mark-dependent contract multipliers are unsupported.

For native tick/lot size s and common-grid size g, conversion is `native_integer × s/g`. The ratio is reduced once at initialization. Each conversion checks divisibility before multiplying, returning an error on fractional target units or overflow. No rounding is allowed. Midpoints retain half ticks as doubled i128 values.

`Normalizer::notional` computes `common_price × common_quantity`. All resulting Money values use one common lattice. `settlement_money` can convert that Money to integer quote settlement atoms at a chosen precision; quote and settlement assets must match, and conversion must be exact. Extreme scale products may be rejected conservatively on checked-intermediate overflow.

## Weighted incremental depth

For common-grid price p:

```text
stored_depth(p) = Σ venue_weight_ppm × normalized_quantity(p)
physical depth in common quantity units = stored_depth(p) / 1,000,000
```

Weights range from 0 to 1,000,000 per venue and need not sum to 1,000,000. Zero-weight books remain tracked and validated but do not contribute quotes or depth. `included` describes a fresh committed book; it can be true for a zero-weight venue.

Bids and asks use separate bounded sorted arrays of i128 weighted quantities. A normal Add/Modify/Cancel applies only the affected price delta. Per-side totals are maintained incrementally and queried in O(1). Trade events change no displayed depth. A snapshot becomes a contribution only on successful SnapshotEnd. SnapshotStart withdraws the previous contribution immediately. Removing a venue subtracts exactly its own contribution, retaining every other venue's levels at shared prices.

Capacity G must be at least V×N at startup so all disjoint venue levels can fit. Book storage is O(V×N), consolidated storage O(G). Ordinary modifications cost O(V + log N + log G): V is the explicit freshness scan. Inserts/deletes may shift bounded contiguous arrays. No heap resizing, shared locks, per-event scheduler or I/O occurs in this path. Snapshot activation/removal walks one venue's depth; it does not rebuild the global book.

The global BBO is the best positive-weight bid and ask across valid sources; its midpoint is their arithmetic midpoint. It is not an average of venue midpoints. Each source's own book must be uncrossed, but quotes across venues can lock/cross. `MarketState::{Open,Locked,Crossed,OneSided,Empty}` exposes these observations without treating them as executable arbitrage.

Venue divergence is retained in doubled ticks and millionths of a basis point:

```text
D2_v = local_midpoint_x2 - global_midpoint_x2
divergence_bps_raw = trunc_toward_zero(D2_v × 10,000 × 1,000,000 / global_midpoint_x2)
```

`depth_disagreement(side,price)` is an explicit research query over normalized **unweighted** depth, with missing displayed levels counted as zero. Zero-weight and excluded venues do not participate. It reports exact population variance `(n×Σq² - (Σq)²)/n²` and the integer floor of its square root. Checked overflow returns an error. This O(V×N) query is not run on every event; distance buckets and depth-coverage reasoning belong to the liquidity milestone. Missing displayed depth is not proof of a genuine liquidity void.

## Freshness and error behavior

All input receive timestamps share one monotonic clock domain and must be globally nondecreasing. `advance_time(now)` allows a future live owner to enforce expiry even when feeds are silent. Calling that function on a periodic tick is the owner's responsibility; there is no background scheduler in this delivery.

A book expires when `now - last_depth_timestamp > stale_after_ns`. The exact threshold remains valid. Trades do not refresh depth freshness. Stale/disconnected books are withdrawn and invalidated; resuming ordinary updates cannot reactivate them. They require a new snapshot. Snapshot completion establishes fresh depth.

Book validation failures withdraw only the affected venue and return a typed error. The CLI aborts on errors; a future session supervisor may handle explicit resynchronization. Normalization failures, global clock regression or aggregate invariants latch an engine fault and make aggregate query methods fail. Recovery from a global fault requires rebuilding/replaying an engine; no silent reset is offered. Unknown venue or wrong-instrument routing errors do not mutate an unrelated market.

`VenueBook::last_committed_levels` is a diagnostic/withdrawal view that can expose stale retained depth; it is never a trading-valid view. Normal `levels` and BBO queries continue to require Live state. Failed single-event operations preserve committed ladders, allowing exact withdrawal after the book invalidates.

## Atomic batches and native sequence contracts

`VenueBook::apply_depth_batch` accepts a bounded slice of canonical depth events for one native message. Canonical sequence increments remain contiguous. Receive timestamp and native update ID must agree across members. The scratch ladder is copied from active depth once, all updates are applied, and only the final BBO is checked for crossing. Successful commit swaps ladders; failure preserves the last committed ladders and invalidates the source. No observer can see an intermediate book.

Batches contain only Add/Modify/Cancel, at most 2×N members. Scratch capacity N must also hold at intermediate steps; an over-capacity batch is rejected even if its final depth would fit. The rare batch path has bounded O(N) scratch preparation. The consolidator withdraws/reinserts only the affected venue at the atomic boundary.

`NativeSequenceTracker` provides a generic contract: seed from a snapshot; require the next native update to be covered by a range; verify a prior-range pointer when supplied; reject gaps, duplicates, backward/invalid ranges and exhaustion. A failure requires reseeding. This does not claim to implement Binance, Bybit or OKX wire rules. Each actual adapter still needs protocol-specific validation, buffering and reconnect behavior.

**Recording limitation:** binary v1 has no atomic-message boundary markers. Its merged replay supports independently valid canonical events. Do not flatten a native atomic batch into v1 and claim equivalent replay. A versioned batch envelope and round-trip tests are required before native batch feeds are recorded. The synthetic demo uses independently valid events only.

## Deterministic merged replay

`MergedReplay<R,V>` owns one reader and one lookahead event per venue. It orders by `(local_receive_timestamp, venue_id, canonical_sequence)`. Source-file argument order has no effect; exchange timestamps are not used for ordering or lead/lag claims. A consumed reader is refilled on the next call, so corrupt future records cannot silently suppress already emitted events.

This defines a stable offline research order. Future threaded live ingress must record a total processing order or enforce an explicit ordering/watermark policy before claiming identical inter-thread replay. The current direct/replay equality proof covers the synchronous synthetic source.

The shared-clock-domain assertion is mandatory because v1 does not persist clock origins. The demo generates all streams in one synthetic clock and saves the assertion in its configuration. A caller cannot infer clock synchronization from similar timestamps; independent recording sessions must not be merged without synchronization metadata.

Native recording header scales and instrument IDs are checked against configuration before replay. An incomplete snapshot at EOF fails. Corrupt records poison the merged reader. Initialization uses a temporary Vec; merging and core application do not allocate. `run` uses maximum throughput; `next_event` is the step API. Multi-venue paced replay is not implemented yet; single-venue pacing remains available.

## Tests and performance

See [Phase 3 validation](phase3-validation.md) for test counts, measurements and raw output. The full-state demo assertion compares direct input with recorded/replayed input, including venue state, aggregate arrays, timestamps and totals.

Thread transport, SPSC queues and CPU pinning remain deferred until an actual threaded feed owner is introduced. This milestone measures synchronous consolidation rather than adding unused transport infrastructure. No trading hypothesis or profitability conclusion has been evaluated.
