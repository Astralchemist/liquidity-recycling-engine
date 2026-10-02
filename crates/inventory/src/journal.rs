//! Separate inventory command journal v1. CRC32 detects corruption, not adversarial tampering.
use super::*;
use std::io::{self, Read, Write};
pub const HEADER_SIZE: usize = 24;
pub const RECORD_SIZE: usize = 160;
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid inventory journal")
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}
fn money_at(b: &[u8], i: usize) -> Money {
    Money(i128::from_le_bytes(b[i..i + 16].try_into().unwrap()))
}
fn kill_code(k: KillReason) -> u8 {
    match k {
        KillReason::SequenceGap => 0,
        KillReason::StaleData => 1,
        KillReason::ClockAnomaly => 2,
        KillReason::Disconnect => 3,
        KillReason::AckTimeout => 4,
        KillReason::InventoryBreach => 5,
        KillReason::PnlBreach => 6,
        KillReason::CorruptBook => 7,
        KillReason::UnhandledState => 8,
        KillReason::ExcessiveLatency => 9,
        KillReason::PositionAge => 10,
        KillReason::EpisodeDuration => 11,
        KillReason::AdverseEpisodes => 12,
        KillReason::StructureInvalidated => 13,
        KillReason::RegimeChange => 14,
    }
}
fn read_kill(k: u8) -> io::Result<KillReason> {
    Ok(match k {
        0 => KillReason::SequenceGap,
        1 => KillReason::StaleData,
        2 => KillReason::ClockAnomaly,
        3 => KillReason::Disconnect,
        4 => KillReason::AckTimeout,
        5 => KillReason::InventoryBreach,
        6 => KillReason::PnlBreach,
        7 => KillReason::CorruptBook,
        8 => KillReason::UnhandledState,
        9 => KillReason::ExcessiveLatency,
        10 => KillReason::PositionAge,
        11 => KillReason::EpisodeDuration,
        12 => KillReason::AdverseEpisodes,
        13 => KillReason::StructureInvalidated,
        14 => KillReason::RegimeChange,
        _ => return Err(invalid()),
    })
}
fn boolean(b: u8) -> io::Result<bool> {
    match b {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
fn write_charges(b: &mut [u8], c: Charges) {
    b[56..72].copy_from_slice(&c.rebate.0.to_le_bytes());
    b[72..88].copy_from_slice(&c.fee.0.to_le_bytes());
    b[88..104].copy_from_slice(&c.slippage.0.to_le_bytes());
}
fn charges(b: &[u8]) -> Charges {
    Charges {
        rebate: money_at(b, 56),
        fee: money_at(b, 72),
        slippage: money_at(b, 88),
    }
}
/// Canonical record bytes with the CRC field left zero.
fn body(e: InventoryEvent) -> [u8; RECORD_SIZE] {
    let mut b = [0; RECORD_SIZE];
    b[..8].copy_from_slice(&e.sequence.to_le_bytes());
    b[8..16].copy_from_slice(&e.timestamp.0.to_le_bytes());
    let mut id = 0;
    b[16] = match e.kind {
        InventoryEventKind::Mark { venue, bid, ask } => {
            b[32..34].copy_from_slice(&venue.0.to_le_bytes());
            b[40..48].copy_from_slice(&bid.0.to_le_bytes());
            b[48..56].copy_from_slice(&ask.0.to_le_bytes());
            0
        }
        InventoryEventKind::ReserveOpen {
            venue,
            side,
            role,
            evidence,
        } => {
            b[32..34].copy_from_slice(&venue.0.to_le_bytes());
            b[34] = u8::from(side == Side::Sell);
            b[35] = match role {
                InventoryRole::Harvest => 0,
                InventoryRole::Rebalance => 1,
                InventoryRole::Residual => 2,
                InventoryRole::ExitPending => 3,
            };
            b[36] = u8::from(evidence.qualified);
            b[120..128].copy_from_slice(&evidence.void_id.to_le_bytes());
            b[128..136].copy_from_slice(&evidence.revisited_at.0.to_le_bytes());
            b[136..144].copy_from_slice(&evidence.valid_until.0.to_le_bytes());
            1
        }
        InventoryEventKind::FillOpen {
            id: i,
            price,
            maker,
            charges,
        } => {
            id = i;
            b[40..48].copy_from_slice(&price.0.to_le_bytes());
            b[38] = u8::from(maker);
            write_charges(&mut b, charges);
            2
        }
        InventoryEventKind::CancelOpen { id: i } => {
            id = i;
            3
        }
        InventoryEventKind::ReserveClose {
            id: i,
            expected_price,
            expected_charges,
            mode,
        } => {
            id = i;
            b[40..48].copy_from_slice(&expected_price.0.to_le_bytes());
            write_charges(&mut b, expected_charges);
            b[37] = u8::from(mode == CloseMode::Emergency);
            4
        }
        InventoryEventKind::FillClose {
            id: i,
            price,
            maker,
            charges,
        } => {
            id = i;
            b[40..48].copy_from_slice(&price.0.to_le_bytes());
            b[38] = u8::from(maker);
            write_charges(&mut b, charges);
            5
        }
        InventoryEventKind::CancelClose { id: i } => {
            id = i;
            6
        }
        InventoryEventKind::Funding { id: i, cost } => {
            id = i;
            b[104..120].copy_from_slice(&cost.0.to_le_bytes());
            7
        }
        InventoryEventKind::Halt { reason } => {
            b[39] = kill_code(reason);
            8
        }
        InventoryEventKind::Tick => 9,
    };
    b[24..32].copy_from_slice(&id.to_le_bytes());
    b
}
pub fn encode(e: InventoryEvent) -> [u8; RECORD_SIZE] {
    let mut b = body(e);
    let crc = recorder::checksum(&b[..156]);
    b[156..].copy_from_slice(&crc.to_le_bytes());
    b
}
pub fn decode(b: [u8; RECORD_SIZE]) -> io::Result<InventoryEvent> {
    if recorder::checksum(&b[..156]) != u32::from_le_bytes(b[156..].try_into().unwrap()) {
        return Err(invalid());
    }
    let id = u64_at(&b, 24);
    let price = PriceTicks(u64_at(&b, 40) as i64);
    let venue = VenueId(u16::from_le_bytes(b[32..34].try_into().unwrap()));
    let kind = match b[16] {
        0 => InventoryEventKind::Mark {
            venue,
            bid: price,
            ask: PriceTicks(u64_at(&b, 48) as i64),
        },
        1 => InventoryEventKind::ReserveOpen {
            venue,
            side: if boolean(b[34])? {
                Side::Sell
            } else {
                Side::Buy
            },
            role: match b[35] {
                0 => InventoryRole::Harvest,
                1 => InventoryRole::Rebalance,
                2 => InventoryRole::Residual,
                3 => InventoryRole::ExitPending,
                _ => return Err(invalid()),
            },
            evidence: RevisitEvidence {
                void_id: u64_at(&b, 120),
                revisited_at: Timestamp(u64_at(&b, 128)),
                valid_until: Timestamp(u64_at(&b, 136)),
                qualified: boolean(b[36])?,
            },
        },
        2 => InventoryEventKind::FillOpen {
            id,
            price,
            maker: boolean(b[38])?,
            charges: charges(&b),
        },
        3 => InventoryEventKind::CancelOpen { id },
        4 => InventoryEventKind::ReserveClose {
            id,
            expected_price: price,
            expected_charges: charges(&b),
            mode: if boolean(b[37])? {
                CloseMode::Emergency
            } else {
                CloseMode::Normal
            },
        },
        5 => InventoryEventKind::FillClose {
            id,
            price,
            maker: boolean(b[38])?,
            charges: charges(&b),
        },
        6 => InventoryEventKind::CancelClose { id },
        7 => InventoryEventKind::Funding {
            id,
            cost: money_at(&b, 104),
        },
        8 => InventoryEventKind::Halt {
            reason: read_kill(b[39])?,
        },
        9 => InventoryEventKind::Tick,
        _ => return Err(invalid()),
    };
    let e = InventoryEvent {
        sequence: u64_at(&b, 0),
        timestamp: Timestamp(u64_at(&b, 8)),
        kind,
    };
    // Canonical form: reserved bytes and unused fields must be zero. The CRC was checked above.
    if body(e)[..156] != b[..156] {
        return Err(invalid());
    }
    Ok(e)
}
pub struct Writer<W: Write> {
    inner: W,
}
impl<W: Write> Writer<W> {
    pub fn new(mut inner: W, config_checksum: u32) -> io::Result<Self> {
        let mut h = [0; HEADER_SIZE];
        h[..8].copy_from_slice(b"LRINV\0\0\0");
        h[8..10].copy_from_slice(&1_u16.to_le_bytes());
        h[10..12].copy_from_slice(&(RECORD_SIZE as u16).to_le_bytes());
        h[12..16].copy_from_slice(&config_checksum.to_le_bytes());
        let crc = recorder::checksum(&h[..20]);
        h[20..].copy_from_slice(&crc.to_le_bytes());
        inner.write_all(&h)?;
        Ok(Self { inner })
    }
    pub fn append(&mut self, e: InventoryEvent) -> io::Result<()> {
        self.inner.write_all(&encode(e))
    }
    pub fn finish(mut self) -> io::Result<W> {
        self.inner.flush()?;
        Ok(self.inner)
    }
}
pub struct Reader<R: Read> {
    inner: R,
}
impl<R: Read> Reader<R> {
    pub fn new(mut inner: R, config_checksum: u32) -> io::Result<Self> {
        let mut h = [0; HEADER_SIZE];
        inner.read_exact(&mut h)?;
        if &h[..8] != b"LRINV\0\0\0"
            || u16::from_le_bytes(h[8..10].try_into().unwrap()) != 1
            || u16::from_le_bytes(h[10..12].try_into().unwrap()) != RECORD_SIZE as u16
            || u32::from_le_bytes(h[12..16].try_into().unwrap()) != config_checksum
            || h[16..20] != [0; 4]
            || recorder::checksum(&h[..20]) != u32::from_le_bytes(h[20..].try_into().unwrap())
        {
            return Err(invalid());
        }
        Ok(Self { inner })
    }
    pub fn next_event(&mut self) -> io::Result<Option<InventoryEvent>> {
        let mut b = [0; RECORD_SIZE];
        match self.inner.read_exact(&mut b[..1]) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        self.inner.read_exact(&mut b[1..])?;
        decode(b).map(Some)
    }
}
