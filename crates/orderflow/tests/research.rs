use common::*;
use fixed_point::*;
use market_events::{MarketEvent, MarketEventType as K};
use orderflow::{research::*, *};
use simulation::structures::{StructureScenario, generate};
mod support;
fn events() -> Vec<MarketEvent> {
    let mut out = Vec::new();
    generate(
        [VenueId(1), VenueId(2), VenueId(3)],
        [InstrumentId(1); 3],
        StructureScenario::Revisit,
        |e| {
            out.push(e);
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    out
}
#[test]
fn direct_and_binary_replay_match_full_state_after_every_event() {
    use recorder::{Recorder, RecordingMetadata};
    use replay::merged::MergedReplay;
    let input = events();
    let mut writers: Vec<_> = (1..=3)
        .map(|v| {
            Recorder::new(
                Vec::new(),
                RecordingMetadata {
                    venue: VenueId(v),
                    instrument: InstrumentId(1),
                    price_decimals: 0,
                    quantity_decimals: 0,
                    tick_atoms: 1,
                    quantity_atoms: 1,
                },
            )
            .unwrap()
        })
        .collect();
    for e in &input {
        writers[e.venue.0 as usize - 1].append(e).unwrap();
    }
    let data: Vec<_> = writers.into_iter().map(|w| w.finish().unwrap()).collect();
    let mut replay = MergedReplay::new(
        [data[2].as_slice(), data[0].as_slice(), data[1].as_slice()],
        true,
    )
    .unwrap();
    let (mut direct, mut recorded) = (support::engine(), support::engine());
    replay.validate(direct.research().market()).unwrap();
    let mut baseline = liquidity::research::ResearchEngine::<3, 16, 48, 64, 3, 16>::new(
        support::metadata(),
        support::configs(),
        support::lc(),
        support::vc(),
    )
    .unwrap();
    for expected in input {
        let actual = replay.next_event().unwrap().unwrap();
        assert_eq!(expected, actual);
        direct.apply(&expected).unwrap();
        recorded.apply(&actual).unwrap();
        assert_eq!(direct, recorded);
        baseline.apply(&expected).unwrap();
        assert_eq!(direct.research(), &baseline);
    }
    assert!(replay.next_event().unwrap().is_none());
    assert_eq!(direct.research().voids().metrics().revisits, 1);
    for v in 1..=3 {
        let s = direct.snapshot(VenueId(v)).unwrap().unwrap();
        assert_eq!(s.time_totals.get(Metric::Ofi), 0); // Bid 100 removed, then restored.
        assert_eq!(s.time_totals.get(Metric::AddedQty), 500); // Pool +400, restored +100.
        assert_eq!(s.time_totals.get(Metric::RemovedQty), 100);
        assert_eq!(s.cancellation_rate().unwrap(), None);
        assert_eq!(direct.epoch(VenueId(v)).unwrap(), 1);
    }
}
#[test]
fn nonbest_depth_change_affects_depth_delta_but_not_best_quote_ofi() {
    let mut e = support::engine();
    let input = events();
    for event in &input {
        e.apply(event).unwrap();
    }
    let mut event = *input.iter().rev().find(|e| e.venue == VenueId(1)).unwrap();
    event.sequence += 1;
    event.receive_ts.0 = input.last().unwrap().receive_ts.0 + 1;
    event.event_type = K::Modify;
    event.price_ticks = PriceTicks(96);
    event.qty_units = QtyUnits(150);
    event.side = Side::Buy;
    let before = e.snapshot(VenueId(1)).unwrap().unwrap();
    e.apply(&event).unwrap();
    let after = e.snapshot(VenueId(1)).unwrap().unwrap();
    assert_eq!(
        after.time_totals.get(Metric::Ofi),
        before.time_totals.get(Metric::Ofi)
    );
    assert_eq!(
        after.time_totals.get(Metric::DepthDelta),
        before.time_totals.get(Metric::DepthDelta) + 50
    );
    assert_eq!(e.queue_imbalance(VenueId(1), true).unwrap(), Some(0));
    assert_eq!(e.queue_imbalance(VenueId(1), false).unwrap(), Some(111111));
    event.sequence += 1;
    event.receive_ts.0 += 1;
    event.event_type = K::Trade;
    event.qty_units = QtyUnits(7);
    event.price_ticks = PriceTicks(104);
    e.apply(&event).unwrap();
    let s = e.snapshot(VenueId(1)).unwrap().unwrap();
    assert_eq!(
        s.time_totals.get(Metric::Ofi),
        after.time_totals.get(Metric::Ofi)
    );
    assert_eq!(
        s.time_totals.get(Metric::BuyQty),
        after.time_totals.get(Metric::BuyQty) + 7
    );
    assert_eq!(
        s.time_buckets[1].get(Metric::BuyQty),
        after.time_buckets[1].get(Metric::BuyQty) + 7
    );
}
#[test]
fn idle_expiry_staleness_reset_and_bad_sequence_fail_closed() {
    let mut configs = support::configs();
    for c in &mut configs {
        c.stale_after_ns = 100_000_000;
    }
    let mut fc = support::fc();
    fc.time_window_ns = 10_000_000;
    let mut e = support::Engine::new(
        support::metadata(),
        configs,
        support::lc(),
        support::vc(),
        fc,
    )
    .unwrap();
    for event in events() {
        e.apply(&event).unwrap();
    }
    e.advance_time(Timestamp(80_000_000)).unwrap();
    assert_eq!(e.snapshot(VenueId(1)).unwrap().unwrap().time_count, 0);
    e.advance_time(Timestamp(200_000_000)).unwrap();
    assert_eq!(e.snapshot(VenueId(1)).unwrap(), None);
    assert_eq!(e.weighted_time_ofi().unwrap(), (0, 0));
    let mut fresh = support::engine();
    for event in events() {
        fresh.apply(&event).unwrap();
    }
    let mut wrong = events().last().copied().unwrap();
    wrong.sequence += 2;
    wrong.receive_ts.0 += 1;
    assert!(fresh.apply(&wrong).is_err());
    assert!(fresh.faulted());
    assert_eq!(fresh.snapshot(VenueId(1)), Err(FlowResearchError::Faulted));
}
#[test]
fn snapshot_replacement_starts_new_epoch_without_counting_snapshot_depth() {
    let mut e = support::engine();
    let input = events();
    for event in &input {
        e.apply(event).unwrap();
    }
    let seq = input
        .iter()
        .rev()
        .find(|e| e.venue == VenueId(1))
        .unwrap()
        .sequence;
    let now = input.last().unwrap().receive_ts.0;
    for (offset, (kind, side, price, qty)) in [
        (K::SnapshotStart, Side::Buy, 0, 0),
        (K::Add, Side::Buy, 100, 20),
        (K::Add, Side::Sell, 104, 40),
        (K::SnapshotEnd, Side::Buy, 0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let seq = seq + offset as u64 + 1;
        let now = now + offset as u64 + 1;
        e.apply(&MarketEvent {
            venue: VenueId(1),
            instrument: InstrumentId(1),
            sequence: seq,
            exchange_sequence: seq,
            receive_ts: Timestamp(now),
            exchange_ts: now,
            event_type: kind,
            side,
            price_ticks: PriceTicks(price),
            qty_units: QtyUnits(qty),
        })
        .unwrap();
    }
    assert_eq!(e.epoch(VenueId(1)).unwrap(), 2);
    assert_eq!(e.snapshot(VenueId(1)).unwrap().unwrap().event_count, 0);
    assert_eq!(e.queue_imbalance(VenueId(1), true).unwrap(), Some(-333333));
    assert_eq!(e.epoch(VenueId(2)).unwrap(), 1);
}

#[test]
fn normalized_quantity_and_weighted_ofi_preserve_native_units() {
    let mut configs = support::configs();
    configs[0].metadata.quantity.atoms = 2;
    let mut e = support::Engine::new(
        support::metadata(),
        configs,
        support::lc(),
        support::vc(),
        support::fc(),
    )
    .unwrap();
    let input = events();
    for event in &input {
        e.apply(event).unwrap();
    }
    let mut event = *input.iter().rev().find(|e| e.venue == VenueId(1)).unwrap();
    event.sequence += 1;
    event.receive_ts.0 = input.last().unwrap().receive_ts.0 + 1;
    event.event_type = K::Modify;
    event.price_ticks = PriceTicks(100);
    event.qty_units = QtyUnits(150);
    event.side = Side::Buy;
    let before = e.snapshot(VenueId(1)).unwrap().unwrap();
    e.apply(&event).unwrap();
    let after = e.snapshot(VenueId(1)).unwrap().unwrap();
    assert_eq!(
        after.time_totals.get(Metric::Ofi) - before.time_totals.get(Metric::Ofi),
        100
    );
    assert_eq!(e.weighted_time_ofi().unwrap(), (100_000_000, 7));
    assert_eq!(e.queue_imbalance(VenueId(1), true).unwrap(), Some(200_000));
}
#[test]
fn flow_capacity_stops_complete_engine_and_queries() {
    let mut config = support::fc();
    config.event_window = 4;
    let mut e = FlowResearchEngine::<3, 16, 48, 64, 3, 16, 4>::new(
        support::metadata(),
        support::configs(),
        support::lc(),
        support::vc(),
        config,
    )
    .unwrap();
    let failure = events().into_iter().find_map(|event| e.apply(&event).err());
    assert_eq!(failure, Some(FlowResearchError::Flow(FlowError::Capacity)));
    assert!(e.faulted());
    assert_eq!(e.weighted_time_ofi(), Err(FlowResearchError::Faulted));
}
