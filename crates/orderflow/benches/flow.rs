use book::Level;
use common::{Side, Timestamp};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use fixed_point::{PriceTicks, QtyUnits};
use orderflow::*;
use std::{hint::black_box, time::Duration};
mod support;
fn bench(c: &mut Criterion) {
    let previous = BestQuotes {
        bid: Level {
            price: PriceTicks(100),
            qty: QtyUnits(100),
        },
        ask: Level {
            price: PriceTicks(104),
            qty: QtyUnits(200),
        },
    };
    let current = BestQuotes {
        bid: Level {
            price: PriceTicks(101),
            qty: QtyUnits(150),
        },
        ..previous
    };
    c.bench_function("best_quote_ofi", |b| {
        b.iter(|| black_box(best_quote_ofi(black_box(previous), black_box(current)).unwrap()))
    });
    c.bench_function("queue_imbalance_integer", |b| {
        b.iter(|| black_box(queue_imbalance(black_box(100), black_box(250)).unwrap()))
    });
    c.bench_function("poisson_event_probability_integer", |b| {
        b.iter(|| {
            black_box(poisson_event_probability_ppm(
                black_box(25_000_000),
                black_box(10_000_000),
            ))
        })
    });
    let mut flow = FlowEngine::<128, 3>::new(support::fc(), Timestamp(0)).unwrap();
    let mut now = 0;
    c.bench_function("flow_two_windows_128_capacity", |b| {
        b.iter(|| {
            now += 1;
            let delta = (now % 21) as i128 - 10;
            let contribution =
                Contribution::depth(delta, Side::Buy, delta, RemovalAttribution::Unknown).unwrap();
            flow.record(
                Timestamp(now),
                black_box(contribution),
                Some((now % 3) as usize),
            )
            .unwrap();
            black_box(&flow);
        })
    });
    let (mut e, mut now, mut seq) = support::setup();
    let mut group = c.benchmark_group("research_with_flow_3_venues");
    group.throughput(Throughput::Elements(1));
    group.bench_function("modify", |b| {
        b.iter(|| {
            let event = support::advance(&mut now, &mut seq);
            e.apply(black_box(&event)).unwrap();
            black_box(&e);
        })
    });
    group.finish();
}
criterion_group! {name=benches;config=Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));targets=bench}
criterion_main!(benches);
