use common::Side;
use market_events::MarketEventType::*;
use venue_bybit::BybitLinear;
use wire::{
    feed::{DecodeError, Decoded, Drop, Resync, Scale},
    window::{FrameKind, WireFrame},
};
const SCALE: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 3,
    qty_atoms: 1,
};
fn book(kind: &str, u: u64, seq: u64, bids: &str, asks: &str) -> String {
    format!(
        r#"{{"topic":"orderbook.50.BTCUSDT","type":"{kind}","ts":1700000000500,"data":{{"s":"BTCUSDT","b":[{bids}],"a":[{asks}],"u":{u},"seq":{seq}}},"cts":1700000000498}}"#
    )
}
#[test]
fn snapshot_contiguous_deltas_duplicates_and_gaps() {
    let mut d = BybitLinear::<128>::new("BTCUSDT", SCALE, 3).unwrap();
    let mut f = WireFrame::new(2);
    let early = book("delta", 5, 1, r#"["60000.0","1"]"#, "");
    assert_eq!(
        d.decode(early.as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::BeforeSnapshot))
    );
    let snap = book(
        "snapshot",
        10,
        100,
        r#"["60000.0","1.000"],["59999.9","2.000"]"#,
        r#"["60000.1","0.500"]"#,
    );
    assert_eq!(d.decode(snap.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!(
        (f.kind, f.len, f.exchange_ts_ns),
        (FrameKind::Snapshot, 3, 1_700_000_000_498_000_000)
    );
    let delta = book(
        "delta",
        11,
        101,
        r#"["60000.0","0"],["59999.8","3.000"]"#,
        r#"["60000.1","0.700"]"#,
    );
    assert_eq!(d.decode(delta.as_bytes(), &mut f), Ok(Decoded::Frame));
    let got: Vec<_> = f
        .changes()
        .iter()
        .map(|c| (c.kind, c.side, c.price))
        .collect();
    assert_eq!(
        got,
        [
            (Cancel, Side::Buy, 600_000),
            (Modify, Side::Sell, 600_001),
            (Add, Side::Buy, 599_998)
        ]
    );
    assert_eq!(
        d.decode(delta.as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::Duplicate))
    );
    let backwards = book("delta", 12, 90, r#"["59999.7","1"]"#, "");
    assert_eq!(
        d.decode(backwards.as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::StaleSequence))
    );
    let gap = book("delta", 13, 103, r#"["59999.7","1"]"#, "");
    assert_eq!(
        d.decode(gap.as_bytes(), &mut f),
        Ok(Decoded::Resync(Resync::Gap))
    );
    assert_eq!(f.kind, FrameKind::Reset);
    // Until a new snapshot, deltas are refused; a restart snapshot (u = 1) restores the book.
    assert_eq!(
        d.decode(book("delta", 14, 104, "", "").as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::BeforeSnapshot))
    );
    let restart = book(
        "snapshot",
        1,
        200,
        r#"["60000.0","1"]"#,
        r#"["60000.1","1"]"#,
    );
    assert_eq!(d.decode(restart.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!(f.kind, FrameKind::Snapshot);
    assert_eq!(d.stats.resyncs, 1);
}
#[test]
fn trades_use_taker_side_and_exclude_block_and_rpi_prints() {
    let mut d = BybitLinear::<128>::new("BTCUSDT", SCALE, 3).unwrap();
    let mut f = WireFrame::new(2);
    let msg = br#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1700000000600,"data":[{"T":1700000000599,"s":"BTCUSDT","S":"Buy","v":"0.010","p":"60000.1","L":"PlusTick","i":"a-1","BT":false,"seq":7},{"T":1700000000599,"s":"BTCUSDT","S":"Sell","v":"0.020","p":"60000.0","L":"MinusTick","i":"a-2","BT":true,"seq":7},{"T":1700000000599,"s":"BTCUSDT","S":"Sell","v":"0.003","p":"59999.9","i":"a-3","BT":false,"RPI":true,"seq":7}]}"#;
    assert_eq!(d.decode(msg, &mut f), Ok(Decoded::Frame));
    let got: Vec<_> = f
        .changes()
        .iter()
        .map(|c| (c.kind, c.side, c.price, c.qty))
        .collect();
    assert_eq!(got, [(Trade, Side::Buy, 600_001, 10)]);
    assert_eq!(d.stats.excluded_prints, 2);
}
#[test]
fn acknowledgements_failures_and_foreign_topics() {
    let mut d = BybitLinear::<128>::new("BTCUSDT", SCALE, 3).unwrap();
    let mut f = WireFrame::new(2);
    assert_eq!(
        d.decode(
            br#"{"success":true,"ret_msg":"","conn_id":"x","op":"subscribe"}"#,
            &mut f
        ),
        Ok(Decoded::Control)
    );
    assert_eq!(
        d.decode(
            br#"{"success":true,"ret_msg":"pong","conn_id":"x","op":"ping"}"#,
            &mut f
        ),
        Ok(Decoded::Control)
    );
    assert_eq!(
        d.decode(
            br#"{"success":false,"ret_msg":"bad","conn_id":"x","op":"subscribe"}"#,
            &mut f
        ),
        Err(DecodeError::Venue)
    );
    let foreign = book("snapshot", 1, 1, r#"["1.0","1"]"#, r#"["2.0","1"]"#)
        .replace("orderbook.50.BTCUSDT", "orderbook.50.ETHUSDT");
    assert!(d.decode(foreign.as_bytes(), &mut f).is_err());
}
#[test]
fn large_trade_bursts_leave_in_chunks_without_losing_prints() {
    use wire::{feed::NativeDecoder, window::MAX_FRAME};
    let mut d = BybitLinear::<128>::new("BTCUSDT", SCALE, 3).unwrap();
    let mut f = WireFrame::new(2);
    let prints: Vec<String> = (0..300)
        .map(|i| format!(r#"{{"T":1700000000599,"s":"BTCUSDT","S":"Buy","v":"0.001","p":"{}.0","BT":false,"seq":9}}"#, 60000 + i))
        .collect();
    let msg = format!(
        r#"{{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1700000000600,"data":[{}]}}"#,
        prints.join(",")
    );
    assert_eq!(d.decode(msg.as_bytes(), &mut f), Ok(Decoded::Frame));
    let mut seen = f.changes().to_vec();
    while NativeDecoder::drain(&mut d, &mut f) {
        assert_eq!(f.kind, FrameKind::Trades);
        seen.extend_from_slice(f.changes());
    }
    assert_eq!(seen.len(), 300);
    assert!(seen.len() > MAX_FRAME);
    assert!(seen.windows(2).all(|w| w[1].price == w[0].price + 10));
}
