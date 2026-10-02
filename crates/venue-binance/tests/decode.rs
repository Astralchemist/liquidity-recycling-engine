use common::Side;
use market_events::MarketEventType::*;
use venue_binance::BinanceFutures;
use wire::{
    feed::{DecodeError, Decoded, Drop, Scale},
    window::{FrameKind, WireFrame},
};
const SCALE: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 3,
    qty_atoms: 1,
};
fn depth(u: u64, pu: u64, bids: &str, asks: &str) -> String {
    format!(
        r#"{{"e":"depthUpdate","E":1700000000123,"T":1700000000120,"s":"BTCUSDT","U":{},"u":{u},"pu":{pu},"b":[{bids}],"a":[{asks}]}}"#,
        u - 5
    )
}
fn changes(f: &WireFrame) -> Vec<(market_events::MarketEventType, Side, i64, i64)> {
    f.changes()
        .iter()
        .map(|c| (c.kind, c.side, c.price, c.qty))
        .collect()
}
#[test]
fn partial_depth_snapshots_become_a_snapshot_then_diff_batches() {
    let mut d = BinanceFutures::<64>::new("BTCUSDT", SCALE, 2).unwrap();
    let mut f = WireFrame::new(1);
    let first = depth(
        100,
        90,
        r#"["60000.10","1.000"],["60000.00","2.5"]"#,
        r#"["60000.20","0.300"],["60000.30","4.000"]"#,
    );
    assert_eq!(d.decode(first.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!(f.kind, FrameKind::Snapshot);
    assert_eq!(
        (f.exchange_sequence, f.exchange_ts_ns),
        (100, 1_700_000_000_120_000_000)
    );
    assert_eq!(f.len, 4);
    // The next complete snapshot moves the best bid and resizes an ask: one atomic diff.
    let second = depth(
        110,
        100,
        r#"["60000.00","2.500"],["59999.90","7.000"]"#,
        r#"["60000.20","0.100"],["60000.30","4.000"]"#,
    );
    assert_eq!(d.decode(second.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!(f.kind, FrameKind::Batch);
    assert_eq!(
        changes(&f),
        [
            (Cancel, Side::Buy, 600_001, 0),
            (Modify, Side::Sell, 600_002, 100),
            (Add, Side::Buy, 599_999, 7000),
        ]
    );
    // Repeated or older update IDs are dropped; a skipped `pu` is counted but accepted.
    assert_eq!(
        d.decode(second.as_bytes(), &mut f),
        Ok(Decoded::Dropped(Drop::StaleSequence))
    );
    let third = depth(
        130,
        125,
        r#"["60000.00","2.500"],["59999.90","7.000"]"#,
        r#"["60000.20","0.100"],["60000.30","4.000"]"#,
    );
    assert_eq!(d.decode(third.as_bytes(), &mut f), Ok(Decoded::Unchanged));
    assert_eq!(d.stats.skipped_updates, 1);
    assert_eq!(
        (d.stats.frames, d.stats.stale, d.stats.unchanged),
        (2, 1, 1)
    );
}
#[test]
fn aggregate_trades_map_maker_flag_to_aggressor_side() {
    let mut d = BinanceFutures::<64>::new("BTCUSDT", SCALE, 2).unwrap();
    let mut f = WireFrame::new(1);
    for (maker, side) in [("true", Side::Sell), ("false", Side::Buy)] {
        let msg = format!(
            r#"{{"e":"aggTrade","E":1700000000200,"a":555,"s":"BTCUSDT","p":"60000.10","q":"0.012","nq":"0.012","f":10,"l":12,"T":1700000000199,"m":{maker}}}"#
        );
        assert_eq!(d.decode(msg.as_bytes(), &mut f), Ok(Decoded::Frame));
        assert_eq!(f.kind, FrameKind::Trades);
        assert_eq!(changes(&f), [(Trade, side, 600_001, 12)]);
        assert_eq!(f.exchange_sequence, 555);
    }
}
#[test]
fn envelopes_controls_and_bad_messages_are_explicit() {
    let mut d = BinanceFutures::<64>::new("BTCUSDT", SCALE, 2).unwrap();
    let mut f = WireFrame::new(1);
    let inner = depth(100, 90, r#"["60000.10","1"]"#, r#"["60000.20","1"]"#);
    let wrapped = format!(r#"{{"stream":"btcusdt@depth20@100ms","data":{inner}}}"#);
    assert_eq!(d.decode(wrapped.as_bytes(), &mut f), Ok(Decoded::Frame));
    assert_eq!(
        d.decode(br#"{"result":null,"id":1}"#, &mut f),
        Ok(Decoded::Control)
    );
    let other =
        depth(200, 100, r#"["60000.10","1"]"#, r#"["60000.20","1"]"#).replace("BTCUSDT", "ETHUSDT");
    assert_eq!(
        d.decode(other.as_bytes(), &mut f),
        Err(DecodeError::WrongSymbol)
    );
    let off_tick = depth(300, 100, r#"["60000.15","1"]"#, r#"["60000.20","1"]"#);
    assert!(d.decode(off_tick.as_bytes(), &mut f).is_err());
    let crossed = depth(400, 100, r#"["60000.30","1"]"#, r#"["60000.20","1"]"#);
    assert!(matches!(
        d.decode(crossed.as_bytes(), &mut f),
        Ok(Decoded::Resync(_))
    ));
    assert_eq!(f.kind, FrameKind::Reset);
}
