//! Version 2 frames: atomic native batch boundaries survive recording and replay.
use common::*;
use fixed_point::*;
use market_events::*;
use recorder::*;
fn metadata() -> RecordingMetadata {
    RecordingMetadata {
        venue: VenueId(1),
        instrument: InstrumentId(7),
        price_decimals: 1,
        quantity_decimals: 4,
        tick_atoms: 1,
        quantity_atoms: 1,
    }
}
fn event(seq: u64, ts: u64, kind: MarketEventType, price: i64, qty: i64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(1),
        instrument: InstrumentId(7),
        sequence: seq,
        exchange_sequence: 900 + ts,
        exchange_ts: 1_700_000_000_000 + ts,
        receive_ts: Timestamp(ts),
        event_type: kind,
        side: if price < 100 { Side::Buy } else { Side::Sell },
        price_ticks: PriceTicks(price),
        qty_units: QtyUnits(qty),
    }
}
fn batch(first_seq: u64, ts: u64) -> [MarketEvent; 3] {
    [
        event(first_seq, ts, MarketEventType::Cancel, 101, 0),
        event(first_seq + 1, ts, MarketEventType::Add, 99, 5),
        event(first_seq + 2, ts, MarketEventType::Modify, 98, 7),
    ]
}
fn sample() -> Vec<u8> {
    let mut w = FrameRecorder::new(Vec::new(), metadata()).unwrap();
    w.append_event(&event(1, 1, MarketEventType::Trade, 100, 3))
        .unwrap();
    w.append_batch(&batch(2, 2)).unwrap();
    w.append_event(&event(5, 3, MarketEventType::Modify, 97, 1))
        .unwrap();
    w.finish().unwrap()
}
#[test]
fn mixed_frames_round_trip_with_batch_boundaries() {
    let bytes = sample();
    assert_eq!(
        bytes.len(),
        HEADER_SIZE + 3 * FRAME_HEADER_SIZE + 5 * RECORD_SIZE
    );
    let mut reader = RecordingReader::new(bytes.as_slice()).unwrap();
    assert_eq!(reader.version(), 2);
    let mut buf = [event(0, 0, MarketEventType::Add, 1, 1); 8];
    assert_eq!(
        reader.next_frame(&mut buf).unwrap(),
        Some(Frame::Event(event(1, 1, MarketEventType::Trade, 100, 3)))
    );
    assert_eq!(reader.next_frame(&mut buf).unwrap(), Some(Frame::Batch(3)));
    assert_eq!(buf[..3], batch(2, 2));
    assert!(matches!(
        reader.next_frame(&mut buf).unwrap(),
        Some(Frame::Event(_))
    ));
    assert_eq!(reader.next_frame(&mut buf).unwrap(), None);
    // Flattening a v2 file event by event is refused.
    let mut reader = RecordingReader::new(bytes.as_slice()).unwrap();
    assert!(reader.next_event().is_err());
}
#[test]
fn version_one_recordings_read_as_single_event_frames() {
    let mut w = Recorder::new(Vec::new(), metadata()).unwrap();
    w.append(&event(1, 1, MarketEventType::Trade, 100, 3))
        .unwrap();
    let bytes = w.finish().unwrap();
    let mut reader = RecordingReader::new(bytes.as_slice()).unwrap();
    assert_eq!(reader.version(), 1);
    assert!(matches!(
        reader.next_frame(&mut []).unwrap(),
        Some(Frame::Event(_))
    ));
    assert_eq!(reader.next_frame(&mut []).unwrap(), None);
}
#[test]
fn writer_enforces_the_atomic_batch_contract() {
    let mut w = FrameRecorder::new(Vec::new(), metadata()).unwrap();
    let mut late = batch(1, 5);
    late[2].receive_ts = Timestamp(6);
    let mut other_update = batch(1, 5);
    other_update[1].exchange_sequence += 1;
    let mut trade = batch(1, 5);
    trade[0].event_type = MarketEventType::Trade;
    let mut gap = batch(1, 5);
    gap[2].sequence = 9;
    let mut foreign = batch(1, 5);
    foreign[0].venue = VenueId(2);
    for bad in [late, other_update, trade, gap, foreign] {
        assert!(w.append_batch(&bad).is_err());
    }
    assert!(w.append_batch(&[]).is_err());
    // Rejected batches write nothing, and the writer remains usable.
    w.append_batch(&batch(1, 5)).unwrap();
    let bytes = w.finish().unwrap();
    assert_eq!(
        bytes.len(),
        HEADER_SIZE + FRAME_HEADER_SIZE + 3 * RECORD_SIZE
    );
}
#[test]
fn reader_rejects_corrupt_headers_small_buffers_truncation_and_contract_breaks() {
    let bytes = sample();
    let mut buf = [event(0, 0, MarketEventType::Add, 1, 1); 8];
    // Every byte of the batch frame header.
    let at = HEADER_SIZE + FRAME_HEADER_SIZE + RECORD_SIZE;
    for i in at..at + FRAME_HEADER_SIZE {
        let mut bad = bytes.clone();
        bad[i] ^= 1;
        let mut r = RecordingReader::new(bad.as_slice()).unwrap();
        r.next_frame(&mut buf).unwrap();
        assert!(r.next_frame(&mut buf).is_err(), "header byte {i}");
        assert!(r.next_frame(&mut buf).is_err(), "reader must stay poisoned");
    }
    let mut r = RecordingReader::new(bytes.as_slice()).unwrap();
    r.next_frame(&mut buf).unwrap();
    assert!(r.next_frame(&mut buf[..2]).is_err());
    // Truncation inside a frame is an error; a cut exactly between frames is a clean end.
    let cut = &bytes[..at + FRAME_HEADER_SIZE + RECORD_SIZE];
    let mut r = RecordingReader::new(cut).unwrap();
    r.next_frame(&mut buf).unwrap();
    assert!(r.next_frame(&mut buf).is_err());
    let mut r = RecordingReader::new(&bytes[..at]).unwrap();
    r.next_frame(&mut buf).unwrap();
    assert_eq!(r.next_frame(&mut buf).unwrap(), None);
    // A batch frame with valid checksums but mixed receive times breaks the contract.
    let mut members = batch(2, 2);
    members[1].receive_ts = Timestamp(9);
    let mut crafted = bytes[..HEADER_SIZE].to_vec();
    let mut header = [0_u8; FRAME_HEADER_SIZE];
    header[..4].copy_from_slice(b"LRFR");
    header[4] = 1;
    header[8..12].copy_from_slice(&3_u32.to_le_bytes());
    let crc = checksum(&header[..12]);
    header[12..].copy_from_slice(&crc.to_le_bytes());
    crafted.extend_from_slice(&header);
    for m in &members {
        crafted.extend_from_slice(&encode_event(m));
    }
    let mut r = RecordingReader::new(crafted.as_slice()).unwrap();
    assert!(r.next_frame(&mut buf).is_err());
}
