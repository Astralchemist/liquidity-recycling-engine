//! Version 1 little-endian records. Header and records have CRC32/IEEE.
//! Run I/O outside the decision thread. EOF is clean only on a frame boundary.
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType};
use std::io::{self, Read, Write};
pub const HEADER_SIZE: usize = 48;
pub const RECORD_SIZE: usize = 60;
const MAGIC: &[u8; 8] = b"LREVENT\0";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingMetadata {
    pub venue: VenueId,
    pub instrument: InstrumentId,
    pub price_decimals: u8,
    pub quantity_decimals: u8,
    pub tick_atoms: i64,
    pub quantity_atoms: i64,
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// Reflected CRC32/IEEE remainder of every byte value, built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};
pub fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
    }
    !crc
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
impl RecordingMetadata {
    fn validate(self) -> io::Result<()> {
        if self.price_decimals > 18
            || self.quantity_decimals > 18
            || self.tick_atoms <= 0
            || self.quantity_atoms <= 0
        {
            return Err(invalid("invalid instrument scale"));
        }
        Ok(())
    }
    fn encode(self) -> io::Result<[u8; HEADER_SIZE]> {
        self.validate()?;
        let mut b = [0; HEADER_SIZE];
        b[..8].copy_from_slice(MAGIC);
        b[8..10].copy_from_slice(&1_u16.to_le_bytes());
        b[10..12].copy_from_slice(&(RECORD_SIZE as u16).to_le_bytes());
        b[12..14].copy_from_slice(&self.venue.0.to_le_bytes());
        b[14] = self.price_decimals;
        b[15] = self.quantity_decimals;
        b[16..20].copy_from_slice(&self.instrument.0.to_le_bytes());
        b[20..28].copy_from_slice(&self.tick_atoms.to_le_bytes());
        b[28..36].copy_from_slice(&self.quantity_atoms.to_le_bytes());
        let crc = checksum(&b[..44]);
        b[44..48].copy_from_slice(&crc.to_le_bytes());
        Ok(b)
    }
    fn decode(b: [u8; HEADER_SIZE]) -> io::Result<Self> {
        if &b[..8] != MAGIC
            || u16_at(&b, 8) != 1
            || u16_at(&b, 10) as usize != RECORD_SIZE
            || b[36..44] != [0; 8]
            || checksum(&b[..44]) != u32_at(&b, 44)
        {
            return Err(invalid("invalid recording header"));
        }
        let metadata = Self {
            venue: VenueId(u16_at(&b, 12)),
            instrument: InstrumentId(u32_at(&b, 16)),
            price_decimals: b[14],
            quantity_decimals: b[15],
            tick_atoms: u64_at(&b, 20) as i64,
            quantity_atoms: u64_at(&b, 28) as i64,
        };
        metadata.validate()?;
        Ok(metadata)
    }
}
pub fn encode_event(e: &MarketEvent) -> [u8; RECORD_SIZE] {
    let mut b = [0; RECORD_SIZE];
    b[..2].copy_from_slice(&e.venue.0.to_le_bytes());
    b[2..6].copy_from_slice(&e.instrument.0.to_le_bytes());
    b[6..14].copy_from_slice(&e.sequence.to_le_bytes());
    b[14..22].copy_from_slice(&e.exchange_sequence.to_le_bytes());
    b[22..30].copy_from_slice(&e.exchange_ts.to_le_bytes());
    b[30..38].copy_from_slice(&e.receive_ts.0.to_le_bytes());
    b[38] = e.event_type as u8;
    b[39] = match e.side {
        Side::Buy => 0,
        Side::Sell => 1,
    };
    b[40..48].copy_from_slice(&e.price_ticks.0.to_le_bytes());
    b[48..56].copy_from_slice(&e.qty_units.0.to_le_bytes());
    let crc = checksum(&b[..56]);
    b[56..60].copy_from_slice(&crc.to_le_bytes());
    b
}
pub fn decode_event(b: &[u8; RECORD_SIZE]) -> io::Result<MarketEvent> {
    if checksum(&b[..56]) != u32_at(b, 56) {
        return Err(invalid("record checksum mismatch"));
    }
    let event_type = match b[38] {
        0 => MarketEventType::Add,
        1 => MarketEventType::Modify,
        2 => MarketEventType::Cancel,
        3 => MarketEventType::Trade,
        4 => MarketEventType::SnapshotStart,
        5 => MarketEventType::SnapshotEnd,
        _ => return Err(invalid("unknown event kind")),
    };
    let side = match b[39] {
        0 => Side::Buy,
        1 => Side::Sell,
        _ => return Err(invalid("unknown side")),
    };
    Ok(MarketEvent {
        venue: VenueId(u16_at(b, 0)),
        instrument: InstrumentId(u32_at(b, 2)),
        sequence: u64_at(b, 6),
        exchange_sequence: u64_at(b, 14),
        exchange_ts: u64_at(b, 22),
        receive_ts: Timestamp(u64_at(b, 30)),
        event_type,
        side,
        price_ticks: PriceTicks(u64_at(b, 40) as i64),
        qty_units: QtyUnits(u64_at(b, 48) as i64),
    })
}
pub struct Recorder<W> {
    writer: W,
    metadata: RecordingMetadata,
    failed: bool,
}
impl<W: Write> Recorder<W> {
    pub fn new(mut writer: W, metadata: RecordingMetadata) -> io::Result<Self> {
        writer.write_all(&metadata.encode()?)?;
        Ok(Self {
            writer,
            metadata,
            failed: false,
        })
    }
    pub fn append(&mut self, event: &MarketEvent) -> io::Result<()> {
        if self.failed {
            return Err(invalid("recorder poisoned by previous I/O failure"));
        }
        if event.venue != self.metadata.venue || event.instrument != self.metadata.instrument {
            return Err(invalid("recording market mismatch"));
        }
        let result = self.writer.write_all(&encode_event(event));
        self.failed = result.is_err();
        result
    }
    pub fn finish(mut self) -> io::Result<W> {
        if self.failed {
            return Err(invalid("recorder poisoned by previous I/O failure"));
        }
        self.writer.flush()?;
        Ok(self.writer)
    }
}
pub struct RecordingReader<R> {
    reader: R,
    pub metadata: RecordingMetadata,
    failed: bool,
}
impl<R: Read> RecordingReader<R> {
    pub fn new(mut reader: R) -> io::Result<Self> {
        let mut header = [0; HEADER_SIZE];
        reader.read_exact(&mut header)?;
        Ok(Self {
            reader,
            metadata: RecordingMetadata::decode(header)?,
            failed: false,
        })
    }
    pub fn next_event(&mut self) -> io::Result<Option<MarketEvent>> {
        if self.failed {
            return Err(invalid("reader poisoned by corrupt or truncated record"));
        }
        let result = self.read_event();
        self.failed = result.is_err();
        result
    }
    fn read_event(&mut self) -> io::Result<Option<MarketEvent>> {
        let mut record = [0; RECORD_SIZE];
        loop {
            match self.reader.read(&mut record[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        self.reader.read_exact(&mut record[1..])?;
        let event = decode_event(&record)?;
        if event.venue != self.metadata.venue || event.instrument != self.metadata.instrument {
            return Err(invalid("recording market mismatch"));
        }
        Ok(Some(event))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_crc_vector() {
        assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
    }
    /// Bit-at-a-time definition the table must reproduce exactly.
    fn bitwise(bytes: &[u8]) -> u32 {
        let mut crc = !0_u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }
    #[test]
    fn table_crc_matches_bitwise_definition() {
        let mut state = 7_u64;
        let mut bytes = [0_u8; 300];
        for byte in &mut bytes {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state >> 56) as u8;
        }
        for len in 0..=bytes.len() {
            assert_eq!(checksum(&bytes[..len]), bitwise(&bytes[..len]));
        }
        for value in 0..=255_u8 {
            assert_eq!(checksum(&[value]), bitwise(&[value]));
        }
    }
}
