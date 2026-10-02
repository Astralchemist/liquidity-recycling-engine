# Binary format v1

All integers are little-endian. CRC is CRC32/IEEE (reflected polynomial `0xedb88320`, initial/final XOR all ones). The standard `123456789` vector is tested. Since Phase 6 the implementation is table-driven; a test proves it equals the bit-at-a-time definition, so existing files are unaffected. The separate inventory command journal (`LRINV`, 160-byte records) is specified in [Phase 6 contracts](phase6.md#inventory-journal-v1). CRC detects accidental corruption; it is not an authenticity mechanism. No Rust struct memory is written directly.

## Header — 48 bytes

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 8 | Magic `LREVENT\0` |
| 8 | 2 | Schema version = 1 |
| 10 | 2 | Record length = 60 |
| 12 | 2 | Venue ID |
| 14 | 1 | Price decimal places, 0–18 |
| 15 | 1 | Quantity decimal places, 0–18 |
| 16 | 4 | Instrument ID |
| 20 | 8 | Positive signed tick atoms |
| 28 | 8 | Positive signed quantity atoms |
| 36 | 8 | Reserved, must be zero |
| 44 | 4 | CRC of bytes 0–43 |

## Event — 60 bytes

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 2 | Venue ID (must match header) |
| 2 | 4 | Instrument ID (must match header) |
| 6 | 8 | Canonical sequence |
| 14 | 8 | Exchange sequence |
| 22 | 8 | Exchange timestamp, epoch nanoseconds |
| 30 | 8 | Receive timestamp, monotonic session nanoseconds |
| 38 | 1 | 0 Add; 1 Modify; 2 Cancel; 3 Trade; 4 SnapshotStart; 5 SnapshotEnd |
| 39 | 1 | 0 Buy; 1 Sell |
| 40 | 8 | Signed price ticks |
| 48 | 8 | Signed quantity units |
| 56 | 4 | CRC of bytes 0–55 |

Reader checks header version, sizes, scale metadata, reserved bytes, event/side tags, CRC, and market identity. Book-level semantic validation happens in `VenueBook::apply`. Raw recordings may intentionally contain a semantically invalid event for failure research; the codec never hides or repairs it.

An I/O write failure poisons the writer. A read/decode failure poisons the reader so callers cannot continue at an uncertain boundary. `finish()` flushes but does not generically guarantee durable storage; the demo additionally calls `File::sync_all`. Production crash recovery/checkpointing and async recorder transport remain future work. Never reopen a v1 file with append and write a second header. The demo uses create-new semantics.

A clean EOF is allowed only between complete frames; replay additionally requires a completed book snapshot. CRC cannot detect deletion/reordering of complete frames by itself: sequence validation catches internal gaps, and a future footer is needed to identify loss of a complete final suffix.
