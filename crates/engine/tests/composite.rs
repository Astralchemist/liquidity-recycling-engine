//! Live venues routinely cross each other (basis and feed latency) while each book stays valid.
//! The consolidated-midpoint reference then suspends research; the composite reference does not.
use engine::fixtures::*;
use liquidity::PriceReference;
use market_events::MarketEventType as K;
use simulation::{Scenario, scenarios::generate};
use toxicity::Environment;
mod support;
use support::*;
/// Scenario A with venue 1 quoted 3 ticks above venues 2 and 3: its bids sit above their asks.
fn shifted() -> Vec<market_events::MarketEvent> {
    generate(Scenario::RevisitOscillation)
        .into_iter()
        .map(|mut e| {
            if e.venue.0 == 1 && !matches!(e.event_type, K::SnapshotStart | K::SnapshotEnd) {
                e.price_ticks.0 += 3;
            }
            e
        })
        .collect()
}
fn run_with_reference(reference: PriceReference) -> Box<SyntheticEngine> {
    let (grid, venues) = market();
    let (mut liquidity, voids) = structures();
    liquidity.price_reference = reference;
    let mut engine = Box::new(
        SyntheticEngine::new(
            grid,
            venues,
            liquidity,
            voids,
            flow(),
            inventory(),
            engine(),
        )
        .unwrap(),
    );
    for e in shifted() {
        engine.apply(&e, &mut |_| {}).unwrap();
        check_invariants(&engine);
    }
    engine
}
#[test]
fn composite_reference_keeps_researching_a_cross_venue_crossed_market() {
    let midpoint = run_with_reference(PriceReference::Midpoint);
    let composite = run_with_reference(PriceReference::Composite);
    // The consolidated touch really is crossed after the snapshots.
    let state = midpoint
        .research()
        .research()
        .market()
        .market_state()
        .unwrap();
    assert_eq!(state, consolidator::MarketState::Crossed);
    let shock = |e: &SyntheticEngine| {
        let t = e.environment().time_in_state();
        t[Environment::LiquidityShock as usize] * 100 / t.iter().sum::<u64>()
    };
    // Midpoint reference: no void is ever registered and the environment is shock throughout.
    assert_eq!(
        midpoint.research().research().voids().metrics().registered,
        0
    );
    assert!(shock(&midpoint) > 90);
    // Midpoint research also stops sampling once the touch crosses; composite keeps sampling.
    let samples = |e: &SyntheticEngine| e.research().research().liquidity().metrics().samples;
    assert!(samples(&composite) > 20 * samples(&midpoint));
    // Composite reference: the multi-venue void registers and is revisited. Only unregistered
    // candidates on venue 1's shifted band are invalidated (ordinary lifecycle), never a
    // registered void.
    let voids = composite.research().research().voids();
    assert!(voids.metrics().registered >= 1);
    assert!(voids.metrics().revisits >= 1);
    assert!(
        voids
            .zones()
            .iter()
            .flatten()
            .filter(|z| z.state == voids::VoidState::Invalidated)
            .all(|z| z.registered_at.is_none())
    );
    assert!(shock(&composite) < 10);
    // Composite midpoint is the weight-averaged venue midpoint (weights 1, 0.75, 0.5).
    let m = composite.research().research().market();
    let mids: Vec<i128> = (1..=3)
        .map(|v| m.venue_midpoint_x2(common::VenueId(v)).unwrap().unwrap())
        .collect();
    let expected =
        (mids[0] * 1_000_000 + mids[1] * 750_000 + mids[2] * 500_000).div_euclid(2_250_000);
    assert_eq!(m.composite_midpoint_x2(None).unwrap(), Some(expected));
    assert_eq!(m.composite_spread().unwrap(), Some(2));
}
