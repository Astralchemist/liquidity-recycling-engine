//! Native decode + canonical window diff per message, on realistic synthetic messages shaped
//! like each venue's documented format (no recorded exchange data is stored in the repo).
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::{hint::black_box, time::Duration};
use wire::{feed::Scale, window::WireFrame};
const SCALE3: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 3,
    qty_atoms: 1,
};
const SCALE_OKX: Scale = Scale {
    price_decimals: 1,
    price_atoms: 1,
    qty_decimals: 2,
    qty_atoms: 1,
};
fn levels(base: i64, step: i64, n: usize, qty: &str) -> String {
    (0..n as i64)
        .map(|i| {
            format!(
                r#"["{}.{}0","{qty}"]"#,
                (base + i * step) / 10,
                ((base + i * step) % 10).abs()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}
fn binance(u: u64, shift: i64) -> String {
    format!(
        r#"{{"e":"depthUpdate","E":1790947927342,"T":1790947927341,"s":"BTCUSDT","ps":"BTCUSDT","U":{},"u":{u},"pu":{},"b":[{}],"a":[{}]}}"#,
        u - 10,
        u - 11,
        levels(867_799 - shift, -1, 20, "0.618"),
        levels(867_800 - shift, 1, 20, "1.205")
    )
}
fn bybit(u: u64, shift: i64) -> String {
    format!(
        r#"{{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":1790947943648,"data":{{"s":"BTCUSDT","b":[{}],"a":[{}],"u":{u},"seq":{u}}},"cts":1790947943646}}"#,
        levels(867_790 - shift, -1, 6, "0.031"),
        levels(867_805 - shift, 1, 6, "0.044")
    )
}
fn okx(prev: i64, seq: i64, shift: i64) -> String {
    let lv = |base: i64, step: i64| {
        (0..6)
            .map(|i| {
                format!(
                    r#"["{}.{}","12","0","3"]"#,
                    (base + i * step) / 10,
                    ((base + i * step) % 10).abs()
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        r#"{{"arg":{{"channel":"books","instId":"BTC-USDT-SWAP"}},"action":"update","data":[{{"asks":[{}],"bids":[{}],"ts":"1790947946818","checksum":0,"prevSeqId":{prev},"seqId":{seq}}}]}}"#,
        lv(867_805 - shift, 1),
        lv(867_790 - shift, -1)
    )
}
fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_and_window_diff");
    group.throughput(Throughput::Elements(1));
    // Binance: complete top-20 snapshots alternating between two price shifts.
    let mut d = venue_binance::BinanceFutures::<64>::new("BTCUSDT", SCALE3, 20).unwrap();
    let mut f = WireFrame::new(1);
    let msgs: Vec<String> = (0..2_000_000_u64)
        .step_by(1_000)
        .map(|u| binance(1_000_000 + u, (u / 1_000 % 2) as i64))
        .collect();
    d.decode(msgs[0].as_bytes(), &mut f).unwrap();
    let mut i = 1;
    group.bench_function("binance_depth20_snapshot_message", |b| {
        b.iter(|| {
            let m = &msgs[i % msgs.len()];
            i += 1;
            black_box(d.decode(black_box(m.as_bytes()), &mut f).ok())
        })
    });
    // Bybit: 10,000 pre-built contiguous deltas of 12 levels; the snapshot is re-sent once per
    // cycle (1 in 10,000 iterations) so continuity holds without building strings in the loop.
    let mut d = venue_bybit::BybitLinear::<256>::new("BTCUSDT", SCALE3, 20).unwrap();
    let snapshot = bybit(1, 0).replace("\"delta\"", "\"snapshot\"");
    let deltas: Vec<String> = (2..10_002).map(|u| bybit(u, (u % 2) as i64)).collect();
    let mut k = 0;
    group.bench_function("bybit_delta_12_levels", |b| {
        b.iter(|| {
            if k % deltas.len() == 0 {
                d.decode(snapshot.as_bytes(), &mut f).unwrap();
            }
            let m = &deltas[k % deltas.len()];
            k += 1;
            black_box(d.decode(black_box(m.as_bytes()), &mut f).ok())
        })
    });
    // OKX: the same scheme with linked prevSeqId/seqId updates.
    let mut d = venue_okx::OkxSwap::<1024>::new("BTC-USDT-SWAP", SCALE_OKX, 20).unwrap();
    let snapshot = okx(-1, 1, 0).replace("\"update\"", "\"snapshot\"");
    let updates: Vec<String> = (2..10_002).map(|q| okx(q - 1, q, q % 2)).collect();
    let mut k = 0;
    group.bench_function("okx_books_update_12_levels", |b| {
        b.iter(|| {
            if k % updates.len() == 0 {
                d.decode(snapshot.as_bytes(), &mut f).unwrap();
            }
            let m = &updates[k % updates.len()];
            k += 1;
            black_box(d.decode(black_box(m.as_bytes()), &mut f).ok())
        })
    });
    group.finish();
    let mut d = venue_bybit::BybitLinear::<256>::new("BTCUSDT", SCALE3, 20).unwrap();
    let trades = format!(
        r#"{{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1790947943700,"data":[{}]}}"#,
        (0..5)
            .map(|i| format!(r#"{{"T":1790947943699,"s":"BTCUSDT","S":"Buy","v":"0.010","p":"86781.{i}","L":"PlusTick","i":"x","BT":false,"seq":5}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    c.bench_function("bybit_trade_message_5_prints", |b| {
        b.iter(|| black_box(d.decode(black_box(trades.as_bytes()), &mut f).ok()))
    });
}
criterion_group! {name=benches;config=Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));targets=bench}
criterion_main!(benches);
