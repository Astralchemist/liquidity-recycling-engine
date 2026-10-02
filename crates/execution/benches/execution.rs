//! Phase 9 primitives: queue updates, markout sampling and fee cost, on pre-built inputs.
use common::Side;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use execution::{
    fees::FeeSchedule,
    markout::{MarkoutConfig, MarkoutTracker},
    queue::{CancelModel, QueuedOrder},
};
use fixed_point::{PriceTicks, QtyUnits};
use std::{hint::black_box, time::Duration};
#[derive(Clone, Copy)]
enum Op {
    Trade(i64, i64),
    Depth(i64, i64),
}
/// 1,024 deterministic queue operations at one level: prints and depth changes.
fn ops() -> Vec<Op> {
    let mut state = 7_u64;
    let mut draw = |n: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    let mut shown = 500_i64;
    (0..1_024)
        .map(|_| {
            if draw(3) == 0 {
                Op::Trade(100 - i64::from(draw(20) == 0), 1 + draw(30) as i64)
            } else {
                let new = (shown + draw(80) as i64 - 40).max(0);
                let op = Op::Depth(shown, new);
                shown = new;
                op
            }
        })
        .collect()
}
fn bench(c: &mut Criterion) {
    let ops = ops();
    let mut group = c.benchmark_group("queue");
    group.throughput(Throughput::Elements(ops.len() as u64));
    for model in [
        CancelModel::Pessimistic,
        CancelModel::Proportional,
        CancelModel::Optimistic,
    ] {
        group.bench_function(format!("{model:?}"), |b| {
            b.iter(|| {
                let mut o = QueuedOrder::place(Side::Buy, 100, 1_000_000, 500);
                for op in &ops {
                    match *op {
                        Op::Trade(price, qty) => {
                            black_box(o.on_trade(Side::Sell, price, qty));
                        }
                        Op::Depth(old, new) => o.on_depth(old, new, model),
                    }
                }
                black_box(o)
            })
        });
    }
    group.finish();
    // A fill every 10 ms, observed every 100 us, over the ten §18 horizons: about 500 fills
    // are pending at the 5 s horizon, as at a few fills per second in the live fill study.
    let mut group = c.benchmark_group("markout");
    group.throughput(Throughput::Elements(102_400));
    group.bench_function("record_and_observe", |b| {
        b.iter(|| {
            let mut t = Box::new(MarkoutTracker::<1024>::new(MarkoutConfig::spec()));
            for i in 0..102_400_u64 {
                let now = i * 100_000;
                if i % 100 == 0 {
                    let side = if i % 200 == 0 { Side::Buy } else { Side::Sell };
                    t.record(now, side, 867_800, 10, Some(1_735_601));
                }
                t.observe(now, Some(1_735_600 + (i % 7) as i128));
            }
            assert_eq!(t.overflows, 0);
            black_box(t.stats()[9])
        })
    });
    group.finish();
    let schedule = FeeSchedule {
        maker_fee_ppm: 200,
        maker_rebate_ppm: 50,
        taker_fee_ppm: 500,
    };
    c.bench_function("fee_cost", |b| {
        b.iter(|| {
            black_box(schedule.cost(
                black_box(PriceTicks(867_801)),
                black_box(QtyUnits(10)),
                black_box(true),
            ))
        })
    });
}
criterion_group! {name=benches;config=Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));targets=bench}
criterion_main!(benches);
