use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use engine::fixtures::*;
use orderflow::research::FlowResearchEngine;
use simulation::{Scenario, scenarios::generate};
use std::{hint::black_box, time::Duration};
type Research = FlowResearchEngine<3, 128, 384, 64, 6, 16, 128>;
fn research() -> Box<Research> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    Box::new(Research::new(grid, venues, liquidity, voids, flow()).unwrap())
}
fn bench(c: &mut Criterion) {
    for scenario in [Scenario::RevisitOscillation, Scenario::ToxicRecovery] {
        let events = generate(scenario);
        let mut group = c.benchmark_group(format!("scenario_{}", scenario.letter()));
        group.throughput(Throughput::Elements(events.len() as u64));
        // Phase 5 shared research path alone: the baseline Phase 7 builds on.
        group.bench_function("research_only", |b| {
            b.iter_batched(
                research,
                |mut r| {
                    for e in &events {
                        r.apply(black_box(e)).unwrap();
                    }
                    r
                },
                BatchSize::LargeInput,
            )
        });
        // Complete Phase 7 path: research, environment, marks, fills, policy, ledger, kill.
        group.bench_function("engine", |b| {
            b.iter_batched(
                || Box::new(build().unwrap()),
                |mut engine| {
                    let mut commands = 0_u64;
                    for e in &events {
                        engine.apply(black_box(e), &mut |_| commands += 1).unwrap();
                    }
                    black_box(commands);
                    engine
                },
                BatchSize::LargeInput,
            )
        });
        group.finish();
    }
}
criterion_group! {name=benches;config=Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));targets=bench}
criterion_main!(benches);
