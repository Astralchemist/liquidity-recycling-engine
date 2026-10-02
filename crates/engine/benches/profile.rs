//! Isolated measurement executable. Unsafe is ONLY a System allocator pass-through.
#![allow(unsafe_code)]
use engine::fixtures::*;
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
fn main() {
    println!(
        "Engine size {} bytes (boxed). Six synthetic scenarios, fresh engine each, 1 ms event spacing.",
        size_of::<SyntheticEngine>()
    );
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let probe = Box::new(build().unwrap());
    ENABLED.store(false, Ordering::Relaxed);
    println!(
        "Startup allocations: {} (engine box, research box, six fixed flow buffers, one batch scratch).",
        ALLOCATIONS.load(Ordering::Relaxed)
    );
    drop(probe);
    let mut all = Vec::new();
    let mut quiet = Vec::new();
    let mut active = Vec::new();
    let mut tagged = Vec::new();
    for scenario in Scenario::ALL {
        let events = generate(scenario);
        let mut engine = Box::new(build().unwrap());
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
            "scenario={} events={} commands={} elapsed_ns={} ns/event={:.1} allocations=0",
            scenario.letter(),
            events.len(),
            engine.metrics().commands,
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
        "slowest {} events: {:?}",
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
            "{name} n={} latency ns including timer: p50={} p90={} p99={} p99.9={} max={}",
            samples.len(),
            q(0.5),
            q(0.9),
            q(0.99),
            q(0.999),
            samples[samples.len() - 1]
        );
    }
}
