use common::{Side, Timestamp};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use fixed_point::PriceTicks;
use liquidity::{LiquidityEngine, grid::PriceGrid};
use std::{hint::black_box, time::Duration};
use voids::{FormationEvidence, PriceRegion, VoidEngine};
mod support;
fn bench(c: &mut Criterion) {
    let mut grid = PriceGrid::<3, 64>::new(PriceTicks(80), 4, [1_000_000; 3]).unwrap();
    let mut now = 0;
    c.bench_function("liquidity_grid_update_64_ticks", |b| {
        b.iter(|| {
            now += 1;
            black_box(
                grid.set(
                    0,
                    Side::Buy,
                    PriceTicks(100),
                    black_box(100 + (now % 19) as i64),
                    Timestamp(now),
                )
                .unwrap(),
            );
            black_box(&grid);
        });
    });
    let mut liquidity = LiquidityEngine::<3, 64, 3>::new(support::lc(), [1_000_000; 3]).unwrap();
    for v in 0..3 {
        liquidity
            .update_level(v, Side::Buy, PriceTicks(100), 100, Timestamp(0))
            .unwrap();
    }
    let mut now = 0;
    c.bench_function("liquidity_sample_16_cells_3_venues", |b| {
        b.iter(|| {
            now += 1_000_000;
            liquidity
                .sample(Timestamp(now), black_box(&[7; 16]), 250_000)
                .unwrap();
            black_box(&liquidity);
        });
    });
    let mut voids = VoidEngine::<16>::new(support::vc()).unwrap();
    voids
        .consider(
            Timestamp(0),
            PriceRegion {
                lower: PriceTicks(100),
                upper: PriceTicks(108),
            },
            FormationEvidence {
                depth: 0,
                baseline: 100,
                score_ppm: 1_000_000,
                venue_mask: 7,
                eligible_mask: 7,
            },
            208,
        )
        .unwrap();
    voids
        .observe(0, Timestamp(3_000_000), 220, 0, true)
        .unwrap();
    let mut now = 3_000_000;
    c.bench_function("void_revisit_update", |b| {
        b.iter(|| {
            now += 1;
            let price = if now % 2 == 0 { 216 } else { 220 };
            voids
                .observe(0, Timestamp(now), black_box(price), 0, true)
                .unwrap();
            black_box(&voids);
        });
    });
    let mut group = c.benchmark_group("research_3_venues_16_cells_1_zone");
    group.throughput(Throughput::Elements(1));
    for sampled in [false, true] {
        let (mut engine, mut now, mut sequences) = support::setup();
        group.bench_function(
            if sampled {
                "every_event_sampled"
            } else {
                "1ns_events_1ms_sample_cadence"
            },
            |b| {
                b.iter(|| {
                    if sampled {
                        now += 999_999;
                    }
                    let event = support::advance(&mut now, &mut sequences);
                    engine.apply(black_box(&event)).unwrap();
                    black_box(&engine);
                });
            },
        );
    }
    group.finish();
}
criterion_group! {name=benches;config=Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));targets=bench}
criterion_main!(benches);
