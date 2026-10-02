use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use engine::{FillModel, FundingConfig, ObjectiveConfig, fixtures::*};
use execution::{fees::FeeSchedule, queue::CancelModel};
use orderflow::research::FlowResearchEngine;
use simulation::{Scenario, scenarios::generate};
use std::{hint::black_box, time::Duration};
type Research = FlowResearchEngine<3, 128, 384, 64, 6, 16, 128>;
fn research() -> Box<Research> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    Box::new(Research::new(grid, venues, liquidity, voids, flow()).unwrap())
}
/// Phase 9 configuration: proportional queue model, retail ppm fees, funding and J(a).
pub fn phase9() -> engine::EngineConfig {
    let mut c = engine();
    c.fills.model = FillModel::Queue(CancelModel::Proportional);
    c.fills.schedule = FeeSchedule {
        maker_fee_ppm: 200,
        maker_rebate_ppm: 0,
        taker_fee_ppm: 500,
    };
    c.funding = FundingConfig {
        rate_ppm: 100,
        interval_ns: 100_000_000,
    };
    c.objective = Some(ObjectiveConfig {
        w_rebate: 0,
        w_spread: 1_000,
        w_rebalance: 1_000,
        w_adverse: 1_000,
        w_inventory: 1_000,
        w_queue: 1_000,
        adverse_horizon: 5,
        adverse_min_samples: 5,
        adverse_prior_x2: 1,
        inventory_risk_x2: 1,
        queue_cost_x2: 0,
        min_edge_x2: -1_000,
    });
    c
}
fn build_phase9() -> Box<SyntheticEngine> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    Box::new(
        SyntheticEngine::new(
            grid,
            venues,
            liquidity,
            voids,
            flow(),
            inventory(),
            phase9(),
        )
        .unwrap(),
    )
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
        // Phase 9 path: queue tracking around depth changes, ppm fees, funding, markouts and
        // the J(a) gate on every entry decision.
        group.bench_function("engine_phase9", |b| {
            b.iter_batched(
                build_phase9,
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
