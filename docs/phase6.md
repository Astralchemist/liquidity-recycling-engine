# Phase 6: exact inventory ledger, accounts and episodes

## Model and scope

This milestone is accounting and hard risk state for fractional inventory. It does not choose actions, place orders, simulate queues or fills, or measure markouts. Fixtures stipulate fills; they are not market simulations. All prices, quantities and money are integers. There is no floating point in the ledger, journal or configuration path.

`Ledger<V, L, E>` is one synchronous, single-owner state machine. V is the venue count (at most 64), L the child slot count (at most 256), and E the completed-episode history capacity (at most 128). Every array is fixed at construction. The ledger performs no heap allocation, I/O, locking or scheduling. It derives `Copy` and `Eq`, so tests can compare complete states.

## Units

- **Quantum.** `Quantum::new(parent, numerator, denominator)` must divide the parent quantity exactly. Every child unit is exactly one quantum, for example `0.1 x 100 = 10` quantity units.
- **Inventory.** Inventory is counted in whole children: net `q = long - short`, gross `long + short`.
- **Money.** Money is `i128` atoms of common-grid price ticks x quantity units for one instrument. All venues share the Phase 3 common grid and an explicitly equivalent contract. Settlement-currency conversion between venues is not modelled.

A partial fill smaller than one quantum cannot be represented. Adapters must split parent orders into quantum children and must not report fractions of a child.

## Commands

The caller drives the ledger with `InventoryEvent { sequence, timestamp, kind }`.

| Command | Effect |
|---|---|
| `Mark{venue,bid,ask}` | Requires `0 < bid <= ask`. Revalues that venue's children at liquidation prices: longs at bid, shorts at ask. |
| `ReserveOpen{venue,side,role,evidence}` | Reserves capacity for one child order and returns `Reserved(id)`. Denied while halted. |
| `FillOpen{id,price,maker,charges}` | Converts the reservation into a child. Moves cash by the notional and books charges. Starts an episode if none is active. |
| `CancelOpen{id}` | Releases a reservation. |
| `ReserveClose{id,expected_price,expected_charges,mode}` | Marks a child `ExitPending` after the approval rules below. |
| `FillClose{id,price,maker,charges}` | Books realized PnL at the **confirmed** price, which may differ from the expected price. Moves the child into closed history. |
| `CancelClose{id}` | Withdraws a pending close and restores the child's previous role. |
| `Funding{id,cost}` | Signed: a positive cost is a debit, a negative cost a credit. Attributed to the child and the funding account. |
| `Halt{reason}` | Latches a kill reason. |
| `Tick` | Advances time so the risk checks below run during idle periods. |

`ReserveOpen` additionally requires all of the following:

- a role other than `ExitPending`
- qualified revisit evidence with a nonzero void id and `revisited_at <= now <= valid_until`
- a venue mark no older than `mark_stale_ns`
- risk approval
- free slot capacity

In this phase `RevisitEvidence` is an attestation supplied by the caller. Phase 7 connects it to the Phase 4 void engine and to environment qualification. The ledger does not verify that the void exists.

Roles `Harvest`, `Rebalance` and `Residual` are caller labels recorded on each child. They do not change approval: the central rule applies to every child.

Fills after a halt are still accepted. An exchange-acknowledged fill is a fact that must be accounted for, never refused.

## Transactions and ordering

Sequence numbers must be exactly `last + 1`. A gap latches `SequenceGap` and is rejected. A timestamp earlier than `now` latches `ClockAnomaly` and is rejected. Otherwise the ledger:

1. copies the transactional state as a rollback image
2. sets `now`
3. runs the risk assessment
4. processes the command
5. runs the risk assessment again
6. updates the active episode
7. consumes the sequence number
8. commits at most one closed child and one completed episode to history

On any error the state is restored from the rollback image. A halt newly observed by the assessment is still latched. An arithmetic error also latches `UnhandledState`. A rejected command does not consume its sequence number, and the journal records accepted commands only.

Unknown ids, duplicate fills, a fill for a child with no pending close, and a duplicate close are rejected without a halt. The Phase 10 gateway must escalate an unrecognized exchange fill as `UnhandledState`. The ledger cannot distinguish a gateway bug from caller misuse.

History rings (closed children and completed episodes) are excluded from the rollback image. They are written only after the transaction succeeds, and their eviction counters are checked before any slot is written. The rollback image is 7,664 bytes at `<3, 32, 8>` and 48,880 bytes at `<3, 256, 128>`.

## Hard limits

Approval reserves for the worst order in which pending orders could fill. Pending closes count as possible buys or sells and keep the child in gross until it fills, so they never release capacity.

| Check | Open approval | Close approval |
|---|---|---|
| Net extremes `net + pending/closing buys + this`, `net - pending/closing sells - this` | within `±max_net_units` | within `±max_net_units` |
| Same extremes vs target | within `max_target_deviation` | within `max_target_deviation` |
| `gross + pending opens + 1` | `<= max_gross_units` | — |
| Venue `gross + pending` | `< max_venue_units` | — |
| Open orders | `< max_open_orders` | `< max_open_orders` |

The assessment runs before and after every command. The first condition that holds latches its reason:

1. `InventoryBreach`: `|net| > max_net_units`, `gross > max_gross_units` or any venue gross above `max_venue_units`. Approvals make this unreachable; it is a defensive check.
2. `PnlBreach`: liability at or above `max_liability`, or drawdown from the equity high-water mark at or above `max_drawdown`.
3. `PositionAge`: any child age at or above `max_position_age_ns`.
4. `EpisodeDuration`: active episode age at or above `max_episode_ns`.
5. `StaleData`: a venue with children or reservations whose mark is missing or older than `mark_stale_ns`.
6. `AdverseEpisodes`: consecutive completed episodes with negative net PnL reach `max_adverse_episodes`. A non-negative episode resets the count.

Money thresholds halt at equality; unit limits halt only when exceeded. The first reason latches for the life of the ledger, and later halts do not overwrite it. There is no automatic resume. Resuming requires an explicit new ledger, and this phase provides no resume procedure.

Drawdown is measured since the ledger started. A daily or session drawdown needs a session boundary that does not exist yet. Order-rate and cancel-rate limits belong to the Phase 10 gateway.

## Central inventory rule

A `Normal` close is approved only if the child contributes profit or improves the portfolio's inventory balance. For child i with side sign `s` (+1 long, −1 short):

```text
expected_net = realized(entry_i, expected_price) + entry_charges_i.net
             + expected_charges.net - funding_i
q            = net + closing_buys - closing_sells        (committed closes projected)
approve if expected_net > 0  or  |q - s - target| < |q - target|
otherwise Denied(NoBenefit)
```

`Charges.net = rebate - fee - slippage`, and each charge component must be non-negative. Projecting committed closes prevents two losing closes from claiming the same rebalance. That flaw was found and fixed during Phase 6 validation; a regression test covers it.

The rule never encodes "never close a loser". Once halted, `Normal` closes are denied and `Emergency` closes bypass the rule. Emergency closes are allowed only while halted and still pass the net, target-deviation and open-order checks. Halts cover these cases:

- inventory, PnL, age or duration breaches
- a caller-supplied `StructureInvalidated`, `Disconnect` or other reason

Realizing a loss is therefore always reachable through hard risk. Closing the majority side always moves net toward zero, so the net check cannot block every exit. An exit can wait for order slots, which cancelling pending opens releases.

## Accounts

```text
Realized   R = sum over closed children of s * (exit - entry) * quantum
Harvest    H = R + rebates - fees - slippage - funding
Unrealized U = sum over open children of s * (liquidation mark - entry) * quantum
Liability  D = sum over open children of max(0, -upnl_i)
Recovery     = H - D
Equity       = H + U
Cash         = signed fill notionals + charges.net - funding
```

Slippage is a separate cash debit only. Do not repeat slippage that is already embedded in a fill price. The cash identity `Equity = Cash + signed market value at the mark` is verified after every generated command.

Each open child records:

- venue, side and entry price
- quantity (one quantum)
- entry time, so its age is `now - entered_at`
- entry charges, including the rebate earned
- funding, unrealized PnL and role
- any pending close mode

Each closed child additionally records:

- exit price, time and charges
- maker flag
- realized PnL
- net PnL after all charges and funding
- close mode

## Episodes

An episode starts with the first `FillOpen` while none is active, recording that reservation's void id. Later acquisitions join the active episode regardless of their void id; only one episode is active at a time.

- **Normal end:** `|net - target| <= target_tolerance`, `gross <= completion_max_gross` and no open orders.
- **Forced end:** while halted, it ends only at zero gross and no open orders. The episode records the halt reason.

Episode fields:

- start, end and duration
- peak long, short, gross and absolute net, plus peak deviation from target and peak liability
- account deltas: realized, rebates, fees, slippage and funding
- residual mark-to-market (unrealized PnL at the end)
- `net_pnl = delta H + U_end - U_start`
- first recovery time: the first command after which `delta H - D >= 0`, having been negative during the episode
- `inventory_recovery_yield_ppm = trunc(delta H * 10^6 / peak_liability)`, which is `None` without liability
- execution counts: orders, fills, maker fills, completed cycles, maker→maker, maker→taker and taker escapes

Orders outstanding when an episode starts are attributed to it.

Completed totals plus the active episode plus `unassigned_pnl` equals equity. With target 0 and `completion_max_gross = 0`, every PnL atom belongs to exactly one episode, so unassigned PnL is zero. The generated test asserts this after every command. A nonzero target or a residual completion gross deliberately leaves children between episodes. Their MTM and funding accrue to unassigned PnL, which is reported, not hidden.

## Maker economics

| Count | Definition |
|---|---|
| maker fills | fills flagged maker by the caller |
| completed cycle | one child opened and closed |
| maker→maker | maker entry and maker exit; numerator of `MakerCompletionRatio` |
| maker→taker | maker entry, taker exit |
| taker escape | emergency close filled as taker |

The CLI reports these counts as ppm ratios. Net maker yield needs adverse-selection markouts and maker notional, so it waits for Phase 9.

## Inventory journal v1

The journal is separate from the market recording format and uses the same CRC32/IEEE, now table-driven (output unchanged). The 24-byte header is:

- magic `LRINV\0\0\0`
- version `1`
- record size `160`
- CRC32 of the exact TOML configuration text
- 4 reserved zero bytes
- CRC of bytes 0–19

The reader rejects any mismatch, including a different configuration text.

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 8 | Sequence |
| 8 | 8 | Timestamp, monotonic ns |
| 16 | 1 | 0 Mark, 1 ReserveOpen, 2 FillOpen, 3 CancelOpen, 4 ReserveClose, 5 FillClose, 6 CancelClose, 7 Funding, 8 Halt, 9 Tick |
| 17 | 7 | Reserved, zero |
| 24 | 8 | Child id |
| 32 | 2 | Venue |
| 34 | 1 | Side: 0 buy, 1 sell |
| 35 | 1 | Role: 0 harvest, 1 rebalance, 2 residual, 3 exit-pending |
| 36 | 1 | Evidence qualified |
| 37 | 1 | Close mode: 1 emergency |
| 38 | 1 | Maker |
| 39 | 1 | Kill reason code 0–13, in `risk::KillReason` declaration order |
| 40 | 8 | Price, bid, or expected price |
| 48 | 8 | Ask |
| 56 | 16 | Rebate |
| 72 | 16 | Fee |
| 88 | 16 | Slippage |
| 104 | 16 | Funding cost |
| 120 | 8 | Void id |
| 128 | 8 | Revisited at |
| 136 | 8 | Valid until |
| 144 | 12 | Reserved, zero |
| 156 | 4 | CRC of bytes 0–155 |

Decoding re-encodes the record body and requires byte equality, so unused fields and reserved bytes must be zero. A clean EOF is allowed only between records, and a truncated record is an error. As with market format v1, loss of a complete final suffix is undetectable without a future footer.

## Configuration and executable

```sh
cargo run -p cli --release --locked -- inventory-demo config/phase6-inventory.toml /tmp/lre-phase6-recovery recovery
cargo run -p cli --release --locked -- inventory-replay /tmp/lre-phase6-recovery
cargo run -p cli --release --locked -- inventory-demo config/phase6-inventory.toml /tmp/lre-phase6-forced forced
```

Use a fresh directory; the demo refuses to overwrite.

- **Configuration.** `[inventory]` sets venues, parent quantity, quantum fraction, target, tolerance and completion gross. `[risk]` sets every hard limit. Unknown fields are rejected. Money limits are positive integer atom strings, so a decimal such as `"0.5"` is rejected. `Ledger::new` validates every combination, including duplicate venues, inexact quanta, a target outside the limits, and capacities beyond L.
- **Demo output.** The demo saves the TOML and the journal and prints Recovery after every command, as required by §21 of the specification. It then verifies complete direct-versus-replay state equality and prints portfolio and per-episode metrics.
- **Scenarios.** `recovery` recycles a losing child through a profitable rebalance child. `forced` takes a liability breach, then an emergency taker exit with funding and fees.

## Performance characteristics

The per-command cost is dominated by three things:

- the rollback-image copy
- O(L) scans: id lookup, mark revaluation of the venue's children, and the position-age check in both assessments
- risk arithmetic

History is never copied. Measurements are in [Phase 6 validation](phase6-validation.md). If the ledger joins a live decision path with large L, the next step is a slot-level undo log with an id→slot index, which removes the O(L) copy. At the current capacities it is not needed.

## Deferred

The following are not part of Phase 6:

- **Phase 7:** an executor that carries out the kill procedure (cancel resting orders, then emergency-flatten), driven by `safety_work_remaining`.
- **Phase 7:** connecting revisit evidence to the void engine.
- **Phase 7 and 9:** the action objective J(a), quote hysteresis and queue position.
- **Phase 9:** markouts and net maker yield.
- **Phase 10:** order and cancel rate limits.
- **No phase scheduled yet:** journal checkpoints and snapshots, session drawdown, and multi-instrument settlement.

No order submission is enabled.
