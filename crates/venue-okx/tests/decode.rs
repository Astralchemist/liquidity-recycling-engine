use common::Side;
use market_events::MarketEventType::*;
use venue_okx::OkxSwap;
use wire::{
    feed::{DecodeError, Decoded, Drop, Resync, Scale},
    window::{FrameKind, WireFrame},
};
/// BTC-USDT-SWAP: tick 0.1; sizes in contracts with lot 0.01 (each lot 0.0001 BTC).
const SCALE: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 2,
    qty_atoms: 1,
};
fn books(action: &str, previous: i64, seq: i64, bids: &str, asks: &str) -> String {
    format!(
        r#"{{"arg":{{"channel":"books","instId":"BTC-USDT-SWAP"}},"action":"{action}","data":[{{"asks":[{asks}],"bids":[{bids}],"ts":"1700000000700","checksum":0,"prevSeqId":{previous},"seqId":{seq}}}]}}"#
    )
}
#[test]
fn snapshot_linked_updates_heartbeats_and_sequence_resets() {
    let mut d = OkxSwap::<512>::new("BTC-USDT-SWAP", SCALE, 3).unwrap();
    let mut f = WireFrame::new(3);
    assert_eq!(
        d.decode(books("update", 1, 2, "", "").as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::BeforeSnapshot))
    );
    let snap = books(
        "snapshot",
        -1,
        100,
        r#"["60000.0","12.5","0","3"]"#,
        r#"["60000.1","1","0","1"]"#,
    );
    assert_eq!(d.decode(snap.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!((f.kind, f.exchange_sequence), (FrameKind::Snapshot, 100));
    assert_eq!(f.changes()[0].qty, 1250);
    let update = books(
        "update",
        100,
        105,
        r#"["60000.0","0","0","0"],["59999.9","2","0","1"]"#,
        "",
    );
    assert_eq!(d.decode(update.as_bytes(), &mut f), Ok(Decoded::Frame));
    let got: Vec<_> = f
        .changes()
        .iter()
        .map(|c| (c.kind, c.side, c.price))
        .collect();
    assert_eq!(
        got,
        [(Cancel, Side::Buy, 600_000), (Add, Side::Buy, 599_999)]
    );
    // Heartbeat: no levels and prevSeqId == seqId.
    assert_eq!(
        d.decode(books("update", 105, 105, "", "").as_bytes(), &mut f),
        Ok(Decoded::Unchanged)
    );
    // After maintenance seqId may decrease; the prevSeqId link is what matters.
    let reset_seq = books("update", 105, 3, "", r#"["60000.2","4","0","1"]"#);
    assert_eq!(d.decode(reset_seq.as_bytes(), &mut f), Ok(Decoded::Frame));
    let broken = books("update", 99, 7, "", r#"["60000.3","1","0","1"]"#);
    assert_eq!(
        d.decode(broken.as_bytes(), &mut f),
        Ok(Decoded::Resync(Resync::Gap))
    );
    assert_eq!(f.kind, FrameKind::Reset);
    assert!(
        d.decode(
            books(
                "snapshot",
                5,
                9,
                r#"["1.0","1","0","1"]"#,
                r#"["2.0","1","0","1"]"#
            )
            .as_bytes(),
            &mut f
        )
        .is_err()
    );
}
#[test]
fn trades_use_taker_side_and_exclude_rpi() {
    let mut d = OkxSwap::<512>::new("BTC-USDT-SWAP", SCALE, 3).unwrap();
    let mut f = WireFrame::new(3);
    let msg = br#"{"arg":{"channel":"trades","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","tradeId":"130639474","px":"60000.1","sz":"0.05","side":"sell","ts":"1700000000800","count":"3","source":"0","seqId":4321},{"instId":"BTC-USDT-SWAP","tradeId":"130639475","px":"60000.2","sz":"1","side":"buy","ts":"1700000000801","count":"1","source":"1","seqId":4322}]}"#;
    assert_eq!(d.decode(msg, &mut f), Ok(Decoded::Frame));
    let got: Vec<_> = f
        .changes()
        .iter()
        .map(|c| (c.kind, c.side, c.price, c.qty))
        .collect();
    assert_eq!(got, [(Trade, Side::Sell, 600_001, 5)]);
    assert_eq!(d.stats.excluded_prints, 1);
}
#[test]
fn control_frames_and_errors() {
    let mut d = OkxSwap::<512>::new("BTC-USDT-SWAP", SCALE, 3).unwrap();
    let mut f = WireFrame::new(3);
    assert_eq!(d.decode(b"pong", &mut f), Ok(Decoded::Control));
    assert_eq!(d.decode(br#"{"event":"subscribe","arg":{"channel":"books","instId":"BTC-USDT-SWAP"},"connId":"a"}"#, &mut f), Ok(Decoded::Control));
    assert_eq!(
        d.decode(
            br#"{"event":"error","code":"60012","msg":"bad","connId":"a"}"#,
            &mut f
        ),
        Err(DecodeError::Venue)
    );
    let foreign = books(
        "snapshot",
        -1,
        1,
        r#"["1.0","1","0","1"]"#,
        r#"["2.0","1","0","1"]"#,
    )
    .replace("BTC-USDT-SWAP", "ETH-USDT-SWAP");
    assert_eq!(
        d.decode(foreign.as_bytes(), &mut f),
        Err(DecodeError::WrongSymbol)
    );
}
