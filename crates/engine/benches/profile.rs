//! Isolated measurement executable. Unsafe is ONLY a System allocator pass-through.
#![allow(unsafe_code)]
use engine::{FillModel, FundingConfig, ObjectiveConfig, fixtures::*};
use execution::{fees::FeeSchedule, queue::CancelModel};
use simulation::{Scenario, scenarios::generate};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};
struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
// SAFETY: forwards every request unchanged to System, with identical ownership/layout.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: caller supplied valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as System.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: pointer and layout are passed directly from the allocator caller.
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: allocations come exclusively from System with the same layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
/// Phase 9 configuration: proportional queue model, retail ppm fees, funding and J(a).
fn phase9() -> engine::EngineConfig {
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
fn build_with(config: engine::EngineConfig) -> Box<SyntheticEngine> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    Box::new(
        SyntheticEngine::new(grid, venues, liquidity, voids, flow(), inventory(), config).unwrap(),
    )
}
fn main() {
    println!(
        "Engine size {} bytes (boxed). Six synthetic scenarios, fresh engine each, 1 ms event spacing.",
        size_of::<SyntheticEngine>()
    );
    let configs = [("phase7_strict", engine()), ("phase9_queue", phase9())];
    // Unmeasured warm-up of both configurations, so neither is timed on a cold machine.
    for (_, config) in configs {
        for scenario in Scenario::ALL {
            let mut engine = build_with(config);
            for e in &generate(scenario) {
                engine.apply(e, &mut |_| {}).unwrap();
            }
            black_box(&engine);
        }
    }
    for (label, config) in configs {
        profile(label, config);
    }
}
fn profile(label: &str, config: engine::EngineConfig) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let probe = build_with(config);
    ENABLED.store(false, Ordering::Relaxed);
    println!(
        "config={label} startup allocations: {} (engine box, research box, markout ring, six fixed flow buffers, one batch scratch).",
        ALLOCATIONS.load(Ordering::Relaxed)
    );
    drop(probe);
    let mut all = Vec::new();
    let mut quiet = Vec::new();
    let mut active = Vec::new();
    let mut tagged = Vec::new();
    for scenario in Scenario::ALL {
        let events = generate(scenario);
        let mut engine = build_with(config);
        let mut samples = vec![(0_u64, 0_u64); events.len()];
        ALLOCATIONS.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let start = Instant::now();
        for (e, sample) in events.iter().zip(samples.iter_mut()) {
            let mut commands = 0_u64;
            let t = Instant::now();
            engine.apply(black_box(e), &mut |_| commands += 1).unwrap();
            black_box(&engine);
            *sample = (t.elapsed().as_nanos() as u64, commands);
        }
        let elapsed = start.elapsed();
        ENABLED.store(false, Ordering::Relaxed);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(allocations, 0);
        println!(
            "config={label} scenario={} events={} commands={} maker_fills={} elapsed_ns={} ns/event={:.1} allocations=0",
            scenario.letter(),
            events.len(),
            engine.metrics().commands,
            engine.metrics().maker_fills,
            elapsed.as_nanos(),
            elapsed.as_nanos() as f64 / events.len() as f64
        );
        for (e, (ns, commands)) in events.iter().zip(samples) {
            tagged.push((ns, e.event_type, commands));
            all.push(ns);
            if commands == 0 {
                quiet.push(ns)
            } else {
                active.push(ns)
            }
        }
    }
    // Which events form the tail: the slowest 0.1% by canonical type and command count.
    tagged.sort_unstable_by_key(|t| std::cmp::Reverse(t.0));
    let tail = &tagged[..tagged.len().div_ceil(1000)];
    println!(
        "config={label} slowest {} events: {:?}",
        tail.len(),
        tail.iter().map(|t| (t.0, t.1, t.2)).collect::<Vec<_>>()
    );
    for (name, mut samples) in [
        ("all_events", all),
        ("no_command", quiet),
        ("with_commands", active),
    ] {
        samples.sort_unstable();
        let q = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
        println!(
            "config={label} {name} n={} latency ns including timer: p50={} p90={} p99={} p99.9={} max={}",
            samples.len(),
            q(0.5),
            q(0.9),
            q(0.99),
            q(0.999),
            samples[samples.len() - 1]
        );
    }
}
