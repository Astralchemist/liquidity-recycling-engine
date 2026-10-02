//! Phase 9 execution simulation through the full engine: queue fills justified by prints,
//! injected cancellations attributed by each model, exact fee schedules, markout and funding
//! oracles, the J(a) gate, configuration validation and determinism.
use book::BookState;
use common::{Side, VenueId};
use engine::{fixtures::*, *};
use execution::{
    fees::FeeSchedule,
    markout::{MAX_HORIZONS, MarkoutConfig},
    queue::CancelModel,
};
use fixed_point::{Money, PriceTicks, QtyUnits};
use inventory::{InventoryEvent, InventoryEventKind as Cmd, Ledger};
use market_events::{MarketEvent, MarketEventType as K};
use simulation::{Scenario, scenarios::generate};
mod support;
use support::*;
const Q: i128 = 10; // one child: 1/10 of a 100-unit parent
const MODELS: [FillModel; 4] = [
    FillModel::StrictTradeThrough,
    FillModel::Queue(CancelModel::Pessimistic),
    FillModel::Queue(CancelModel::Proportional),
    FillModel::Queue(CancelModel::Optimistic),
];
fn with_model(model: FillModel) -> EngineConfig {
    let mut c = engine();
    c.fills.model = model;
    c
}
/// A maker fill observed in a step: the order as it rested BEFORE the event.
#[derive(Debug, Clone, Copy)]
struct Fill {
    time: u64,
    order: RestingOrder,
    mid_x2: Option<i128>,
}
struct Trace {
    run: Run,
    fills: Vec<Fill>,
    /// (time, reference midpoint) after every event.
    mids: Vec<(u64, Option<i128>)>,
}
/// Runs `events` one at a time, checking every Phase 9 fill against the print that caused it.
fn trace(events: &[MarketEvent], config: EngineConfig) -> Trace {
    let mut engine = build_with(config);
    let mut journal: Vec<InventoryEvent> = Vec::new();
    let mut fills = Vec::new();
    let mut mids = Vec::new();
    for event in events {
        let before = *engine.orders();
        let start = journal.len();
        engine.apply(event, &mut |c| journal.push(*c)).unwrap();
        check_invariants(&engine);
        for command in &journal[start..] {
            let (id, price, maker, charges) = match command.kind {
                Cmd::FillOpen {
                    id,
                    price,
                    maker,
                    charges,
                }
                | Cmd::FillClose {
                    id,
                    price,
                    maker,
                    charges,
                } => (id, price, maker, charges),
                _ => continue,
            };
            if !maker {
                continue;
            }
            let o = before
                .iter()
                .flatten()
                .find(|o| o.id == id)
                .copied()
                .unwrap();
            assert_eq!(o.price, price, "maker fills execute at the order price");
            assert!(charges.validate().is_ok());
            assert_eq!(event.event_type, K::Trade, "only prints fill");
            assert_eq!(event.venue, o.venue);
            assert_ne!(event.side, o.side, "the aggressor hits the opposite side");
            let through = match o.side {
                Side::Buy => event.price_ticks < o.price,
                Side::Sell => event.price_ticks > o.price,
            };
            match o.queue {
                None => assert!(through, "strict fills need a trade-through"),
                Some(q) => assert!(
                    through
                        || (event.price_ticks == o.price
                            && event.qty_units.0 >= q.ahead + q.remaining()),
                    "a fill at our price needs the queue ahead plus our size to trade"
                ),
            }
            fills.push(Fill {
                time: event.receive_ts.0,
                order: o,
                mid_x2: engine.reference_mid_x2(),
            });
        }
        // No entry or rebalance is placed at a price a print has already traded through.
        for o in engine.orders().iter().flatten() {
            let new = !before
                .iter()
                .flatten()
                .any(|b| b.id == o.id && b.kind == o.kind);
            if new && o.kind != OrderKind::Harvest {
                assert!(!engine.quote_is_stale(o.venue, o.side, o.price));
            }
        }
        mids.push((event.receive_ts.0, engine.reference_mid_x2()));
    }
    let run = Run {
        engine,
        journal,
        steps: Vec::new(),
    };
    Trace { run, fills, mids }
}
#[test]
fn every_simulated_fill_is_justified_by_its_print() {
    for scenario in Scenario::ALL {
        let events = generate(scenario);
        for model in MODELS {
            let t = trace(&events, with_model(model));
            let m = t.run.engine.metrics();
            assert_eq!(
                t.fills.len() as u64,
                m.maker_fills,
                "{scenario:?} {model:?}"
            );
            if matches!(model, FillModel::Queue(_)) {
                assert_eq!(
                    m.queue_fills_at_price + m.queue_fills_through,
                    m.maker_fills
                );
            } else {
                assert_eq!((m.queue_fills_at_price, m.partial_prints), (0, 0));
            }
        }
    }
    // The queue model fills at the touch when the queue ahead trades; strict needs a sweep.
    let events = generate(Scenario::GlobalVoid);
    let strict = trace(&events, with_model(MODELS[0])).run.engine.metrics();
    let queue = trace(&events, with_model(MODELS[1])).run.engine.metrics();
    assert!(queue.queue_fills_at_price > 0);
    assert!(queue.maker_fills > strict.maker_fills);
}
/// Inserts two depth changes (no prints) at a queued entry's level: others join behind it,
/// then part of the level is cancelled. Later events on that venue are renumbered.
#[test]
fn cancel_models_attribute_an_injected_cancellation() {
    let events = generate(Scenario::RevisitOscillation);
    let config = with_model(FillModel::Queue(CancelModel::Pessimistic));
    // Find the first step after which an entry rests with at least 4 units ahead.
    let mut engine = build_with(config);
    let mut found = None;
    for (k, event) in events.iter().enumerate() {
        engine.apply(event, &mut |_| {}).unwrap();
        let resting = engine.orders().iter().flatten().find(|o| {
            matches!(o.kind, OrderKind::Entry { .. })
                && o.queue
                    .is_some_and(|q| q.ahead >= 4 && q.pending_execution == 0)
        });
        if let Some(o) = resting {
            let v = (o.venue.0 - 1) as usize;
            let shown = engine.displayed(v, o.side, o.price).unwrap().unwrap();
            found = Some((k, *o, shown));
            break;
        }
    }
    let (k, order, shown) = found.expect("a queued entry with depth ahead");
    let ahead = order.queue.unwrap().ahead;
    let (joined, cancelled) = (ahead, ahead / 2);
    let previous = events[..=k]
        .iter()
        .rev()
        .find(|e| e.venue == order.venue)
        .copied()
        .unwrap();
    let modify = |offset: u64, qty: i64| MarketEvent {
        sequence: previous.sequence + offset,
        receive_ts: events[k].receive_ts,
        event_type: K::Modify,
        side: order.side,
        price_ticks: order.price,
        qty_units: QtyUnits(qty),
        ..previous
    };
    let mut stream = events[..=k].to_vec();
    stream.push(modify(1, shown + joined));
    stream.push(modify(2, shown + joined - cancelled));
    for e in &events[k + 1..] {
        let mut e = *e;
        if e.venue == order.venue {
            e.sequence += 2;
        }
        stream.push(e);
    }
    let level = i128::from(shown + joined);
    let expected = [
        (CancelModel::Pessimistic, ahead),
        (
            CancelModel::Proportional,
            ahead - (i128::from(ahead) * i128::from(cancelled) / level) as i64,
        ),
        (CancelModel::Optimistic, ahead - cancelled),
    ];
    for (model, want) in expected {
        let mut engine = build_with(with_model(FillModel::Queue(model)));
        for e in &stream[..=k + 2] {
            engine.apply(e, &mut |_| {}).unwrap();
            check_invariants(&engine);
        }
        let o = engine
            .orders()
            .iter()
            .flatten()
            .find(|o| o.id == order.id)
            .copied()
            .expect("the entry still rests");
        assert_eq!(o.queue.unwrap().ahead, want, "{model:?}");
        // The rest of the stream stays valid and every invariant holds.
        for e in &stream[k + 3..] {
            engine.apply(e, &mut |_| {}).unwrap();
            check_invariants(&engine);
        }
    }
}
/// A sell print below the bid, injected before the step that would place a new bid entry on
/// that venue, defers the entry until the venue's next depth update.
#[test]
fn a_print_through_the_touch_defers_quotes_until_the_next_depth_update() {
    let events = generate(Scenario::RevisitOscillation);
    let mut probe = build_with(engine());
    let mut found = None;
    for (k, event) in events.iter().enumerate() {
        let before = *probe.orders();
        probe.apply(event, &mut |_| {}).unwrap();
        let placed = probe.orders().iter().flatten().copied().find(|o| {
            matches!(o.kind, OrderKind::Entry { .. })
                && o.side == Side::Buy
                && !before.iter().flatten().any(|b| b.id == o.id)
        });
        let Some(o) = placed else { continue };
        let depth_here = event.venue == o.venue && event.event_type != K::Trade;
        let bids_resting = before
            .iter()
            .flatten()
            .any(|b| b.venue == o.venue && b.side == Side::Buy);
        if !depth_here && !bids_resting {
            found = Some((k, o));
            break;
        }
    }
    let (k, order) = found.expect("a bid entry placed on a step without depth on its venue");
    let previous = *events[..k]
        .iter()
        .rev()
        .find(|e| e.venue == order.venue)
        .unwrap();
    let print = MarketEvent {
        sequence: previous.sequence + 1,
        receive_ts: events[k - 1].receive_ts,
        event_type: K::Trade,
        side: Side::Sell,
        price_ticks: PriceTicks(order.price.0 - 1),
        qty_units: QtyUnits(1),
        ..previous
    };
    let mut stream = events[..k].to_vec();
    stream.push(print);
    for e in &events[k..] {
        let mut e = *e;
        if e.venue == order.venue {
            e.sequence += 1;
        }
        stream.push(e);
    }
    let mut engine = build_with(engine());
    for e in &stream[..=k + 1] {
        engine.apply(e, &mut |_| {}).unwrap();
        check_invariants(&engine);
    }
    assert!(engine.quote_is_stale(order.venue, Side::Buy, order.price));
    assert!(engine.metrics().stale_touch_skips > 0);
    assert!(!engine.orders().iter().flatten().any(|o| {
        matches!(o.kind, OrderKind::Entry { .. }) && o.venue == order.venue && o.side == Side::Buy
    }));
    // The next depth update on that venue clears the condition; the full stream stays valid
    // and every placement passes the trace checks.
    let next_depth = stream[k + 2..]
        .iter()
        .position(|e| e.venue == order.venue && e.event_type != K::Trade)
        .unwrap()
        + k
        + 2;
    for e in &stream[k + 2..=next_depth] {
        engine.apply(e, &mut |_| {}).unwrap();
    }
    assert!(!engine.quote_is_stale(order.venue, Side::Buy, order.price));
    let t = trace(&stream, engine::fixtures::engine());
    assert!(t.run.engine.metrics().entries_placed > 0);
}
/// Live regression (Phase 9 session): every venue fell silent for just over the staleness
/// limit; the first frame afterwards passed a check of the CURRENT book state, then `apply`
/// advanced the clock, withdrew that very book and faulted the engine. `accepts` must refuse it.
#[test]
fn input_after_a_silent_gap_is_refused_before_it_can_fault_the_engine() {
    let events = generate(Scenario::RevisitOscillation);
    let stale_after = market().1[0].stale_after_ns;
    let venue = VenueId(1);
    let mut engine = build_with(engine());
    let (mut k, mut last_depth) = (0, 0);
    for (i, e) in events.iter().enumerate() {
        engine.apply(e, &mut |_| {}).unwrap();
        if e.venue == venue && e.event_type != K::Trade {
            last_depth = e.receive_ts.0;
        }
        if i > events.len() / 2 && e.venue == venue && e.event_type != K::Trade {
            k = i;
            break;
        }
    }
    // The very next event on that venue, so its sequence is contiguous.
    let next = *events[k + 1..].iter().find(|e| e.venue == venue).unwrap();
    let book = engine
        .research()
        .research()
        .market()
        .venue_book(venue)
        .unwrap();
    assert_eq!(book.state(), BookState::Live);
    let late = common::Timestamp(last_depth + stale_after + 1);
    let edge = common::Timestamp(last_depth + stale_after);
    assert!(engine.accepts(venue, edge, false));
    assert!(!engine.accepts(venue, late, false));
    assert!(
        engine.accepts(venue, late, true),
        "a snapshot always restores a venue"
    );
    // Without the check, the same input faults the engine.
    let mut unchecked = engine.clone();
    let event = MarketEvent {
        receive_ts: late,
        ..next
    };
    unchecked.apply(&event, &mut |_| {}).unwrap();
    let fault = format!("{:?}", unchecked.research_fault());
    assert!(
        fault.contains("SnapshotRequired"),
        "{fault} after {:?}",
        next.event_type
    );
    assert!(unchecked.ledger().halt_reason().is_some());
    // After a research fault, input is ignored and therefore accepted.
    assert!(unchecked.accepts(venue, late, false));
}
fn ceil_ppm(notional: i128, ppm: u32) -> i128 {
    let n = notional * i128::from(ppm);
    n.div_euclid(1_000_000) + i128::from(n.rem_euclid(1_000_000) != 0)
}
#[test]
fn fee_schedules_charge_every_fill_exactly() {
    let schedule = FeeSchedule {
        maker_fee_ppm: 2_000,
        maker_rebate_ppm: 300,
        taker_fee_ppm: 5_000,
    };
    for scenario in [Scenario::RevisitOscillation, Scenario::OneWayContinuation] {
        for model in [MODELS[0], MODELS[2]] {
            let mut config = with_model(model);
            config.fills.schedule = schedule;
            let f = config.fills;
            let run = run_with(scenario, config);
            let mut checked = 0;
            let mut expected_close = std::collections::HashMap::new();
            for c in &run.journal {
                match c.kind {
                    Cmd::ReserveClose {
                        id,
                        expected_charges,
                        ..
                    } => {
                        expected_close.insert(id, expected_charges);
                    }
                    Cmd::FillOpen {
                        price,
                        maker,
                        charges,
                        ..
                    }
                    | Cmd::FillClose {
                        price,
                        maker,
                        charges,
                        ..
                    } => {
                        let n = i128::from(price.0) * Q;
                        let (fee, rebate) = if maker {
                            (
                                f.maker_fee.0 + ceil_ppm(n, schedule.maker_fee_ppm),
                                f.maker_rebate.0 + n * 300 / 1_000_000,
                            )
                        } else {
                            (f.taker_fee.0 + ceil_ppm(n, schedule.taker_fee_ppm), 0)
                        };
                        assert_eq!((charges.fee.0, charges.rebate.0), (fee, rebate));
                        checked += 1;
                    }
                    _ => {}
                }
                // A close fills with exactly the charges its reservation assumed.
                if let Cmd::FillClose { id, charges, .. } = c.kind {
                    assert_eq!(expected_close.get(&id), Some(&charges));
                }
            }
            assert!(checked > 0);
            let m = run.engine.metrics();
            let a = run.engine.ledger().accounts();
            assert_eq!(a.fees.0, m.maker_fees + m.taker_fees);
            assert_eq!(a.rebates.0, m.rebates);
            assert!(m.maker_notional > 0);
            if scenario == Scenario::OneWayContinuation {
                assert!(m.taker_fills > 0 && m.taker_fees > 0 && m.taker_notional > 0);
            }
            // Net maker yield is the harvest account plus open P&L over maker notional.
            let net = a.harvest().unwrap().0 + run.engine.ledger().unrealized().0;
            assert_eq!(
                run.engine.net_maker_yield_ppm().unwrap(),
                Some(net * 1_000_000 / m.maker_notional)
            );
        }
    }
}
#[test]
fn markouts_match_an_independent_oracle() {
    for (scenario, model) in [
        (Scenario::RevisitOscillation, MODELS[0]),
        (Scenario::ToxicRecovery, MODELS[2]),
        (Scenario::OneWayContinuation, MODELS[3]),
    ] {
        let t = trace(&generate(scenario), with_model(model));
        let tracker = t.run.engine.markouts();
        let horizons = tracker.config();
        assert_eq!(tracker.fills, t.fills.len() as u64);
        for (h, stats) in tracker.stats().iter().enumerate() {
            let horizon = horizons.horizons_ns[h];
            let (mut samples, mut missing, mut sum, mut sum_sq, mut adverse, mut drift) =
                (0_u64, 0_u64, 0_i128, 0_i128, 0_u64, 0_i128);
            for f in &t.fills {
                let Some(&(_, mid)) = t.mids.iter().find(|(time, _)| *time >= f.time + horizon)
                else {
                    continue;
                };
                let Some(mid) = mid else {
                    missing += 1;
                    continue;
                };
                let p2 = 2 * i128::from(f.order.price.0);
                let (m, d) = match f.order.side {
                    Side::Buy => (mid - p2, f.mid_x2.map(|at| mid - at)),
                    Side::Sell => (p2 - mid, f.mid_x2.map(|at| at - mid)),
                };
                samples += 1;
                sum += m * Q;
                sum_sq += m * m;
                adverse += u64::from(m < 0);
                drift += d.unwrap() * Q;
            }
            assert_eq!(
                (
                    stats.samples,
                    stats.missing,
                    stats.sum_x2,
                    stats.sum_sq_x2,
                    stats.adverse,
                    stats.sum_drift_x2
                ),
                (samples, missing, sum, sum_sq, adverse, drift),
                "{scenario:?} horizon {h}"
            );
            // Synthetic paths are a few seconds long: the 1 s and 5 s horizons may stay pending.
            assert!(samples > 0 || horizon > 500_000_000);
        }
    }
}
#[test]
fn funding_charges_every_held_child_at_each_boundary() {
    let interval = 50_000_000;
    for (scenario, rate) in [
        (Scenario::RevisitOscillation, 1_234),
        (Scenario::OneWayContinuation, -9_870),
    ] {
        let mut config = engine();
        config.funding = FundingConfig {
            rate_ppm: rate,
            interval_ns: interval,
        };
        let events = generate(scenario);
        let run = run_events(&events, config);
        // Replay the journal; check each funding command against the replayed ledger state.
        let mut ledger = Ledger::<3, 32, 8>::new(inventory()).unwrap();
        let mut charged = 0;
        let mut boundary_lots = std::collections::HashMap::<u64, usize>::new();
        for c in &run.journal {
            if let Cmd::Funding { id, cost } = c.kind {
                let lot = ledger
                    .lots()
                    .iter()
                    .flatten()
                    .find(|l| l.id == id)
                    .copied()
                    .unwrap();
                let mark = ledger.mark(lot.venue).unwrap().unwrap();
                let sign = if lot.side == Side::Buy { 1 } else { -1 };
                let n =
                    sign * (i128::from(mark.bid.0) + i128::from(mark.ask.0)) * Q * i128::from(rate);
                let want = n.div_euclid(2_000_000) + i128::from(n.rem_euclid(2_000_000) != 0);
                assert_eq!(cost, Money(want));
                // Payments round up, receipts round toward zero.
                assert!(cost.0 * 2_000_000 >= n);
                charged += cost.0;
                *boundary_lots.entry(c.timestamp.0).or_default() += 1;
                if *boundary_lots.get(&c.timestamp.0).unwrap() == 1 {
                    // Every held child is charged at the boundary (no zero-cost case here).
                    assert_eq!(ledger.lots().iter().flatten().count(), {
                        run.journal
                            .iter()
                            .filter(|x| {
                                x.timestamp == c.timestamp && matches!(x.kind, Cmd::Funding { .. })
                            })
                            .count()
                    });
                }
            }
            ledger.apply(*c).unwrap();
        }
        assert_eq!(ledger, *run.engine.ledger());
        assert!(
            !boundary_lots.is_empty(),
            "{scenario:?} held inventory across a boundary"
        );
        // Funding happens only where the engine clock crossed a boundary.
        for &t in boundary_lots.keys() {
            let previous = events
                .iter()
                .map(|e| e.receive_ts.0)
                .filter(|&e| e < t)
                .max()
                .unwrap();
            assert!(t / interval > previous / interval);
        }
        let m = run.engine.metrics();
        assert_eq!(m.funding_cost, charged);
        assert_eq!(run.engine.ledger().accounts().funding.0, charged);
        assert_eq!(
            m.funding_events as usize,
            boundary_lots.values().sum::<usize>()
        );
    }
}
fn objective(min_edge_x2: i128) -> ObjectiveConfig {
    ObjectiveConfig {
        w_rebate: 1_000,
        w_spread: 1_000,
        w_rebalance: 1_000,
        w_adverse: 1_000,
        w_inventory: 1_000,
        w_queue: 1_000,
        adverse_horizon: 1,
        adverse_min_samples: 5,
        adverse_prior_x2: 1,
        inventory_risk_x2: 1,
        queue_cost_x2: 1,
        min_edge_x2,
    }
}
#[test]
fn the_objective_gates_entries_and_values_queue_position() {
    // An unreachable edge: nothing is ever quoted.
    let mut config = engine();
    config.objective = Some(objective(i128::MAX / 4_000));
    let run = run_with(Scenario::RevisitOscillation, config);
    let m = run.engine.metrics();
    assert_eq!((m.entries_placed, m.maker_fills), (0, 0));
    assert!(m.objective_holds > 0);
    // Zero weights and zero edge: quoting is allowed, but moving never beats keeping the queue.
    let zero = ObjectiveConfig {
        w_rebate: 0,
        w_spread: 0,
        w_rebalance: 0,
        w_adverse: 0,
        w_inventory: 0,
        w_queue: 0,
        ..objective(0)
    };
    let mut config = with_model(FillModel::Queue(CancelModel::Proportional));
    config.objective = Some(zero);
    let run = run_with(Scenario::RevisitOscillation, config);
    let m = run.engine.metrics();
    assert!(m.entries_placed > 0 && m.objective_keeps > 0);
    assert_eq!((m.reprices, m.objective_cancels), (0, 0));
}
/// Recomputes J from public state at every step and compares the engine's value.
#[test]
fn entry_utility_matches_a_recomputation_from_public_state() {
    let mut config = with_model(FillModel::Queue(CancelModel::Pessimistic));
    config.fills.schedule = FeeSchedule {
        maker_fee_ppm: 200,
        maker_rebate_ppm: 0,
        taker_fee_ppm: 500,
    };
    let o = ObjectiveConfig {
        w_rebate: 1_000,
        w_spread: 900,
        w_rebalance: 500,
        w_adverse: 1_200,
        w_inventory: 700,
        w_queue: 300,
        // Permissive edge so that entries rest and the keep path is valued too.
        ..objective(-(1 << 60))
    };
    config.objective = Some(o);
    let f = config.fills;
    let mut engine = build_with(config);
    let (mut compared, mut resting, mut ahead_of_back) = (0, 0, 0);
    for event in generate(Scenario::RevisitOscillation) {
        engine.apply(&event, &mut |_| {}).unwrap();
        let venue = VenueId(1);
        let book = engine
            .research()
            .research()
            .market()
            .venue_book(venue)
            .unwrap();
        if book.state() != BookState::Live {
            continue;
        }
        let (Some(bid), Some(ask)) = (
            book.best(Side::Buy).unwrap(),
            book.best(Side::Sell).unwrap(),
        ) else {
            continue;
        };
        // J recomputed from public state; `at_level` is the queue at our price.
        let want = |side: Side, price: PriceTicks, at_level: i64| -> i128 {
            let mid = engine.reference_mid_x2().unwrap();
            let n = i128::from(price.0) * Q;
            let maker_fee = f.maker_fee.0 + ceil_ppm(n, 200);
            let rebate = 2 * (f.maker_rebate.0 - maker_fee);
            let p2 = 2 * i128::from(price.0);
            let spread = if side == Side::Buy {
                mid - p2
            } else {
                p2 - mid
            } * Q;
            let net = engine.ledger().exposure().net();
            let after = net + if side == Side::Buy { 1 } else { -1 };
            let (d0, d1) = (net.abs(), after.abs());
            let rebalance = if d1 < d0 {
                2 * (f.taker_fee.0 + ceil_ppm(n, 500)) + i128::from(ask.price.0 - bid.price.0) * Q
            } else {
                0
            };
            let inventory = if d1 > d0 { i128::from(d1) * Q } else { 0 };
            let adverse = engine.markouts().stats()[1]
                .adverse_x2_per_unit(5)
                .unwrap_or(1)
                * Q;
            let better: i64 = book
                .levels(side)
                .unwrap()
                .iter()
                .take_while(|l| match side {
                    Side::Buy => l.price > price,
                    Side::Sell => l.price < price,
                })
                .map(|l| l.qty.0)
                .sum();
            let queue = i128::from(better + at_level);
            1_000 * rebate + 900 * spread + 500 * rebalance
                - 1_200 * adverse
                - 700 * inventory
                - 300 * queue
        };
        // A new order one tick behind the touch joins behind that level's displayed size.
        for (side, price) in [(Side::Buy, bid.price), (Side::Sell, ask.price)] {
            let price = PriceTicks(price.0 - if side == Side::Buy { 1 } else { -1 });
            let Some(j) = engine.entry_utility(venue, side, price).unwrap() else {
                continue;
            };
            let at = book.level(side, price).unwrap().map_or(0, |l| l.qty.0);
            assert_eq!(j, want(side, price, at));
            compared += 1;
        }
        // A resting order is valued at its tracked position, not at the back of its level.
        for o in engine
            .orders()
            .iter()
            .flatten()
            .filter(|o| o.venue == venue)
        {
            let Some(j) = engine.resting_utility(o).unwrap() else {
                continue;
            };
            assert_eq!(j, want(o.side, o.price, o.queue.unwrap().ahead));
            let shown = book.level(o.side, o.price).unwrap().map_or(0, |l| l.qty.0);
            resting += 1;
            ahead_of_back += u64::from(o.queue.unwrap().ahead < shown);
        }
    }
    assert!(resting > 0 && ahead_of_back > 0);
    assert!(compared > 1_000);
}
#[test]
fn invalid_execution_configuration_is_rejected() {
    let (grid, mut venues) = market();
    let (liquidity, voids) = structures();
    let mut bad = [engine(); 6];
    bad[0].fills.schedule.maker_fee_ppm = 100_001;
    bad[1].funding.rate_ppm = -100_001;
    bad[2].markout = MarkoutConfig {
        horizons_ns: [0; MAX_HORIZONS],
        count: 0,
    };
    bad[3].markout.horizons_ns[1] = bad[3].markout.horizons_ns[0];
    bad[4].objective = Some(ObjectiveConfig {
        adverse_horizon: MAX_HORIZONS,
        ..objective(0)
    });
    bad[5].objective = Some(ObjectiveConfig {
        queue_cost_x2: -1,
        ..objective(0)
    });
    for config in bad {
        assert_eq!(
            SyntheticEngine::new(grid, venues, liquidity, voids, flow(), inventory(), config).err(),
            Some(EngineError::InvalidConfig)
        );
    }
    // A venue quoting in tenths of the grid tick cannot host the queue model.
    venues[1].metadata.price.decimals = 1;
    let strict = SyntheticEngine::new(
        grid,
        venues,
        liquidity,
        voids,
        flow(),
        inventory(),
        engine(),
    );
    assert!(strict.is_ok());
    let queue = with_model(MODELS[1]);
    assert_eq!(
        SyntheticEngine::new(grid, venues, liquidity, voids, flow(), inventory(), queue).err(),
        Some(EngineError::InvalidConfig)
    );
}
#[test]
fn phase9_runs_are_deterministic_and_journals_rebuild_the_ledger() {
    let mut config = with_model(FillModel::Queue(CancelModel::Proportional));
    config.fills.schedule = FeeSchedule {
        maker_fee_ppm: 200,
        maker_rebate_ppm: 50,
        taker_fee_ppm: 500,
    };
    config.funding = FundingConfig {
        rate_ppm: 100,
        interval_ns: 80_000_000,
    };
    config.objective = Some(objective(-1_000));
    for scenario in Scenario::ALL {
        let a = run_with(scenario, config);
        let b = run_with(scenario, config);
        assert!(*a.engine == *b.engine, "{scenario:?}");
        assert_eq!(a.journal, b.journal);
        let mut ledger = Ledger::<3, 32, 8>::new(inventory()).unwrap();
        for c in &a.journal {
            ledger.apply(*c).unwrap();
        }
        assert_eq!(ledger, *a.engine.ledger());
    }
}
