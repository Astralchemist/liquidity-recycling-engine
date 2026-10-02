//! Shared live/replay path: recorded market streams reproduce the complete engine state and the
//! exact inventory command journal, and the journal alone reproduces the ledger.
use engine::fixtures::*;
use inventory::{Ledger, journal};
use recorder::{Recorder, RecordingMetadata};
use replay::merged::MergedReplay;
use simulation::{Scenario, scenarios::generate};
mod support;
use support::*;
#[test]
fn recorded_market_replay_reproduces_engine_state_and_command_journal() {
    let (_, venues) = market();
    for scenario in Scenario::ALL {
        let events = generate(scenario);
        let direct = run_events(&events, engine());
        let mut writers = venues.map(|c| {
            Recorder::new(
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
        for e in &events {
            let v = venues.iter().position(|c| c.venue == e.venue).unwrap();
            writers[v].append(e).unwrap();
        }
        let streams = writers.map(|w| w.finish().unwrap());
        let mut merged = MergedReplay::new(streams.each_ref().map(|s| s.as_slice()), true).unwrap();
        let mut replayed = build_with(engine());
        let mut journal = Vec::new();
        while let Some(e) = merged.next_event().unwrap() {
            replayed.apply(&e, &mut |c| journal.push(*c)).unwrap();
        }
        assert_eq!(*replayed, *direct.engine, "scenario {}", scenario.letter());
        assert_eq!(journal, direct.journal);
        // The binary inventory journal alone rebuilds the identical ledger.
        let mut writer = journal::Writer::new(Vec::new(), 7).unwrap();
        for c in &journal {
            writer.append(*c).unwrap();
        }
        let bytes = writer.finish().unwrap();
        let mut reader = journal::Reader::new(bytes.as_slice(), 7).unwrap();
        let mut ledger = Ledger::<3, 32, 8>::new(inventory()).unwrap();
        while let Some(c) = reader.next_event().unwrap() {
            ledger.apply(c).unwrap();
        }
        assert_eq!(ledger, *direct.engine.ledger());
    }
}
