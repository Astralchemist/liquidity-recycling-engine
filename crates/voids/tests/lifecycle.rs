use common::Timestamp;
use fixed_point::PriceTicks;
use voids::*;
fn cfg() -> VoidConfig {
    VoidConfig {
        minimum_width_ticks: 4,
        minimum_persistence_ns: 10,
        maximum_age_ns: 100,
        low_depth_ppm: 250_000,
        refill_depth_ppm: 800_000,
        minimum_score_ppm: 750_000,
        coverage_loss: voids::CoverageLoss::Invalidate,
    }
}
fn region() -> PriceRegion {
    PriceRegion {
        lower: PriceTicks(100),
        upper: PriceTicks(108),
    }
}
fn evidence() -> FormationEvidence {
    FormationEvidence {
        depth: 0,
        baseline: 100,
        score_ppm: 1_000_000,
        venue_mask: 7,
        eligible_mask: 7,
    }
}
fn registered() -> VoidEngine<4> {
    let mut e = VoidEngine::new(cfg()).unwrap();
    e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
    e.observe(0, Timestamp(10), 208, 0, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Persisted);
    e.observe(0, Timestamp(11), 220, 0, true).unwrap();
    assert_eq!(e.metrics().registered, 1);
    e
}
#[test]
fn boundary_touch_and_partial_revisit_do_not_require_full_traversal() {
    let mut e = registered();
    e.observe(0, Timestamp(20), 216, 0, true).unwrap();
    let z = e.zones()[0].unwrap();
    assert_eq!(z.state, VoidState::Revisited);
    assert_eq!(z.revisit_count, 1);
    assert_eq!(z.max_penetration_ppm, 0);
    assert_eq!(e.metrics().first_revisit_latency_ns, 9);
    e.observe(0, Timestamp(21), 212, 0, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().max_penetration_ppm, 250_000);
    e.observe(0, Timestamp(22), 214, 0, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().max_penetration_ppm, 250_000);
    assert_eq!(e.metrics().revisits, 1);
    e.observe(0, Timestamp(23), 220, 0, true).unwrap();
    e.observe(0, Timestamp(24), 216, 0, true).unwrap();
    assert_eq!(e.metrics().revisits, 2);
    assert_eq!(e.metrics().penetration_distribution, [1, 0, 1, 0, 0, 0]);
}
#[test]
fn mirrored_penetration_and_histogram_conservation() {
    for side in [-1, 1] {
        let mut e = VoidEngine::<4>::new(cfg()).unwrap();
        e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
        let exit = if side == 1 { 220 } else { 196 };
        e.observe(0, Timestamp(11), exit, 0, true).unwrap();
        for step in 0..=16 {
            let price = if side == 1 { 216 - step } else { 200 + step };
            e.observe(0, Timestamp(20 + step as u64), price, 0, true)
                .unwrap();
            let z = e.zones()[0].unwrap();
            assert_eq!(z.max_penetration_ppm, (step * 1_000_000 / 16) as u32);
            assert_eq!(
                e.metrics().penetration_distribution.iter().sum::<u64>(),
                e.metrics().revisits
            );
        }
        assert_eq!(e.zones()[0].unwrap().state, VoidState::Revisited);
        assert_eq!(e.metrics().penetration_distribution[5], 1);
    }
}
#[test]
fn incomplete_persistence_and_early_exit_do_not_register() {
    let mut e = VoidEngine::<4>::new(cfg()).unwrap();
    e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
    e.observe(0, Timestamp(5), 220, 0, true).unwrap();
    e.observe(0, Timestamp(15), 220, 0, true).unwrap();
    assert_eq!(e.metrics().registered, 0);
    e.observe(0, Timestamp(16), 208, 0, true).unwrap();
    e.observe(0, Timestamp(17), 220, 0, true).unwrap();
    assert_eq!(e.metrics().registered, 1);
}
#[test]
fn refill_coverage_and_expiry_precede_touch() {
    for mode in 0..3 {
        let mut e = registered();
        let time = if mode == 2 { 111 } else { 20 };
        e.observe(
            0,
            Timestamp(time),
            216,
            if mode == 0 { 80 } else { 0 },
            mode != 1,
        )
        .unwrap();
        assert_eq!(e.metrics().revisits, 0);
        assert_eq!(
            e.zones()[0].unwrap().state,
            [
                VoidState::Refilled,
                VoidState::Invalidated,
                VoidState::Expired
            ][mode]
        );
    }
    let mut e = registered();
    e.observe(0, Timestamp(20), 220, 90, true).unwrap();
    assert_eq!(e.metrics().refilled_before_revisit, 1);
}
#[test]
fn skipped_crossing_is_not_an_observed_revisit() {
    let mut e = registered();
    e.observe(0, Timestamp(20), 198, 0, true).unwrap();
    assert_eq!(e.metrics().revisits, 0);
    assert_eq!(e.metrics().skipped_crossings, 1);
    e.observe(0, Timestamp(21), 200, 0, true).unwrap();
    assert_eq!(e.metrics().revisits, 1);
    assert_eq!(e.zones()[0].unwrap().max_penetration_ppm, 0);
}
#[test]
fn bounded_history_never_evicts_active_zones() {
    let mut e = VoidEngine::<1>::new(cfg()).unwrap();
    e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
    let other = PriceRegion {
        lower: PriceTicks(120),
        upper: PriceTicks(128),
    };
    assert_eq!(
        e.consider(Timestamp(1), other, evidence(), 248).unwrap(),
        Considered::Rejected
    );
    assert_eq!(e.metrics().capacity_rejections, 1);
    e.invalidate_all(Timestamp(2)).unwrap();
    assert_eq!(
        e.consider(Timestamp(3), other, evidence(), 248).unwrap(),
        Considered::Created(2)
    );
    assert_eq!(e.metrics().evicted_terminal, 1);
}
#[test]
fn overlap_mask_changes_and_bad_inputs_are_explicit() {
    let mut e = registered();
    let overlap = PriceRegion {
        lower: PriceTicks(104),
        upper: PriceTicks(112),
    };
    assert_eq!(
        e.consider(Timestamp(20), overlap, evidence(), 208).unwrap(),
        Considered::Rejected
    );
    assert_eq!(e.metrics().overlap_rejections, 1);
    assert_eq!(
        e.observe(0, Timestamp(19), 216, 0, true),
        Err(VoidError::ClockRegression)
    );
    let mut e = VoidEngine::<1>::new(cfg()).unwrap();
    let mut ev = evidence();
    ev.venue_mask = 1;
    e.consider(Timestamp(0), region(), ev, 208).unwrap();
    e.consider(Timestamp(5), region(), evidence(), 208).unwrap();
    e.observe(0, Timestamp(10), 220, 0, true).unwrap();
    assert_eq!(e.metrics().registered, 0);
    assert_eq!(e.metrics().candidate_restarts, 1);
    assert_eq!(e.zones()[0].unwrap().scope, Scope::MarketWide);
    ev.baseline = 0;
    assert_eq!(
        e.consider(Timestamp(11), region(), ev, 208),
        Err(VoidError::InvalidObservation)
    );
}
fn suspended() -> VoidEngine<4> {
    let mut e = VoidEngine::new(VoidConfig {
        coverage_loss: CoverageLoss::Suspend,
        ..cfg()
    })
    .unwrap();
    e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
    e.observe(0, Timestamp(10), 208, 0, true).unwrap();
    e.observe(0, Timestamp(11), 220, 0, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Exited);
    e
}
/// Under `Suspend`, a registered zone out of view stays registered and is revisited when the
/// price comes back into view; depth seen while uncovered is never judged.
#[test]
fn suspended_coverage_loss_keeps_registered_zones_until_they_are_seen_again() {
    let mut e = suspended();
    // Out of view, far above, with a meaningless "depth" that would otherwise be a refill.
    e.observe(0, Timestamp(20), 300, 95, false).unwrap();
    let z = e.zones()[0].unwrap();
    assert_eq!((z.state, z.current_depth), (VoidState::Exited, 0));
    assert_eq!((e.metrics().unobserved, e.metrics().invalidated), (1, 0));
    // Back in view and inside: an observed revisit, entered from above.
    e.observe(0, Timestamp(30), 216, 0, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Revisited);
    assert_eq!(e.metrics().revisits, 1);
    // Leaving while out of view returns the zone to Exited, so no stale revisit persists.
    e.observe(0, Timestamp(40), 300, 0, false).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Exited);
    // A return where depth has recovered is a refill, judged before any revisit.
    e.observe(0, Timestamp(50), 216, 90, true).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Refilled);
    assert_eq!(e.metrics().revisits, 1);
}
#[test]
fn suspended_zones_still_expire_and_candidates_still_invalidate() {
    let mut e = suspended();
    e.observe(0, Timestamp(111), 300, 0, false).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Expired);
    assert_eq!(e.metrics().unobserved, 0);
    // A candidate still forming cannot be confirmed out of view.
    let mut e = VoidEngine::<4>::new(VoidConfig {
        coverage_loss: CoverageLoss::Suspend,
        ..cfg()
    })
    .unwrap();
    e.consider(Timestamp(0), region(), evidence(), 208).unwrap();
    e.observe(0, Timestamp(5), 208, 0, false).unwrap();
    assert_eq!(e.zones()[0].unwrap().state, VoidState::Invalidated);
    // The default keeps Phase 4 behaviour.
    assert_eq!(CoverageLoss::default(), CoverageLoss::Invalidate);
}
