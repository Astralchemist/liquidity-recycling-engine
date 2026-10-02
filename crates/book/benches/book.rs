use common::Side;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::MarketEventType as K;
use std::{hint::black_box, time::Duration};
mod support;
fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("book_modify_128_levels");
    for count in [1_u64, 10_000, 1_000_000] {
        let (mut b, mut e) = support::setup();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &count,
            |bencher, &count| {
                bencher.iter(|| {
                    for _ in 0..count {
                        support::advance(&mut e);
                        b.apply(black_box(&e)).unwrap();
                    }
                    black_box(&b);
                });
            },
        );
    }
    group.finish();
    let (mut batch_book, mut batch_event) = support::setup();
    c.bench_function("book_atomic_two_level_modify_128_levels", |bencher| {
        bencher.iter(|| {
            batch_event.sequence += 1;
            batch_event.receive_ts.0 += 1;
            batch_event.exchange_sequence += 1;
            batch_event.price_ticks = PriceTicks(100_000);
            batch_event.qty_units = QtyUnits(100 + (batch_event.sequence % 7) as i64);
            let first = batch_event;
            batch_event.sequence += 1;
            batch_event.price_ticks = PriceTicks(99_999);
            batch_book
                .apply_depth_batch(black_box(&[first, batch_event]))
                .unwrap();
            black_box(&batch_book);
        });
    });
    let mut native = market_events::NativeSequenceTracker::default();
    native.reset_from_snapshot(1);
    let mut native_id = 1_u64;
    c.bench_function("native_range_validation", |bencher| {
        bencher.iter(|| {
            native_id += 1;
            native
                .accept(black_box(market_events::NativeUpdateRange {
                    first: native_id,
                    last: native_id,
                    previous_last: Some(native_id - 1),
                }))
                .unwrap();
            black_box(&native);
        });
    });
    let (mut b, mut e) = support::setup();
    c.bench_function("book_insert_cancel_top_128_levels", |bencher| {
        bencher.iter(|| {
            e.sequence += 1;
            e.receive_ts.0 += 1;
            e.event_type = K::Add;
            e.price_ticks = PriceTicks(100_001);
            e.qty_units = QtyUnits(10);
            b.apply(black_box(&e)).unwrap();
            black_box(&b);
            e.sequence += 1;
            e.receive_ts.0 += 1;
            e.event_type = K::Cancel;
            e.qty_units = QtyUnits(0);
            b.apply(black_box(&e)).unwrap();
            black_box(&b);
        })
    });
    c.bench_function("integer_pnl", |bencher| {
        bencher.iter(|| {
            fixed_point::realized_pnl(
                Side::Buy,
                black_box(PriceTicks(6_174_253)),
                black_box(PriceTicks(6_174_257)),
                black_box(QtyUnits(10)),
            )
            .unwrap()
        })
    });
    let encoded = recorder::encode_event(&e);
    c.bench_function("event_encode_crc", |bencher| {
        bencher.iter(|| recorder::encode_event(black_box(&e)))
    });
    c.bench_function("event_decode_crc", |bencher| {
        bencher.iter(|| recorder::decode_event(black_box(&encoded)).unwrap())
    });
}
criterion_group! { name = benches; config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2)); targets = benchmarks }
criterion_main!(benches);
