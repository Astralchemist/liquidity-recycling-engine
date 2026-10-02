//! Specification §39 scenarios A–F. Assertions are about mechanics, gating and risk, not about
//! profitability: fills follow the deliberately pessimistic strict trade-through rule.
use engine::{fixtures::*, *};
use inventory::InventoryEventKind as Cmd;
use risk::KillReason;
use simulation::Scenario;
use toxicity::Environment;
use voids::{Scope, VoidState};
mod support;
use support::*;
fn zone(run: &Run, id: u64) -> voids::LiquidityVoid {
    *run.engine
        .research()
        .research()
        .voids()
        .zones()
        .iter()
        .flatten()
        .find(|z| z.id == id)
        .unwrap()
}
/// New inventory is only ever reserved while an allowed zone is revisited AND balanced-active.
fn assert_entries_gated(run: &Run) {
    let mut placed = 0;
    for s in &run.steps {
        if s.entries_placed > placed {
            assert!(
                s.allowed_revisit,
                "entry without an allowed revisit at {}",
                s.time
            );
            assert_eq!(s.environment, Environment::BalancedActive);
            assert!(!s.halted);
        }
        placed = s.entries_placed;
    }
}
/// After the first halt, the journal contains no new reservation and no normal close.
fn assert_no_opens_after_halt(run: &Run) {
    let Some(halt) = run
        .journal
        .iter()
        .position(|c| matches!(c.kind, Cmd::Halt { .. }))
    else {
        return;
    };
    assert!(run.journal[halt..].iter().all(|c| !matches!(
        c.kind,
        Cmd::ReserveOpen { .. }
            | Cmd::ReserveClose {
                mode: inventory::CloseMode::Normal,
                ..
            }
    )));
}
fn flat(run: &Run) -> bool {
    let x = run.engine.ledger().exposure();
    x.gross() == 0 && x.open_orders == 0
}
#[test]
fn a_revisit_then_oscillation_recycles_inventory_without_a_halt() {
    let run = run(Scenario::RevisitOscillation);
    let z = zone(&run, 1);
    assert_eq!((z.region.lower.0, z.region.upper.0), (100, 103));
    assert_eq!(z.scope, Scope::MarketWide);
    assert!(z.registered_at.is_some() && z.revisit_count >= 1);
    assert_entries_gated(&run);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), None);
    assert!(flat(&run));
    let (completed, _, _) = l.episode_totals();
    assert!(completed >= 1);
    let episode = l.episodes().iter().flatten().next().unwrap();
    assert_eq!(episode.void_id, 1);
    assert_eq!(episode.forced, None);
    assert!(episode.counts.completed_cycles >= 10);
    // Only maker fills: no emergency exit ran.
    assert_eq!(l.counts().maker_to_maker, l.counts().completed_cycles);
    assert_eq!(run.engine.metrics().taker_fills, 0);
}
#[test]
fn b_one_way_continuation_is_stopped_by_hard_risk_with_bounded_loss() {
    let run = run(Scenario::OneWayContinuation);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), Some(KillReason::PnlBreach));
    assert!(flat(&run));
    assert_entries_gated(&run);
    assert_no_opens_after_halt(&run);
    let episode = l.episodes().iter().flatten().next().unwrap();
    assert_eq!(episode.forced, Some(KillReason::PnlBreach));
    assert!(episode.counts.taker_escapes >= 1);
    // The breach is detected within one event of crossing the 300-atom liability limit, so the
    // realized loss stays near it: limit + one tick per child + slippage and fees per exit.
    let limits = l.config().limits;
    assert!(episode.peak_liability.0 >= limits.max_liability.0);
    assert!(l.equity().unwrap().0 > -(limits.max_liability.0 + 200));
    // The continuation itself is classified as trending; no inventory is added during it.
    assert!(run.engine.environment().time_in_state()[Environment::Trending as usize] > 0);
}
#[test]
fn c_venue_local_void_is_observed_but_not_traded() {
    let run = run(Scenario::LocalVoid);
    let z = zone(&run, 1);
    assert_eq!(z.scope, Scope::VenueLocal);
    assert_eq!(z.venue_mask, 0b001);
    assert!(z.revisit_count >= 1);
    let m = run.engine.metrics();
    assert!(m.disallowed_scope_steps > 0);
    assert_eq!(m.entries_placed, 0);
    assert_eq!(run.engine.ledger().counts().fills, 0);
    assert!(
        run.journal
            .iter()
            .all(|c| matches!(c.kind, Cmd::Mark { .. } | Cmd::Tick))
    );
}
#[test]
fn d_market_wide_void_with_venue_lead_lag_stays_open_and_trades() {
    let run = run(Scenario::GlobalVoid);
    let z = zone(&run, 1);
    assert_eq!(z.scope, Scope::MarketWide);
    // A sustained one-tick venue lead never locks the consolidated two-tick book, so the
    // market-wide zone survives the cross-venue movement instead of being invalidated.
    assert_ne!(z.state, VoidState::Invalidated);
    assert_eq!(
        run.engine
            .research()
            .research()
            .voids()
            .metrics()
            .invalidated,
        0
    );
    // Half-tick venue divergence is observed (sequential venue updates make it common in every
    // scenario; lead/lag cross-correlation is later research tooling, not asserted here).
    let m = run.engine.metrics();
    assert_eq!(m.max_abs_divergence_x2, 1);
    assert!(m.divergent_events > 0);
    assert_entries_gated(&run);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), None);
    assert!(l.counts().completed_cycles >= 1);
    // Any residual inventory is reported through the active episode's mark-to-market.
    let open = l.active_episode().map_or(0, |e| e.residual_mtm.0);
    assert_eq!(
        l.equity().unwrap().0,
        l.episode_totals().2.0 + l.active_episode().map_or(0, |e| e.net_pnl.0)
    );
    assert_eq!(open, l.unrealized().0);
}
#[test]
fn e_toxic_fill_is_held_through_the_shock_and_recovers_after_replenishment() {
    let run = run(Scenario::ToxicRecovery);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), None);
    assert!(flat(&run));
    assert_entries_gated(&run);
    // Inventory was held through a liquidity shock: no exit on shock in the default policy.
    let shocked = run
        .steps
        .iter()
        .position(|s| {
            s.environment == Environment::LiquidityShock && s.gross > 0 && s.time > 700_000_000
        })
        .expect("held inventory during the sweep");
    // Liability peaks after the sweep, then Recovery = harvest - liability returns to >= 0.
    let peak = (shocked..run.steps.len())
        .max_by_key(|&i| (run.steps[i].liability, std::cmp::Reverse(i)))
        .unwrap();
    assert!(run.steps[peak].liability > 0 && run.steps[peak].recovery < 0);
    assert!(run.steps[peak..].iter().any(|s| s.recovery >= 0));
    let episode = l.episodes().iter().flatten().last().unwrap();
    assert_eq!(episode.forced, None);
}
#[test]
fn e_variant_exit_on_liquidity_shock_forces_a_regime_change_exit() {
    let mut config = engine();
    config.policy.exit_on_environment |= Environment::LiquidityShock.bit();
    let run = run_with(Scenario::ToxicRecovery, config);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), Some(KillReason::RegimeChange));
    assert!(flat(&run));
    assert_no_opens_after_halt(&run);
    assert!(run.engine.metrics().taker_fills >= 1);
}
#[test]
fn f_toxic_fill_without_replenishment_exits_through_risk() {
    let run = run(Scenario::EmergencyExit);
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), Some(KillReason::PnlBreach));
    assert!(flat(&run));
    assert_no_opens_after_halt(&run);
    let episode = l.episodes().iter().flatten().next().unwrap();
    assert_eq!(episode.forced, Some(KillReason::PnlBreach));
    assert!(episode.counts.taker_escapes >= 1);
    // Further voids form below as bids drain, but nothing is traded after the halt.
    assert!(
        run.engine
            .research()
            .research()
            .voids()
            .zones()
            .iter()
            .flatten()
            .count()
            >= 2
    );
    assert!(run.engine.environment().time_in_state()[Environment::LiquidityShock as usize] > 0);
}
#[test]
fn every_scenario_is_deterministic() {
    for scenario in Scenario::ALL {
        let (a, b) = (run(scenario), run(scenario));
        assert_eq!(a.engine, b.engine);
        assert_eq!(a.journal, b.journal);
    }
}
#[test]
fn market_data_fault_halts_and_flattens_from_the_last_marks() {
    // Drop one venue-1 event while inventory is held: a sequence gap faults the research path.
    let mut events = simulation::scenarios::generate(Scenario::RevisitOscillation);
    let held = run_events(&events, engine())
        .steps
        .iter()
        .position(|s| s.gross > 0)
        .unwrap();
    let gap = (held..events.len())
        .find(|&i| events[i].venue.0 == 1)
        .unwrap();
    events.remove(gap);
    let run = run_events(&events, engine());
    let l = run.engine.ledger();
    assert_eq!(l.halt_reason(), Some(KillReason::SequenceGap));
    assert!(run.engine.research_fault().is_some());
    assert!(flat(&run));
    assert_eq!(run.engine.metrics().unpriced_emergency_steps, 0);
}
#[test]
fn invalid_engine_configuration_is_rejected() {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    let mut bad = [engine(); 6];
    bad[0].policy.entry_levels = 0;
    bad[1].policy.harvest_ticks = 0;
    bad[2].policy.mark_refresh_ns = inventory().limits.mark_stale_ns;
    bad[3].policy.allowed_scopes = 0;
    bad[4].fills.taker_fee = fixed_point::Money(-1);
    bad[5].policy.entry_max_abs_net = inventory().limits.max_net_units + 1;
    for config in bad {
        assert_eq!(
            SyntheticEngine::new(grid, venues, liquidity, voids, flow(), inventory(), config),
            Err(EngineError::InvalidConfig)
        );
    }
    let mut swapped = inventory();
    swapped.venues.swap(0, 1);
    assert!(
        SyntheticEngine::new(grid, venues, liquidity, voids, flow(), swapped, engine()).is_err()
    );
}
