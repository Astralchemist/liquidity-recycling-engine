//! Seeded sweep over policy parameters and all scenarios. `run_with` checks the engine-wide
//! invariants after every market event: hard limits, order/ledger consistency, episode PnL
//! partition and same-step flattening after a halt. Outcomes may differ; invariants may not.
use engine::fixtures::*;
use simulation::Scenario;
use toxicity::Environment;
mod support;
use support::*;
#[test]
fn policy_parameter_sweep_never_breaks_engine_invariants() {
    let mut state = 17_u64;
    let mut draw = |n: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    let mut halted = 0;
    let mut cycles = 0;
    for _ in 0..12 {
        let mut c = engine();
        c.policy.entry_levels = 1 + draw(3) as u32;
        c.policy.entry_max_abs_net = 1 + draw(4) as i64;
        c.policy.harvest_ticks = 1 + draw(3) as i64;
        c.policy.rebalance_threshold_units = draw(3) as i64;
        c.policy.rebalance_improve_ticks = draw(2) as i64;
        c.policy.reprice_ticks = 1 + draw(3) as i64;
        c.policy.rebalance_age_ns = [50_000_000, 600_000_000, u64::MAX / 4][draw(3) as usize];
        if draw(2) == 0 {
            c.policy.exit_on_environment |= Environment::LiquidityShock.bit();
        }
        for scenario in Scenario::ALL {
            let run = run_with(scenario, c);
            halted += u32::from(run.engine.ledger().halt_reason().is_some());
            cycles += run.engine.ledger().counts().completed_cycles;
        }
    }
    // The sweep exercised both recycling and forced exits (observed: 16 halts, 390 cycles).
    assert!(halted >= 12 && cycles >= 300);
}
