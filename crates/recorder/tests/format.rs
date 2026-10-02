use common::*;
use fixed_point::*;
use market_events::*;
use recorder::*;
fn metadata() -> RecordingMetadata {
    RecordingMetadata {
        venue: VenueId(1),
        instrument: InstrumentId(7),
        price_decimals: 2,
        quantity_decimals: 6,
        tick_atoms: 1,
        quantity_atoms: 10,
    }
}
fn event(seq: u64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(1),
        instrument: InstrumentId(7),
        sequence: seq,
        exchange_sequence: seq + 100,
        exchange_ts: 123456789,
        receive_ts: Timestamp(seq),
        event_type: MarketEventType::Modify,
        side: Side::Sell,
        price_ticks: PriceTicks(i64::MAX),
        qty_units: QtyUnits(100),
    }
}
#[test]
fn round_trip_and_corruption_at_every_byte() {
    let mut writer = Recorder::new(Vec::new(), metadata()).unwrap();
    writer.append(&event(1)).unwrap();
    let bytes = writer.finish().unwrap();
    assert_eq!(bytes.len(), HEADER_SIZE + RECORD_SIZE);
    let mut reader = RecordingReader::new(bytes.as_slice()).unwrap();
    assert_eq!(reader.metadata, metadata());
    assert_eq!(reader.next_event().unwrap(), Some(event(1)));
    assert_eq!(reader.next_event().unwrap(), None);
    for index in 0..bytes.len() {
        let mut corrupt = bytes.clone();
        corrupt[index] ^= 1;
        match RecordingReader::new(corrupt.as_slice()) {
            Err(_) => assert!(index < HEADER_SIZE),
            Ok(mut r) => {
                assert!(r.next_event().is_err());
                assert!(r.next_event().is_err());
            }
        }
    }
}
#[test]
fn truncation_never_looks_like_clean_eof() {
    let mut writer = Recorder::new(Vec::new(), metadata()).unwrap();
    writer.append(&event(1)).unwrap();
    let bytes = writer.finish().unwrap();
    for length in 0..bytes.len() {
        match RecordingReader::new(&bytes[..length]) {
            Err(_) => assert!(length < HEADER_SIZE),
            Ok(mut r) if length == HEADER_SIZE => assert_eq!(r.next_event().unwrap(), None),
            Ok(mut r) => assert!(r.next_event().is_err()),
        }
    }
}
#[test]
fn generated_event_codec_round_trip() {
    for sequence in 1..10_000 {
        let mut e = event(sequence);
        e.side = if sequence % 2 == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        e.event_type = [
            MarketEventType::Add,
            MarketEventType::Modify,
            MarketEventType::Cancel,
            MarketEventType::Trade,
            MarketEventType::SnapshotStart,
            MarketEventType::SnapshotEnd,
        ][sequence as usize % 6];
        e.price_ticks = PriceTicks(-(sequence as i64));
        assert_eq!(decode_event(&encode_event(&e)).unwrap(), e);
    }
}
#[test]
fn wrong_market_rejected_before_write() {
    let mut writer = Recorder::new(Vec::new(), metadata()).unwrap();
    let mut e = event(1);
    e.venue = VenueId(99);
    assert!(writer.append(&e).is_err());
    assert_eq!(writer.finish().unwrap().len(), HEADER_SIZE);
}

#[test]
fn unknown_tags_and_versions_are_rejected_even_with_valid_checksum() {
    for (offset, value) in [(38, 6), (39, 2)] {
        let mut bytes = encode_event(&event(1));
        bytes[offset] = value;
        let crc = checksum(&bytes[..56]);
        bytes[56..].copy_from_slice(&crc.to_le_bytes());
        assert!(decode_event(&bytes).is_err());
    }
    let writer = Recorder::new(Vec::new(), metadata()).unwrap();
    let original = writer.finish().unwrap();
    for (offset, value) in [(8, 2), (10, 59), (14, 19), (36, 1)] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        let crc = checksum(&bytes[..44]);
        bytes[44..].copy_from_slice(&crc.to_le_bytes());
        assert!(RecordingReader::new(bytes.as_slice()).is_err());
    }
}

#[test]
fn partial_write_poisons_recorder() {
    use std::io::{self, Write};
    struct FailAfter {
        remaining: usize,
    }
    impl Write for FailAfter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected disk failure"));
            }
            let n = self.remaining.min(bytes.len());
            self.remaining -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut r = Recorder::new(
        FailAfter {
            remaining: HEADER_SIZE + 5,
        },
        metadata(),
    )
    .unwrap();
    assert!(r.append(&event(1)).is_err());
    assert!(r.append(&event(2)).is_err());
    assert!(r.finish().is_err());
}
