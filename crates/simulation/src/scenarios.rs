//! Specification §39 scenarios A–F as explicit market scripts. Every price path, depth band
//! and print is stated here; nothing is random. Outcomes are asserted by engine tests.
use crate::{
    Scenario,
    market::{ALL, Shape, SyntheticMarket},
};
use common::{InstrumentId, VenueId};
use market_events::MarketEvent;
/// Common grid corridor [80, 143], 100 units per level, 1 ms per event.
pub const SHAPE: Shape = Shape {
    low: 80,
    high: 143,
    base_qty: 100,
    step_ns: 1_000_000,
};
/// The void band: exactly one Phase 4 cell `[100, 103]` with `lower_price = 80`, width 4.
pub const VOID: (i64, i64) = (100, 103);
pub const VOID_QTY: i64 = 5;
/// Stable book, void forms, price traverses and exits below, stays away, then revisits.
fn prelude(void_mask: u8) -> SyntheticMarket {
    let mut m = SyntheticMarket::new(
        SHAPE,
        [VenueId(1), VenueId(2), VenueId(3)],
        [InstrumentId(101), InstrumentId(202), InstrumentId(303)],
        118,
    );
    m.snapshot();
    m.oscillate(ALL, 1, 4);
    m.set_band(VOID.0, VOID.1, void_mask, VOID_QTY);
    m.idle(ALL, 5);
    m.down(ALL, 22); // 118 -> 96: inside [100,103], then exits below: registration.
    m.oscillate(ALL, 1, 6);
    m.up(ALL, 5); // 96 -> 101: lower-boundary touch, then partial penetration.
    m
}
/// Quiet two-way trading inside the zone, then a calm exit upwards.
fn oscillation(m: &mut SyntheticMarket, mask: u8, cycles: usize) {
    for _ in 0..cycles {
        m.up(mask, 2);
        m.idle(ALL, 2);
        m.down(mask, 3);
        m.idle(ALL, 2);
        m.up(mask, 1);
    }
}
/// Choppy drift: `steps` cycles of two ticks forward, one back. Efficiency 1/3, not a trend.
fn drift_up(m: &mut SyntheticMarket, steps: usize) {
    for _ in 0..steps {
        m.up(ALL, 2);
        m.idle(ALL, 2);
        m.down(ALL, 1);
        m.idle(ALL, 2);
    }
}
/// Venue 1 moves one tick first; venues 2 and 3 follow after a pause. Never more than one tick
/// apart, so the consolidated book stays open (a two-tick lead would lock a two-tick spread).
fn staircase(m: &mut SyntheticMarket, up: bool, ticks: usize) {
    for _ in 0..ticks {
        if up {
            m.up(0b001, 1);
            m.idle(ALL, 2);
            m.up(0b110, 1);
        } else {
            m.down(0b001, 1);
            m.idle(ALL, 2);
            m.down(0b110, 1);
        }
        m.idle(ALL, 1);
    }
}
pub fn generate(scenario: Scenario) -> Vec<MarketEvent> {
    match scenario {
        // A: stable book -> void forms -> crosses -> leaves -> revisits -> oscillates.
        Scenario::RevisitOscillation => {
            let mut m = prelude(ALL);
            oscillation(&mut m, ALL, 12);
            drift_up(&mut m, 5);
            m.oscillate(ALL, 2, 30);
            m.events()
        }
        // B: void forms -> revisit -> violent one-way continuation upwards.
        Scenario::OneWayContinuation => {
            let mut m = prelude(ALL);
            oscillation(&mut m, ALL, 4);
            m.up(ALL, 28);
            m.idle(ALL, 30);
            m.events()
        }
        // C: the band thins on venue 1 only; venues 2 and 3 stay liquid.
        Scenario::LocalVoid => {
            let mut m = prelude(0b001);
            oscillation(&mut m, ALL, 12);
            drift_up(&mut m, 5);
            m.oscillate(ALL, 2, 30);
            m.events()
        }
        // D: market-wide void; venue 1 leads the others by one tick during the revisit.
        Scenario::GlobalVoid => {
            let mut m = prelude(ALL);
            for _ in 0..12 {
                staircase(&mut m, true, 2);
                staircase(&mut m, false, 3);
                staircase(&mut m, true, 1);
            }
            drift_up(&mut m, 5);
            m.oscillate(ALL, 2, 30);
            m.events()
        }
        // E: toxic sell sweep fills resting bids, liquidity replenishes, two-way recovery.
        Scenario::ToxicRecovery => {
            let mut m = prelude(ALL);
            oscillation(&mut m, ALL, 3);
            m.sell(ALL, 5);
            m.idle(ALL, 3);
            m.refill_asks(ALL, 5);
            m.idle(ALL, 10);
            oscillation(&mut m, ALL, 2);
            m.up(ALL, 6);
            oscillation(&mut m, ALL, 8);
            drift_up(&mut m, 5);
            m.oscillate(ALL, 2, 30);
            m.events()
        }
        // F: toxic sweep with no replenishment; bids keep draining until risk exits.
        Scenario::EmergencyExit => {
            let mut m = prelude(ALL);
            oscillation(&mut m, ALL, 3);
            m.sell(ALL, 5);
            for _ in 0..6 {
                m.idle(ALL, 3);
                m.sell(ALL, 2);
            }
            m.idle(ALL, 30);
            m.events()
        }
    }
}
