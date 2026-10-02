//! Isolated measurement executable. Unsafe is ONLY a System allocator pass-through.
#![allow(unsafe_code)]
use common::Side;
use fixed_point::{PriceTicks, QtyUnits};
use market_events::MarketEventType as K;
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
fn quantile(values: &[u64], numerator: usize, denominator: usize) -> u64 {
    values[(values.len() * numerator)
        .div_ceil(denominator)
        .saturating_sub(1)]
}
fn main() {
    println!(
        "Unpinned local release profile; 128 levels/side, capacity 256. Times include event preparation."
    );
    let (mut b, mut e) = support::setup();
    for _ in 0..100_000 {
        support::advance(&mut e);
        b.apply(black_box(&e)).unwrap();
    }
    for count in [1_usize, 10_000, 1_000_000] {
        ALLOCATIONS.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let start = Instant::now();
        for _ in 0..count {
            support::advance(&mut e);
            b.apply(black_box(&e)).unwrap();
        }
        black_box(&b);
        let elapsed = start.elapsed();
        ENABLED.store(false, Ordering::Relaxed);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(allocations, 0);
        println!(
            "updates={count} elapsed_ns={} ns/op={:.2} throughput/s={:.0} allocations/op={:.1}",
            elapsed.as_nanos(),
            elapsed.as_nanos() as f64 / count as f64,
            count as f64 / elapsed.as_secs_f64(),
            allocations as f64 / count as f64
        );
    }
    let mut samples = vec![0_u64; 100_000];
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    for sample in &mut samples {
        support::advance(&mut e);
        let start = Instant::now();
        b.apply(black_box(&e)).unwrap();
        black_box(&b);
        *sample = start.elapsed().as_nanos() as u64;
    }
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    samples.sort_unstable();
    println!(
        "single-call latency ns (includes timer overhead): p50={} p90={} p95={} p99={} p99.9={} max={}",
        quantile(&samples, 50, 100),
        quantile(&samples, 90, 100),
        quantile(&samples, 95, 100),
        quantile(&samples, 99, 100),
        quantile(&samples, 999, 1000),
        samples.last().unwrap()
    );
    // Exercise insertion, shifting and cancellation within the same allocation guard.
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    for _ in 0..100_000 {
        e.sequence += 1;
        e.receive_ts.0 += 1;
        e.event_type = K::Add;
        e.side = Side::Buy;
        e.price_ticks = PriceTicks(100_001);
        e.qty_units = QtyUnits(10);
        b.apply(black_box(&e)).unwrap();
        black_box(&b);
        e.sequence += 1;
        e.receive_ts.0 += 1;
        e.event_type = K::Cancel;
        e.qty_units = QtyUnits(0);
        b.apply(black_box(&e)).unwrap();
        black_box(&b);
    }
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    println!("insert/cancel: 200000 operations, zero allocations");
    // Snapshot reset, staging and commit also must not allocate.
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let mut snapshot_book =
        book::VenueBook::<256>::new(common::VenueId(1), common::InstrumentId(1));
    for e in [
        support::event(1, K::SnapshotStart, Side::Buy, 0, 0),
        support::event(2, K::Add, Side::Buy, 99, 1),
        support::event(3, K::SnapshotEnd, Side::Buy, 0, 0),
    ] {
        snapshot_book.apply(black_box(&e)).unwrap();
        black_box(&snapshot_book);
    }
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    println!("snapshot construction/staging/commit: zero allocations");
}
