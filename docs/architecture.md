# Architecture and contracts — foundation delivery

Historical Phase 1–2 design. See [Phase 3](phase3.md) for implemented normalization, consolidation, TOML configuration, atomic batch support and merged replay; those supersede the corresponding planned-only sections below.

## Scope and execution model

The current executable is synchronous: a synthetic source or binary replay supplies canonical events to the same `VenueBook::apply`. No async runtime, locks, threads, network clients, live credentials, or order gateway implementations are present. This makes the initial invariants independently testable before threading and venue protocols are introduced.

Planned live ownership, within one process:

```text
Binance socket → native validator/decoder → owned Binance book ─┐
Bybit socket  → native validator/decoder → owned Bybit book ────┼→ bounded SPSC snapshots/deltas
OKX socket    → native validator/decoder → owned OKX book ──────┘          ↓
                                                        owned consolidator
                                                                ↓
                                                     liquidity/flow/void state
                                                                ↓
                                                  inventory + candidate utility
                                                                ↓
                                                        hard risk gate
                                                                ↓
                                                     owned order gateway
```

Each ingress thread owns its feed decoder and book. The consolidator owns normalized aggregate state. Strategy, inventory, and risk may initially share the consolidator thread to minimize hops; split only after measuring stage/tail latency. One gateway thread owns order lifecycles. A separate recorder/research thread performs disk I/O. No `Arc<Mutex<VenueBook>>`.

SPSC transport and CPU pinning are planned, not implemented. Queue overflow must produce an explicit loss/kill event; never silently discard depth and continue trading. Optional CPU assignment belongs in validated startup configuration and must account for hardware and platform support. Monotonic elapsed nanoseconds use a process/session origin. Merging recordings from different sessions requires clock-origin metadata and synchronization evidence.

## Event contract

`MarketEvent` is a fixed-size `Copy` value with venue/instrument IDs, a contiguous canonical per-market sequence, native sequence, exchange timestamp, local receive timestamp, event kind, side, integer price and quantity. In-memory Rust layout is not an ABI or storage format. Serialization uses explicit little-endian offsets.

Canonical sequences include trades and snapshot markers. They are **not exchange sequences**. An adapter must validate the native sequence/update range first, then normalize native messages to canonical events. A single `exchange_sequence` is audit context and cannot replace a venue's complete native sequencing rules. Native depth-batch boundaries and range IDs require protocol-specific handling before live feeds are enabled.

- Add: price absent, absolute positive quantity.
- Modify: price present, new absolute positive quantity.
- Cancel: price present, quantity exactly zero.
- Trade: positive price/quantity, aggressor side; never modifies book depth. Applying the corresponding exchange depth update twice would double-count execution.
- SnapshotStart/SnapshotEnd: zero price and quantity; side is ignored.
- All accepted events have nondecreasing local receive timestamps. Equal timestamps are valid.
- Exchange timestamps are stored without ordering assumptions and never subtracted from the local monotonic clock.

`LifecycleTimes` separately reserves optional processed, decision, send, and acknowledgment timestamps. Their instrumentation is future work; they are not fabricated as feed receive timestamps. Native JSON/WebSocket decoding is not part of this milestone. The existing decoder benchmark measures the canonical binary record and its CRC.

## Arithmetic model

| Value | Representation | Units / invariant |
|---|---|---|
| PriceTicks | i64 newtype | Signed arithmetic primitive; book prices strictly positive |
| QtyUnits | i64 newtype | Signed primitive; resting level quantities strictly positive |
| InventoryUnits | i64 newtype | Signed count of exact fractional parent quanta |
| Money | i128 newtype | Price ticks × quantity units, for a single instrument scale |
| BasisPoints | i64 newtype | One basis point = 1,000,000 stored units |
| Timestamp | u64 newtype | Local monotonic nanoseconds, session-relative |

`notional(p,q) = i128(p) × i128(q)`. A product of two i64 values fits i128. Long PnL is `(exit-entry)×quantity`; short PnL is its negation. Subtraction widens operands before computing the difference. Explicit checked addition/subtraction fails on overflow. Release builds also enable overflow checks.

For tick size `tick_atoms / 10^price_decimals` and lot size `quantity_atoms / 10^quantity_decimals`, physical quote value is:

```text
Money × tick_atoms × quantity_atoms / 10^(price_decimals + quantity_decimals)
```

The prototype stores this exact lattice amount, not rounded currency cents. Cross-market/contract aggregation is prohibited until an explicit settlement and contract-equivalence layer is implemented. Newtypes are dimension labels; they do not yet carry a runtime instrument tag. Callers must not add Money from incompatible scales.

Inventory quantum is an exact rational: `parent × numerator / denominator`, default policy later will be `1/10`. A quantity that cannot divide exactly is rejected. There is no silent fractional-lot rounding. `Quantum::quantity` checks signed multiplication overflow. No inventory ledger is implied by these primitives.

`BasisPoints::from_ratio(n,d)` computes `n×10,000×1,000,000/d`, requires `d>0`, rounds toward zero, and rejects an out-of-range output. Decimal price parsing occurs at ingress/control boundaries, rejects excessive precision and off-tick values, and never converts through a floating-point number.

Midpoint is exposed as `midpoint_x2 = bid+ask` in i128 so half ticks remain exact. Midpoint/spread are absent for one-sided or empty books. Such books can be valid L2 state but cannot qualify as a two-sided trading environment.

## Book representation and state machine

`VenueBook<N>` owns four arrays of `[Level; N]`: active bids/asks and staging bids/asks. Each level contains an i64 price and i64 quantity (16 bytes). Level storage is `64×N` bytes (16 KiB at N=256), plus lengths, IDs, sequence, clocks, and status. There are no heap allocations, pointers to individually allocated levels, or locks in book updates. Construct large books once, optionally in preallocated heap storage, before entering the live loop.

Bids sort strictly descending, asks strictly ascending; prices are unique per side. Binary search takes O(log N). Quantity replacement is O(log N). Insertion/deletion additionally shifts at most N levels in contiguous memory, O(N). Queries for best price and spread are O(1); top-N depth is an explicit O(N) query and is not recomputed on each update. A later incremental statistics module can maintain selected aggregates.

```text
AwaitingSnapshot → SnapshotStart → BuildingSnapshot → SnapshotEnd → Live
        ↑                               │                           │
        └──── new book                  └── invalid event ──────────┴→ Invalid
                                                                        │
                                                     fresh SnapshotStart┘
```

SnapshotStart clears only staging and immediately makes BBO/depth queries unavailable. Snapshot levels must be Adds; a matching SnapshotEnd validates uncrossed state and swaps the complete ladders. A new snapshot may restart an interrupted snapshot. SnapshotStart may jump sequence forward to recover after dropped events, but must be nonzero and newer than the last accepted event. All other events require exactly last accepted sequence + 1, without wrapping. A new session/book is required after sequence exhaustion or local monotonic-clock reset.

A crossed or locked book (`best_bid >= best_ask`), duplicate Add, missing Modify/Cancel, invalid quantity/price, capacity exhaustion, sequence gap, or clock regression invalidates the book. Query methods then fail until a snapshot succeeds. A wrong-market routing error does not mutate the unrelated book; the caller must treat routing errors as fatal at ingress. No updates are silently skipped. Full capacity is a failure, not an implicit truncation of depth. Upstream finite-depth feeds need explicit coverage/truncation semantics before they can be used for void research.

**Atomic venue batches:** the current canonical contract assumes each incremental event yields a valid uncrossed book. Some native messages atomically update several levels and may look crossed if split in the wrong order. Native adapters must preserve atomic semantics; a validated batch API/protocol normalization milestone is required before enabling live feeds. The prototype is not a general-purpose decoder for raw exchange updates.

## Recording/replay

The `recorder` crate implements a versioned, checksummed fixed-width binary format. Each file is one venue/instrument/session with scale metadata. Native network payloads and cross-venue clock alignment are not recorded yet. Use `BufWriter`/`BufReader` only in the recorder/replay I/O layer. Disk writes never belong in a future live book/decision thread.

`Replay::step` is the step-by-event API; `run` repeatedly calls it. The exact `VenueBook::apply` used by a direct source applies all replay events. Pacing is 1x/2x/10x/100x or maximum speed and does not change event timestamps or calculations. A paced step applies and validates the event, then waits until the recorded receive offset; callers observe the step only on return. Sleep is replay tooling, not a live decision scheduler.

Invalid data, checksum failure, sequence gaps, clock regressions, truncated frames, and EOF during an unfinished snapshot return errors. The executable stops on the first error. Tests compare every field of direct/replayed book state after every event and replay the same stream twice. A complete-frame truncation of a live recording cannot be detected by per-record CRC alone; a durable footer/checkpoint is a future extension.

## Module boundaries and status

| Crates | Current role |
|---|---|
| common, fixed-point | Implemented identity, time, side, exact arithmetic |
| market-events | Implemented canonical events, lifecycle timestamp/decoder contracts |
| book | Implemented one generic bounded L2 book |
| recorder, replay, cli | Implemented recording, shared-module replay, executable demo |
| venue-binance, venue-bybit, venue-okx | Reserved adapter boundaries; no connection or protocol logic |
| consolidator | Reserved venue weight and aggregate-level types |
| liquidity, voids | Reserved regions and lifecycle/penetration states |
| orderflow, toxicity | Reserved observable component types |
| inventory, quotes | Reserved roles, target and action types |
| risk, execution | Reserved kill reasons and gateway interface; no active trading |
| metrics, simulation | Reserved stage/scenario catalogs |

The only implemented benchmark allocator instrumentation uses an audited pass-through `System` allocator in a separate benchmark executable. Runtime libraries forbid unsafe code. The book package permits a benchmark-only override of a deny lint; its library independently forbids unsafe code.

## Next mathematical contracts

Later score terms must be individually observable and configuration-driven. Before implementing Phase 4, define rolling baselines, persistence clocks, refill/expiry policy, and ownership of overlapping zones. A void becomes eligible for revisit analysis after formation persistence and exit; touch or partial penetration is sufficient. Full traversal is never a prerequisite.

Before Phase 6, specify signed ledger conservation, exact fee/rebate scales, liability `Σmax(0,-UPnL)`, harvest account, and episode accounting. Normal actions must create expected portfolio benefit or reduce inventory deviation; hard safety exits may realize losses. The future core success metrics are recovery yield, recovery time, and maximum inventory excursion, not trade win rate.

Phase 4 adds fixed-grid liquidity research and bounded historical voids. See [Phase 4 ownership, models and lifecycle](phase4.md) for the current research event path.

Phase 5 wraps the same research engine with per-venue flow windows. See [flow ownership, exact windows and attribution](phase5.md).
