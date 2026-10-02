//! Isolated measurement executable. Unsafe is ONLY a System allocator pass-through.
#![allow(unsafe_code)]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};
mod support;
use support::{Driver, Large, Small, setup};
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
fn guarded<T>(f: impl FnOnce() -> T) -> (T, u64) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let value = f();
    ENABLED.store(false, Ordering::Relaxed);
    (value, ALLOCATIONS.load(Ordering::Relaxed))
}
/// 100,000 individually timed commands; sample storage is allocated outside the guard.
fn percentiles<const L: usize, const E: usize>(
    name: &str,
    ledger: &mut inventory::Ledger<3, L, E>,
    d: &mut Driver,
    next: fn(&mut Driver) -> inventory::InventoryEvent,
) {
    let mut samples = vec![0_u64; 100_000];
    let ((), allocations) = guarded(|| {
        for sample in &mut samples {
            let event = next(d);
            let start = Instant::now();
            black_box(ledger.apply(black_box(event)).unwrap());
            *sample = start.elapsed().as_nanos() as u64;
        }
    });
    assert_eq!(allocations, 0);
    samples.sort_unstable();
    println!(
        "{name} latency ns including timer/barrier: p50={} p90={} p95={} p99={} p99.9={} max={}",
        samples[49_999],
        samples[89_999],
        samples[94_999],
        samples[98_999],
        samples[99_899],
        samples[99_999]
    );
}
fn main() {
    println!(
        "Ledger sizes: <3 venues, 32 slots, 8 episodes>={} bytes (rollback image {}); <3, 256, 128>={} bytes (rollback image {}). History is committed after success and never copied.",
        size_of::<Small>(),
        Small::rollback_image_bytes(),
        size_of::<Large>(),
        Large::rollback_image_bytes()
    );
    let ((mut ledger, mut d), startup) = guarded(setup::<32, 8>);
    assert_eq!(startup, 0);
    println!(
        "Startup: zero heap allocations (fixed inline arrays). Six resting children, gross 6, net 0, one active episode."
    );
    for _ in 0..100_000 {
        ledger.apply(d.mixed()).unwrap();
    }
    println!(
        "Mixed workload: four round-robin venue marks per recycle-cycle command; 1 ns timestamp increments; risk thresholds disabled."
    );
    for count in [1_u64, 10_000, 1_000_000] {
        let (elapsed, allocations) = guarded(|| {
            let start = Instant::now();
            for _ in 0..count {
                black_box(ledger.apply(black_box(d.mixed())).unwrap());
            }
            start.elapsed()
        });
        assert_eq!(allocations, 0);
        println!(
            "commands={count} elapsed_ns={} ns/op={:.2} throughput/s={:.0} allocations/op=0",
            elapsed.as_nanos(),
            elapsed.as_nanos() as f64 / count as f64,
            count as f64 / elapsed.as_secs_f64()
        );
    }
    percentiles("mixed_32_slots", &mut ledger, &mut d, Driver::mixed);
    percentiles("mark_32_slots", &mut ledger, &mut d, Driver::mark);
    percentiles("cycle_32_slots", &mut ledger, &mut d, Driver::cycle);
    let (mut large, mut d) = setup::<256, 128>();
    for _ in 0..100_000 {
        large.apply(d.mixed()).unwrap();
    }
    percentiles("mixed_256_slots", &mut large, &mut d, Driver::mixed);
    let (completed, _, _) = ledger.episode_totals();
    assert_eq!(completed, 0);
    assert!(ledger.halt_reason().is_none() && large.halt_reason().is_none());
    println!(
        "No halt and no completed episode during measurement; zero allocations in every guarded section."
    );
}
