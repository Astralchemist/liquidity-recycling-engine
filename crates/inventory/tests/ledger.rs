use common::*;
use fixed_point::*;
use inventory::{
    fixtures::{Scenario, generate},
    *,
};
use risk::{Denial, KillReason};
mod support;
use support::*;
#[test]
fn profitable_episode_recycles_a_losing_child_and_recovers_liability() {
    let mut e = engine();
    generate(Scenario::Recovery, |event| e.apply(event).map(|_| ())).unwrap();
    assert_eq!(e.accounts().realized, Money(10));
    assert_eq!(e.accounts().rebates, Money(4));
    assert_eq!(e.equity().unwrap(), Money(14));
    assert_eq!(e.cash(), Money(14));
    assert_eq!(e.exposure().gross(), 0);
    let episode = e.episodes().iter().flatten().last().unwrap();
    assert_eq!(episode.net_pnl, Money(14));
    assert_eq!(episode.peak_liability, Money(30));
    assert_eq!(episode.peak_gross, 2);
    assert_eq!(episode.counts.orders, 4);
    assert_eq!(episode.counts.maker_to_maker, 2);
    assert!(episode.first_recovery_at.is_some());
    assert_eq!(episode.recovery_yield_ppm().unwrap(), Some(466666));
    assert!(e.closed_lots().iter().flatten().any(|l| l.realized.0 < 0));
    assert_eq!(e.halt_reason(), None);
}
#[test]
fn forced_exit_includes_liquidation_tail_fees_funding_and_loss() {
    let mut e = engine();
    let mut forced_open = false;
    generate(Scenario::ForcedExit, |event| {
        e.apply(event)?;
        if matches!(
            event.kind,
            InventoryEventKind::Mark {
                bid: PriceTicks(90),
                ..
            }
        ) {
            assert_eq!(e.halt_reason(), Some(KillReason::PnlBreach));
            assert!(e.active_episode().unwrap().ended_at.is_none());
            assert!(e.safety_work_remaining());
            forced_open = true;
        }
        Ok::<_, InventoryError>(())
    })
    .unwrap();
    assert!(forced_open);
    assert_eq!(e.accounts().realized, Money(-110));
    assert_eq!(e.equity().unwrap(), Money(-112));
    assert_eq!(e.cash(), Money(-112));
    assert!(!e.safety_work_remaining());
    let episode = e.episodes().iter().flatten().next().unwrap();
    assert_eq!(episode.net_pnl, Money(-112));
    assert_eq!(episode.forced, Some(KillReason::PnlBreach));
    assert_eq!(episode.counts.taker_escapes, 1);
    assert_eq!(episode.peak_liability, Money(100));
    assert_eq!(
        apply(
            &mut e,
            InventoryEventKind::ReserveOpen {
                venue: VenueId(1),
                side: Side::Buy,
                role: InventoryRole::Harvest,
                evidence: evidence()
            }
        ),
        Err(InventoryError::Denied(Denial::Halted))
    );
}
#[test]
fn underwater_balanced_child_requires_profit_rebalance_or_emergency() {
    let mut e = engine();
    mark(&mut e, 100);
    let long = open(&mut e, Side::Buy, 100);
    let short = open(&mut e, Side::Sell, 100);
    let before = e;
    assert_eq!(
        reserve_close(&mut e, long, 99, CloseMode::Normal),
        Err(InventoryError::Denied(Denial::NoBenefit))
    );
    assert_eq!(e, before);
    apply(
        &mut e,
        InventoryEventKind::Halt {
            reason: KillReason::StructureInvalidated,
        },
    )
    .unwrap();
    close(&mut e, long, 99, CloseMode::Emergency);
    close(&mut e, short, 101, CloseMode::Emergency);
    assert_eq!(e.accounts().realized, Money(-20));
    assert_eq!(e.exposure().gross(), 0);
}
#[test]
fn confirmed_fill_loss_is_accounted_after_profitable_approval() {
    let mut e = engine();
    mark(&mut e, 100);
    let long = open(&mut e, Side::Buy, 100);
    open(&mut e, Side::Sell, 100);
    reserve_close(&mut e, long, 102, CloseMode::Normal).unwrap();
    apply(
        &mut e,
        InventoryEventKind::FillClose {
            id: long,
            price: PriceTicks(99),
            maker: false,
            charges: Charges {
                fee: Money(2),
                ..Charges::default()
            },
        },
    )
    .unwrap();
    assert_eq!(e.accounts().realized, Money(-10));
    assert_eq!(e.accounts().fees, Money(2));
    assert_eq!(e.exposure().net(), -1);
}
#[test]
fn in_flight_open_after_halt_and_cancel_ack_reservations() {
    let mut e = engine();
    mark(&mut e, 100);
    let reserve = InventoryEventKind::ReserveOpen {
        venue: VenueId(1),
        side: Side::Buy,
        role: InventoryRole::Harvest,
        evidence: evidence(),
    };
    let Outcome::Reserved(id) = apply(&mut e, reserve).unwrap() else {
        panic!()
    };
    let Outcome::Reserved(cancel) = apply(&mut e, reserve).unwrap() else {
        panic!()
    };
    apply(
        &mut e,
        InventoryEventKind::Halt {
            reason: KillReason::Disconnect,
        },
    )
    .unwrap();
    assert_eq!(e.exposure().pending_buys, 2);
    apply(
        &mut e,
        InventoryEventKind::FillOpen {
            id,
            price: PriceTicks(100),
            maker: true,
            charges: Charges::default(),
        },
    )
    .unwrap();
    apply(&mut e, InventoryEventKind::CancelOpen { id: cancel }).unwrap();
    close(&mut e, id, 99, CloseMode::Emergency);
    assert!(!e.safety_work_remaining());
    assert_eq!(e.equity().unwrap(), Money(-10));
}
#[test]
fn pending_closes_consume_net_budget_and_do_not_release_gross_capacity() {
    let mut c = config();
    c.limits.max_net_units = 1;
    c.limits.max_gross_units = 2;
    c.limits.max_venue_units = 2;
    c.limits.max_target_deviation = 1;
    let mut e = Engine::new(c).unwrap();
    mark(&mut e, 100);
    let long = open(&mut e, Side::Buy, 100);
    open(&mut e, Side::Sell, 100);
    reserve_close(&mut e, long, 101, CloseMode::Normal).unwrap();
    assert_eq!(
        apply(
            &mut e,
            InventoryEventKind::ReserveOpen {
                venue: VenueId(1),
                side: Side::Sell,
                role: InventoryRole::Harvest,
                evidence: evidence()
            }
        ),
        Err(InventoryError::Denied(Denial::Net))
    );
    assert_eq!(
        apply(
            &mut e,
            InventoryEventKind::ReserveOpen {
                venue: VenueId(1),
                side: Side::Buy,
                role: InventoryRole::Harvest,
                evidence: evidence()
            }
        ),
        Err(InventoryError::Denied(Denial::Gross))
    );
    apply(&mut e, InventoryEventKind::CancelClose { id: long }).unwrap();
    assert_eq!(
        e.lots()
            .iter()
            .flatten()
            .find(|l| l.id == long)
            .unwrap()
            .role,
        InventoryRole::Harvest
    );
}
#[test]
fn nonzero_target_and_residual_episode_mtm_are_explicit() {
    let mut c = config();
    c.target = InventoryUnits(1);
    c.completion_max_gross = 1;
    let mut e = Engine::new(c).unwrap();
    mark(&mut e, 99);
    open(&mut e, Side::Buy, 100);
    let episode = e.episodes().iter().flatten().next().unwrap();
    assert_eq!(episode.net_pnl, Money(-10));
    assert_eq!(episode.residual_mtm, Money(-10));
    assert_eq!(e.exposure().net(), 1);
    assert!(e.active_episode().is_none());
}
#[test]
fn funding_credits_and_distinct_charges_conserve_cash() {
    let mut e = engine();
    mark(&mut e, 100);
    let id = open(&mut e, Side::Sell, 100);
    apply(
        &mut e,
        InventoryEventKind::Funding {
            id,
            cost: Money(-3),
        },
    )
    .unwrap();
    reserve_close(&mut e, id, 99, CloseMode::Normal).unwrap();
    apply(
        &mut e,
        InventoryEventKind::FillClose {
            id,
            price: PriceTicks(99),
            maker: false,
            charges: Charges {
                rebate: Money(1),
                fee: Money(2),
                slippage: Money(1),
            },
        },
    )
    .unwrap();
    assert_eq!(e.equity().unwrap(), Money(11));
    assert_eq!(e.cash(), Money(11));
    assert_eq!(
        e.closed_lots().iter().flatten().next().unwrap().net_pnl,
        Money(11)
    );
}
#[test]
fn stale_marks_age_episode_and_drawdown_thresholds_halt() {
    for cause in 0..4 {
        let mut c = config();
        match cause {
            0 => c.limits.mark_stale_ns = 5,
            1 => c.limits.max_position_age_ns = 5,
            2 => c.limits.max_episode_ns = 5,
            _ => c.limits.max_drawdown = Money(5),
        };
        let mut e = Engine::new(c).unwrap();
        mark(&mut e, 100);
        open(&mut e, Side::Buy, 100);
        if cause == 3 {
            mark(&mut e, 99);
        } else {
            e.apply(InventoryEvent {
                sequence: e.last_sequence() + 1,
                timestamp: Timestamp(20),
                kind: InventoryEventKind::Tick,
            })
            .unwrap();
        }
        assert_eq!(
            e.halt_reason(),
            Some(match cause {
                0 => KillReason::StaleData,
                1 => KillReason::PositionAge,
                2 => KillReason::EpisodeDuration,
                _ => KillReason::PnlBreach,
            })
        );
    }
}
#[test]
fn arithmetic_failure_is_atomic_and_protocol_faults_latch() {
    let mut e = engine();
    mark(&mut e, 100);
    let id = open(&mut e, Side::Buy, 100);
    let before = e;
    assert!(matches!(
        apply(
            &mut e,
            InventoryEventKind::Funding {
                id,
                cost: Money(i128::MAX)
            }
        ),
        Err(InventoryError::Arithmetic(_))
    ));
    assert_eq!(e.cash(), before.cash());
    assert_eq!(e.accounts(), before.accounts());
    assert_eq!(e.lots(), before.lots());
    assert_eq!(e.last_sequence(), before.last_sequence());
    assert_eq!(e.halt_reason(), Some(KillReason::UnhandledState));
    let mut e = engine();
    assert_eq!(
        e.apply(InventoryEvent {
            sequence: 2,
            timestamp: Timestamp(0),
            kind: InventoryEventKind::Tick
        }),
        Err(InventoryError::SequenceGap)
    );
    assert_eq!(e.halt_reason(), Some(KillReason::SequenceGap));
    let mut e = engine();
    mark(&mut e, 100);
    assert_eq!(
        e.apply(InventoryEvent {
            sequence: 2,
            timestamp: Timestamp(0),
            kind: InventoryEventKind::Tick
        }),
        Err(InventoryError::ClockRegression)
    );
    assert_eq!(e.halt_reason(), Some(KillReason::ClockAnomaly));
}
#[test]
fn pending_closes_cannot_both_claim_the_same_rebalance() {
    let mut e = engine();
    mark(&mut e, 100);
    let first = open(&mut e, Side::Buy, 100);
    let second = open(&mut e, Side::Buy, 100);
    open(&mut e, Side::Sell, 100);
    // Net +1: one losing long close restores target; a second would overshoot to -1.
    reserve_close(&mut e, first, 99, CloseMode::Normal).unwrap();
    let before = e;
    assert_eq!(
        reserve_close(&mut e, second, 99, CloseMode::Normal),
        Err(InventoryError::Denied(Denial::NoBenefit))
    );
    assert_eq!(e, before);
    // A profitable second close remains allowed even though it overshoots the target.
    reserve_close(&mut e, second, 101, CloseMode::Normal).unwrap();
}
