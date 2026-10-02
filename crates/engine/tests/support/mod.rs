#![allow(dead_code)]
use engine::{fixtures::*, *};
use inventory::{CloseMode, InventoryEvent};
use market_events::MarketEvent;
use simulation::{Scenario, scenarios::generate};
use toxicity::Environment;
use voids::VoidState;
/// State after one market event, for path assertions.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub time: u64,
    pub environment: Environment,
    pub gross: i64,
    pub liability: i128,
    pub recovery: i128,
    pub allowed_revisit: bool,
    pub entries_placed: u64,
    pub halted: bool,
}
/// The engine is boxed: debug builds copy large by-value returns on 2 MB test-thread stacks.
pub struct Run {
    pub engine: Box<SyntheticEngine>,
    pub journal: Vec<InventoryEvent>,
    pub steps: Vec<Step>,
}
pub fn build_with(config: EngineConfig) -> Box<SyntheticEngine> {
    let (grid, venues) = market();
    let (liquidity, voids) = structures();
    Box::new(
        SyntheticEngine::new(grid, venues, liquidity, voids, flow(), inventory(), config).unwrap(),
    )
}
pub fn run_events(events: &[MarketEvent], config: EngineConfig) -> Run {
    let mut engine = build_with(config);
    let mut journal = Vec::new();
    let mut steps = Vec::with_capacity(events.len());
    for event in events {
        engine.apply(event, &mut |c| journal.push(*c)).unwrap();
        check_invariants(&engine);
        let l = engine.ledger();
        steps.push(Step {
            time: event.receive_ts.0,
            environment: engine.environment().state(),
            gross: l.exposure().gross(),
            liability: l.liability().0,
            recovery: l.recovery().unwrap().0,
            allowed_revisit: engine
                .research()
                .research()
                .voids()
                .zones()
                .iter()
                .flatten()
                .any(|z| {
                    z.state == VoidState::Revisited
                        && scope_bit(z.scope) & config.policy.allowed_scopes != 0
                }),
            entries_placed: engine.metrics().entries_placed,
            halted: l.halt_reason().is_some(),
        });
    }
    Run {
        engine,
        journal,
        steps,
    }
}
pub fn run_with(scenario: Scenario, config: EngineConfig) -> Run {
    run_events(&generate(scenario), config)
}
pub fn run(scenario: Scenario) -> Run {
    run_with(scenario, engine())
}
/// Engine-wide invariants checked after EVERY market event.
pub fn check_invariants(e: &SyntheticEngine) {
    let l = e.ledger();
    let limits = l.config().limits;
    let x = l.exposure();
    assert!(x.net().abs() <= limits.max_net_units);
    assert!(x.gross() <= limits.max_gross_units);
    assert!(x.open_orders <= limits.max_open_orders);
    for v in 1..=3 {
        assert!(l.venue_exposure(common::VenueId(v)).unwrap().0 <= limits.max_venue_units);
    }
    // Every simulated resting order is exactly one ledger reservation or pending normal close.
    let mut entries = 0;
    let mut closes = 0;
    let queued = matches!(e.config().fills.model, FillModel::Queue(_));
    for o in e.orders().iter().flatten() {
        // Queue state exists exactly under the queue model, is never complete while resting
        // and never holds more ahead than the venue displays at the order price.
        assert_eq!(o.queue.is_some(), queued);
        if let Some(q) = o.queue {
            assert!(q.ahead >= 0 && q.pending_execution >= 0);
            assert!(q.filled >= 0 && q.filled < q.size);
            let v = (o.venue.0 - 1) as usize;
            if let Some(shown) = e.displayed(v, o.side, o.price).unwrap() {
                assert!(q.ahead <= shown || q.ahead >= i64::MAX / 4);
            }
        }
        match o.kind {
            OrderKind::Entry { .. } => {
                entries += 1;
                let r = l
                    .reservations()
                    .iter()
                    .flatten()
                    .find(|r| r.id == o.id)
                    .unwrap();
                assert_eq!((r.venue, r.side), (o.venue, o.side));
            }
            OrderKind::Harvest | OrderKind::Rebalance => {
                closes += 1;
                let lot = l
                    .lots()
                    .iter()
                    .flatten()
                    .find(|lot| lot.id == o.id)
                    .unwrap();
                assert_eq!(lot.pending_close, Some(CloseMode::Normal));
                assert_ne!(lot.side, o.side);
            }
        }
    }
    assert_eq!(entries, l.reservations().iter().flatten().count());
    assert_eq!(
        closes,
        l.lots()
            .iter()
            .flatten()
            .filter(|lot| lot.pending_close.is_some())
            .count()
    );
    // Target 0 with completion at zero gross: every PnL atom belongs to one episode.
    assert_eq!(l.unassigned_pnl().unwrap().0, 0);
    // The kill procedure completes in the step that observes a priced halt.
    if l.halt_reason().is_some() && e.metrics().unpriced_emergency_steps == 0 {
        assert_eq!((x.gross(), x.open_orders), (0, 0));
        assert!(e.orders().iter().all(Option::is_none));
    }
}
