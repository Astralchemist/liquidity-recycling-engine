use common::*;
use fixed_point::*;
use inventory::*;
use std::collections::BTreeMap;
mod support;
use support::*;
/// Independent reference for one open child unit.
#[derive(Clone, Copy)]
struct Child {
    venue: VenueId,
    side: Side,
    entry: i64,
    /// Entry rebate - fee - slippage, minus funding paid so far.
    carry: i128,
    closing: bool,
}
fn sign(side: Side) -> i128 {
    if side == Side::Buy { 1 } else { -1 }
}
fn charge_net(c: Charges) -> i128 {
    c.rebate.0 - c.fee.0 - c.slippage.0
}
/// Reference statement of the central rule: profit net of carry, or a projected rebalance.
fn close_allowed(children: &BTreeMap<u64, Child>, id: u64, expected: i64, c: Charges) -> bool {
    let child = children[&id];
    let net =
        sign(child.side) * i128::from(expected - child.entry) * 10 + child.carry + charge_net(c);
    let projected: i128 = children
        .values()
        .filter(|c| !c.closing)
        .map(|c| sign(c.side))
        .sum();
    net > 0 || (projected - sign(child.side)).abs() < projected.abs()
}
#[test]
fn generated_commands_conserve_cash_equity_and_reserved_exposure() {
    let mut cfg = config();
    cfg.limits.max_liability = Money(1_000_000);
    cfg.limits.max_drawdown = Money(1_000_000);
    cfg.limits.max_adverse_episodes = 100_000;
    let mut e = Engine::new(cfg).unwrap();
    let mut marks = [100_i64; 3];
    for v in 1..=3 {
        apply(
            &mut e,
            InventoryEventKind::Mark {
                venue: VenueId(v),
                bid: PriceTicks(100),
                ask: PriceTicks(100),
            },
        )
        .unwrap();
    }
    let mut children = BTreeMap::<u64, Child>::new();
    let (mut cash, mut realized) = (0_i128, 0_i128);
    let (mut rebalance_losses, mut no_benefit) = (0, 0);
    let mut state = 33_u64;
    // High LCG bits only: low bits of a power-of-two LCG have short periods.
    let mut draw = |n: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    for _ in 0..20_000 {
        let price = 98 + draw(5) as i64;
        let venue = VenueId(draw(3) as u16 + 1);
        let charges = Charges {
            rebate: Money(draw(2) as i128),
            fee: Money(draw(3) as i128),
            slippage: Money(draw(2) as i128),
        };
        let side = if draw(2) == 0 { Side::Buy } else { Side::Sell };
        let maker = draw(2) == 0;
        let favourable = draw(2) == 0;
        let funding = draw(5) as i128 - 2;
        let kind = match draw(8) {
            0 => Some(InventoryEventKind::ReserveOpen {
                venue,
                side,
                role: InventoryRole::Harvest,
                evidence: evidence(),
            }),
            1 => e
                .reservations()
                .iter()
                .flatten()
                .next()
                .map(|r| InventoryEventKind::FillOpen {
                    id: r.id,
                    price: PriceTicks(price),
                    maker,
                    charges,
                }),
            2 => e
                .reservations()
                .iter()
                .flatten()
                .next()
                .map(|r| InventoryEventKind::CancelOpen { id: r.id }),
            3 => e
                .lots()
                .iter()
                .flatten()
                .find(|l| l.pending_close.is_none())
                .map(|l| InventoryEventKind::ReserveClose {
                    id: l.id,
                    expected_price: PriceTicks(match (favourable, l.side) {
                        (true, Side::Buy) => 200,
                        (true, Side::Sell) => 1,
                        (false, _) => price,
                    }),
                    expected_charges: charges,
                    mode: CloseMode::Normal,
                }),
            4 => e
                .lots()
                .iter()
                .flatten()
                .find(|l| l.pending_close.is_some())
                .map(|l| InventoryEventKind::FillClose {
                    id: l.id,
                    price: PriceTicks(price),
                    maker,
                    charges,
                }),
            5 => e
                .lots()
                .iter()
                .flatten()
                .find(|l| l.pending_close.is_some())
                .map(|l| InventoryEventKind::CancelClose { id: l.id }),
            6 => Some(InventoryEventKind::Mark {
                venue,
                bid: PriceTicks(price),
                ask: PriceTicks(price),
            }),
            _ => e
                .lots()
                .iter()
                .flatten()
                .next()
                .map(|l| InventoryEventKind::Funding {
                    id: l.id,
                    cost: Money(funding),
                }),
        };
        let Some(kind) = kind else { continue };
        let before = e;
        let rule = match kind {
            InventoryEventKind::ReserveClose {
                id,
                expected_price,
                expected_charges,
                ..
            } => Some(close_allowed(
                &children,
                id,
                expected_price.0,
                expected_charges,
            )),
            _ => None,
        };
        match apply(&mut e, kind) {
            Ok(_) => match kind {
                InventoryEventKind::FillOpen {
                    id, price, charges, ..
                } => {
                    let r = before
                        .reservations()
                        .iter()
                        .flatten()
                        .find(|r| r.id == id)
                        .unwrap();
                    cash -= sign(r.side) * i128::from(price.0) * 10;
                    cash += charge_net(charges);
                    children.insert(
                        id,
                        Child {
                            venue: r.venue,
                            side: r.side,
                            entry: price.0,
                            carry: charge_net(charges),
                            closing: false,
                        },
                    );
                }
                InventoryEventKind::ReserveClose {
                    id, expected_price, ..
                } => {
                    assert_eq!(rule, Some(true), "central rule violated by approval");
                    let child = children.get_mut(&id).unwrap();
                    if sign(child.side) * i128::from(expected_price.0 - child.entry) < 0 {
                        rebalance_losses += 1;
                    }
                    child.closing = true;
                }
                InventoryEventKind::CancelClose { id } => {
                    children.get_mut(&id).unwrap().closing = false;
                }
                InventoryEventKind::FillClose {
                    id, price, charges, ..
                } => {
                    let child = children.remove(&id).unwrap();
                    cash += sign(child.side) * i128::from(price.0) * 10;
                    cash += charge_net(charges);
                    realized += sign(child.side) * i128::from(price.0 - child.entry) * 10;
                }
                InventoryEventKind::Funding { id, cost } => {
                    cash -= cost.0;
                    children.get_mut(&id).unwrap().carry -= cost.0;
                }
                InventoryEventKind::Mark { venue, bid, .. } => marks[venue.0 as usize - 1] = bid.0,
                _ => {}
            },
            Err(InventoryError::Denied(denial)) => {
                assert_eq!(e, before);
                if denial == risk::Denial::NoBenefit {
                    assert_eq!(rule, Some(false), "central rule over-denied");
                    no_benefit += 1;
                }
            }
            Err(error) => panic!("unexpected {error:?}"),
        }
        let mut market_value = 0;
        let mut debt = 0;
        let mut net = 0;
        let mut venue_gross = [0_i64; 3];
        for child in children.values() {
            let s = sign(child.side);
            net += s;
            venue_gross[child.venue.0 as usize - 1] += 1;
            let price = marks[child.venue.0 as usize - 1];
            market_value += s * i128::from(price) * 10;
            debt += (-s * i128::from(price - child.entry) * 10).max(0);
        }
        assert_eq!(e.cash().0, cash);
        assert_eq!(e.equity().unwrap().0, cash + market_value);
        assert_eq!(e.accounts().realized.0, realized);
        assert_eq!(e.liability().0, debt);
        assert_eq!(i128::from(e.exposure().net()), net);
        assert_eq!(e.exposure().gross() as usize, children.len());
        for v in 1..=3_u16 {
            let pending = e
                .reservations()
                .iter()
                .flatten()
                .filter(|r| r.venue == VenueId(v))
                .count() as i64;
            assert_eq!(
                e.venue_exposure(VenueId(v)).unwrap(),
                (venue_gross[v as usize - 1], pending)
            );
        }
        // History is committed only with its transaction: ring contents + evictions match counters.
        let (closed_evicted, episodes_evicted) = e.history_evictions();
        assert_eq!(
            e.closed_lots().iter().flatten().count() as u64 + closed_evicted,
            e.counts().completed_cycles
        );
        assert_eq!(
            e.episodes().iter().flatten().count() as u64 + episodes_evicted,
            e.episode_totals().0
        );
        // Target zero, completion at zero gross: every PnL atom belongs to exactly one episode.
        assert_eq!(e.unassigned_pnl().unwrap(), Money(0));
        if children.is_empty() && e.exposure().open_orders == 0 {
            assert!(e.active_episode().is_none());
        }
        assert_eq!(e.halt_reason(), None);
        let x = e.exposure();
        assert!(x.net() + x.pending_buys + x.closing_buys <= cfg.limits.max_net_units);
        assert!(x.net() - x.pending_sells - x.closing_sells >= -cfg.limits.max_net_units);
        assert!(x.gross() + x.pending_buys + x.pending_sells <= cfg.limits.max_gross_units);
        assert!(x.open_orders <= cfg.limits.max_open_orders);
    }
    assert!(e.counts().fills > 100);
    assert!(e.counts().completed_cycles > 20);
    assert!(
        e.episode_totals().0 > 8,
        "episode ring eviction was not exercised"
    );
    assert!(
        rebalance_losses > 0,
        "no losing rebalance close was exercised"
    );
    assert!(no_benefit > 0, "no NoBenefit denial was exercised");
}
#[test]
fn all_reserved_fill_permutations_stay_inside_net_and_gross_limits() {
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).any(|i| order[..i].contains(&order[i])) {
                        continue;
                    }
                    let mut cfg = config();
                    cfg.limits.max_net_units = 2;
                    cfg.limits.max_gross_units = 4;
                    cfg.limits.max_venue_units = 4;
                    cfg.limits.max_target_deviation = 2;
                    let mut e = Engine::new(cfg).unwrap();
                    mark(&mut e, 100);
                    for side in [Side::Buy, Side::Buy, Side::Sell, Side::Sell] {
                        apply(
                            &mut e,
                            InventoryEventKind::ReserveOpen {
                                venue: VenueId(1),
                                side,
                                role: InventoryRole::Harvest,
                                evidence: evidence(),
                            },
                        )
                        .unwrap();
                    }
                    for i in order {
                        apply(
                            &mut e,
                            InventoryEventKind::FillOpen {
                                id: i + 1,
                                price: PriceTicks(100),
                                maker: true,
                                charges: Charges::default(),
                            },
                        )
                        .unwrap();
                        assert!(e.exposure().net().abs() <= 2);
                        assert!(e.exposure().gross() <= 4);
                    }
                }
            }
        }
    }
}
#[test]
fn bounded_history_eviction_and_consecutive_adverse_episodes() {
    let mut e = engine();
    mark(&mut e, 100);
    for _ in 0..40 {
        let id = open(&mut e, Side::Buy, 100);
        close(&mut e, id, 101, CloseMode::Normal);
    }
    assert_eq!(e.history_evictions(), (8, 32));
    assert_eq!(e.accounts().realized, Money(400));
    assert_eq!(e.episodes().iter().flatten().count(), 8);
    let mut e = engine();
    mark(&mut e, 100);
    for _ in 0..3 {
        let id = open(&mut e, Side::Buy, 100);
        close(&mut e, id, 99, CloseMode::Normal);
    }
    assert_eq!(e.halt_reason(), Some(risk::KillReason::AdverseEpisodes));
    assert_eq!(e.equity().unwrap(), Money(-30));
}
#[test]
fn malformed_configuration_eligibility_and_duplicate_fill_rejected() {
    let mut c = config();
    c.quantum = Quantum::new(QtyUnits(i64::MAX), 1, 1).unwrap();
    assert!(Engine::new(c).is_err());
    let mut c = config();
    c.venues[2] = c.venues[0];
    assert!(Engine::new(c).is_err());
    let mut e = engine();
    mark(&mut e, 100);
    let mut proof = evidence();
    proof.qualified = false;
    assert!(matches!(
        apply(
            &mut e,
            InventoryEventKind::ReserveOpen {
                venue: VenueId(1),
                side: Side::Buy,
                role: InventoryRole::Harvest,
                evidence: proof
            }
        ),
        Err(InventoryError::Denied(risk::Denial::NotQualified))
    ));
    let id = open(&mut e, Side::Buy, 100);
    let before = e;
    assert_eq!(
        apply(
            &mut e,
            InventoryEventKind::FillOpen {
                id,
                price: PriceTicks(100),
                maker: true,
                charges: Charges::default()
            }
        ),
        Err(InventoryError::UnknownId)
    );
    assert_eq!(e, before);
}
