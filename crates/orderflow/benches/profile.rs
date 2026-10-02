//! Isolated measurement executable. Unsafe is ONLY a System allocator pass-through.
#![allow(unsafe_code)]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};
mod support;
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
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let (mut engine, mut now, mut sequences) = support::setup();
    ENABLED.store(false, Ordering::Relaxed);
    let startup_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(startup_allocations, 7);
    println!(
        "Three books (capacity 128/side, 3-4 occupied), 64 ticks, 16 cells, one active zone: startup allocates six fixed flow buffers and one batch scratch buffer. No buffer growth during processing."
    );
    for _ in 0..100_000 {
        engine
            .apply(&support::advance(&mut now, &mut sequences))
            .unwrap();
    }
    println!(
        "Steady events: 1 ns increments, 1 ms formation samples, 16-event / 32 ns flow windows, 128 slots per window."
    );
    for count in [1_u64, 10_000, 1_000_000] {
        ALLOCATIONS.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let start = Instant::now();
        for _ in 0..count {
            engine
                .apply(black_box(&support::advance(&mut now, &mut sequences)))
                .unwrap();
        }
        black_box(&engine);
        let elapsed = start.elapsed();
        ENABLED.store(false, Ordering::Relaxed);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(allocations, 0);
        println!(
            "updates={count} elapsed_ns={} ns/op={:.2} throughput/s={:.0} allocations/op=0",
            elapsed.as_nanos(),
            elapsed.as_nanos() as f64 / count as f64,
            count as f64 / elapsed.as_secs_f64()
        );
    }
    let mut samples = vec![0_u64; 100_000];
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    for sample in &mut samples {
        let e = support::advance(&mut now, &mut sequences);
        let start = Instant::now();
        engine.apply(black_box(&e)).unwrap();
        black_box(&engine);
        *sample = start.elapsed().as_nanos() as u64;
    }
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    samples.sort_unstable();
    println!(
        "latency ns including timer/barrier: p50={} p90={} p95={} p99={} p99.9={} max={}",
        samples[49999],
        samples[89999],
        samples[94999],
        samples[98999],
        samples[99899],
        samples[99999]
    );
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    now += 1_000_000;
    engine.advance_time(common::Timestamp(now)).unwrap();
    let mut event = support::advance(&mut now, &mut sequences);
    event.event_type = market_events::MarketEventType::SnapshotStart;
    event.price_ticks = fixed_point::PriceTicks(0);
    event.qty_units = fixed_point::QtyUnits(0);
    engine.apply(&event).unwrap();
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    println!("Idle expiry and snapshot-triggered flow reset: zero allocations.");
}
