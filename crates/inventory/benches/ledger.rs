use common::Timestamp;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use fixed_point::{Money, PriceTicks};
use inventory::{Charges, InventoryEvent, InventoryEventKind, journal};
use std::{hint::black_box, time::Duration};
mod support;
use support::setup;
fn bench(c: &mut Criterion) {
    let (mut ledger, mut d) = setup::<32, 8>();
    c.bench_function("inventory_mark_6_children_32_slots", |b| {
        b.iter(|| {
            black_box(ledger.apply(black_box(d.mark())).unwrap());
        })
    });
    let (mut ledger, mut d) = setup::<32, 8>();
    c.bench_function("inventory_tick_32_slots", |b| {
        b.iter(|| {
            black_box(ledger.apply(black_box(d.tick())).unwrap());
        })
    });
    let (mut ledger, mut d) = setup::<32, 8>();
    let mut group = c.benchmark_group("inventory_recycle_cycle_32_slots");
    group.throughput(Throughput::Elements(4));
    group.bench_function("reserve_fill_close_fill", |b| {
        b.iter(|| {
            for _ in 0..4 {
                black_box(ledger.apply(black_box(d.cycle())).unwrap());
            }
        })
    });
    group.finish();
    // Larger capacities show the cost of slot scans and the whole-ledger transaction copy.
    let (mut ledger, mut d) = setup::<256, 128>();
    c.bench_function("inventory_mark_6_children_256_slots", |b| {
        b.iter(|| {
            black_box(ledger.apply(black_box(d.mark())).unwrap());
        })
    });
    let (mut ledger, mut d) = setup::<256, 128>();
    c.bench_function("inventory_tick_256_slots", |b| {
        b.iter(|| {
            black_box(ledger.apply(black_box(d.tick())).unwrap());
        })
    });
    let event = InventoryEvent {
        sequence: 9,
        timestamp: Timestamp(33),
        kind: InventoryEventKind::FillClose {
            id: 7,
            price: PriceTicks(101),
            maker: true,
            charges: Charges {
                rebate: Money(1),
                fee: Money(2),
                slippage: Money(3),
            },
        },
    };
    c.bench_function("inventory_journal_encode_decode", |b| {
        b.iter(|| black_box(journal::decode(journal::encode(black_box(event))).unwrap()))
    });
}
criterion_group! {name=benches;config=Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));targets=bench}
criterion_main!(benches);
