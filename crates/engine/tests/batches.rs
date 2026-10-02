//! Native feeds deliver multi-level messages. Delivering scenario A with every run of
//! consecutive same-venue depth updates as ONE atomic batch must keep research state
//! incremental: no rebuild, no void invalidation, and the same final books as single events.
use engine::fixtures::*;
use market_events::{MarketEvent, MarketEventType as K};
use simulation::{Scenario, scenarios::generate};
mod support;
use support::*;
/// Groups maximal runs of live depth updates on one venue with distinct levels. Members take
/// the first member's receive time and native update ID; canonical sequences stay contiguous.
fn batched(events: &[MarketEvent]) -> Vec<Vec<MarketEvent>> {
    let mut out: Vec<Vec<MarketEvent>> = Vec::new();
    let mut building = [false; 4];
    for e in events {
        let v = usize::from(e.venue.0);
        match e.event_type {
            K::SnapshotStart => building[v] = true,
            K::SnapshotEnd => building[v] = false,
            _ => {}
        }
        let depth = matches!(e.event_type, K::Add | K::Modify | K::Cancel) && !building[v];
        if let Some(last) = out.last_mut() {
            let head = last[0];
            let joinable = depth
                && matches!(head.event_type, K::Add | K::Modify | K::Cancel)
                && head.venue == e.venue
                && last.len() < 64
                && last.last().is_some_and(|x| x.sequence + 1 == e.sequence)
                && !last
                    .iter()
                    .any(|x| x.side == e.side && x.price_ticks == e.price_ticks);
            if joinable {
                let mut m = *e;
                m.receive_ts = head.receive_ts;
                m.exchange_ts = head.exchange_ts;
                m.exchange_sequence = head.exchange_sequence;
                last.push(m);
                continue;
            }
        }
        out.push(vec![*e]);
    }
    out
}
#[test]
fn atomic_batches_keep_research_incremental_and_books_identical() {
    let events = generate(Scenario::RevisitOscillation);
    let groups = batched(&events);
    let multi = groups.iter().filter(|g| g.len() > 1).count();
    assert!(multi > 100, "fixture produced too few batches");
    let mut engine = build_with(engine());
    for g in &groups {
        if g.len() == 1 {
            engine.apply(&g[0], &mut |_| {}).unwrap();
        } else {
            engine
                .apply_depth_batch(g[0].venue, g, &mut |_| {})
                .unwrap();
        }
        check_invariants(&engine);
    }
    let single = run(Scenario::RevisitOscillation);
    let (b, s) = (
        engine.research().research(),
        single.engine.research().research(),
    );
    // Only the three snapshot activations are structural, exactly as with single events.
    assert_eq!(
        b.market().structural_revision(),
        s.market().structural_revision()
    );
    assert_eq!(b.voids().metrics().invalidated, 0);
    let zone = b.voids().zones().iter().flatten().next().unwrap();
    assert!(zone.registered_at.is_some() && zone.revisit_count >= 1);
    for side in [common::Side::Buy, common::Side::Sell] {
        assert_eq!(
            b.market().levels(side).unwrap(),
            s.market().levels(side).unwrap()
        );
    }
    assert_eq!(engine.metrics().batches, multi as u64);
    assert_eq!(engine.metrics().market_events, events.len() as u64);
}
#[test]
fn version_two_recordings_replay_batched_feeds_exactly() {
    use recorder::{FrameRecorder, RecordingMetadata};
    use replay::merged::{MergedFrame, MergedFrames};
    let (_, venues) = market();
    let groups = batched(&generate(Scenario::ToxicRecovery));
    let mut direct = build_with(engine());
    let mut journal = Vec::new();
    let mut writers = venues.map(|c| {
        FrameRecorder::new(
            Vec::new(),
            RecordingMetadata {
                venue: c.venue,
                instrument: c.metadata.instrument,
                price_decimals: c.metadata.price.decimals,
                quantity_decimals: c.metadata.quantity.decimals,
                tick_atoms: c.metadata.price.atoms,
                quantity_atoms: c.metadata.quantity.atoms,
            },
        )
        .unwrap()
    });
    for g in &groups {
        let v = usize::from(g[0].venue.0 - 1);
        if g.len() == 1 {
            direct.apply(&g[0], &mut |c| journal.push(*c)).unwrap();
            writers[v].append_event(&g[0]).unwrap();
        } else {
            direct
                .apply_depth_batch(g[0].venue, g, &mut |c| journal.push(*c))
                .unwrap();
            writers[v].append_batch(g).unwrap();
        }
    }
    let streams = writers.map(|w| w.finish().unwrap());
    let mut merged = MergedFrames::new(streams.each_ref().map(|s| s.as_slice()), true).unwrap();
    merged
        .validate(direct.research().research().market())
        .unwrap();
    let mut replayed = build_with(engine());
    let mut replayed_journal = Vec::new();
    while let Some(frame) = merged.next_frame().unwrap() {
        match frame {
            MergedFrame::Event(e) => replayed.apply(&e, &mut |c| replayed_journal.push(*c)),
            MergedFrame::Batch(b) => {
                replayed.apply_depth_batch(b[0].venue, b, &mut |c| replayed_journal.push(*c))
            }
        }
        .unwrap();
    }
    assert_eq!(*replayed, *direct);
    assert_eq!(replayed_journal, journal);
    assert!(direct.metrics().batches > 100);
}
