use book::VenueBook;
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
use recorder::{Recorder, RecordingMetadata};
use replay::{Replay, ReplayMode};
use std::{
    fs::OpenOptions,
    io::{self, BufReader, BufWriter, Write},
    process::ExitCode,
};
const CAPACITY: usize = 256;
mod flow;
mod inventory_demo;
mod multi;
mod scenarios;
mod structures;
fn event(sequence: u64, kind: K, side: Side, price: i64, qty: i64) -> MarketEvent {
    MarketEvent {
        venue: VenueId(1),
        instrument: InstrumentId(1),
        sequence,
        exchange_sequence: sequence,
        exchange_ts: sequence * 1_000_000,
        receive_ts: Timestamp(sequence * 1_000_000),
        event_type: kind,
        side,
        price_ticks: PriceTicks(price),
        qty_units: QtyUnits(qty),
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("scenario-demo") if args.len() == 7 => scenarios::demo(&args[1], &args[2], &args[3], &args[4], &args[5], &args[6])?,
        Some("scenario-replay") if args.len() == 2 => scenarios::replay(&args[1])?,
        Some("scenario-suite") if args.len() == 5 => scenarios::suite(&args[1], &args[2], &args[3], &args[4])?,
        Some("inventory-demo") if args.len() == 3 || args.len() == 4 => inventory_demo::demo(&args[1], &args[2], args.get(3).map(String::as_str).unwrap_or("recovery"))?,
        Some("inventory-replay") if args.len() == 2 => inventory_demo::replay(&args[1])?,
        Some("flow-demo") if args.len() == 5 => flow::demo(&args[1], &args[2], &args[3], &args[4])?,
        Some("flow-replay") if args.len() == 2 => flow::replay(&args[1])?,
        Some("structure-demo") if args.len() == 4 || args.len() == 5 => structures::demo(&args[1], &args[2], &args[3], args.get(4).map(String::as_str).unwrap_or("revisit"))?,
        Some("structure-replay") if args.len() == 2 => structures::replay(&args[1])?,
        Some("multi-demo") if args.len() == 3 => multi::demo(&args[1], &args[2])?,
        Some("multi-replay") if args.len() == 2 => multi::replay(&args[1])?,
        Some("demo") if args.len() == 2 => {
            // create_new never overwrites research data.
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[1])
                .map_err(|e| e.to_string())?;
            let metadata = RecordingMetadata {
                venue: VenueId(1),
                instrument: InstrumentId(1),
                price_decimals: 2,
                quantity_decimals: 6,
                tick_atoms: 1,
                quantity_atoms: 1,
            };
            let mut recorder =
                Recorder::new(BufWriter::new(file), metadata).map_err(|e| e.to_string())?;
            let mut book = VenueBook::<CAPACITY>::new(metadata.venue, metadata.instrument);
            let initial = [
                event(1, K::SnapshotStart, Side::Buy, 0, 0),
                event(2, K::Add, Side::Buy, 6_174_252, 100),
                event(3, K::Add, Side::Sell, 6_174_254, 100),
                event(4, K::SnapshotEnd, Side::Buy, 0, 0),
            ];
            for e in initial.into_iter().chain(
                (5..=10_004)
                    .map(|n| event(n, K::Modify, Side::Buy, 6_174_252, 100 + (n % 7) as i64)),
            ) {
                book.apply(&e).map_err(|e| format!("book: {e:?}"))?;
                recorder.append(&e).map_err(|e| e.to_string())?;
            }
            let writer = recorder.finish().map_err(|e| e.to_string())?;
            writer.get_ref().sync_all().map_err(|e| e.to_string())?;
            println!("Recorded 10004 synthetic events to {}", args[1]);
        }
        Some("replay") if args.len() == 2 || args.len() == 3 => {
            let mode_arg = args.get(2).map(String::as_str).unwrap_or("max");
            let mode = match mode_arg {
                "max" | "step" => ReplayMode::Maximum,
                "1x" => ReplayMode::Paced { speed: 1 },
                "2x" => ReplayMode::Paced { speed: 2 },
                "10x" => ReplayMode::Paced { speed: 10 },
                "100x" => ReplayMode::Paced { speed: 100 },
                _ => return Err("mode must be max, step, 1x, 2x, 10x, or 100x".into()),
            };
            let file = std::fs::File::open(&args[1]).map_err(|e| e.to_string())?;
            let mut replay =
                Replay::new(BufReader::new(file), mode).map_err(|e| format!("replay: {e:?}"))?;
            let metadata = replay.metadata();
            let mut book = VenueBook::<CAPACITY>::new(metadata.venue, metadata.instrument);
            let count = if mode_arg == "step" {
                let mut count = 0;
                loop {
                    print!("Enter to step, q to stop: ");
                    io::stdout().flush().map_err(|e| e.to_string())?;
                    let mut line = String::new();
                    if io::stdin()
                        .read_line(&mut line)
                        .map_err(|e| e.to_string())?
                        == 0
                        || line.trim() == "q"
                    {
                        break;
                    }
                    match replay
                        .step(&mut book)
                        .map_err(|e| format!("replay: {e:?}"))?
                    {
                        Some(e) => {
                            count += 1;
                            println!("{e:?} state={:?}", book.state());
                        }
                        None => break,
                    }
                }
                count
            } else {
                replay
                    .run(&mut book)
                    .map_err(|e| format!("replay: {e:?}"))?
            };
            println!(
                "events={count} state={:?} sequence={:?} bid={:?} ask={:?} midpoint_x2={:?}",
                book.state(),
                book.sequence(),
                book.best(Side::Buy),
                book.best(Side::Sell),
                book.midpoint_x2()
            );
        }
        _ => return Err("usage: lre scenario-demo MARKET STRUCTURE FLOW ENGINE DIRECTORY a|b|c|d|e|f | lre scenario-replay DIRECTORY | lre scenario-suite MARKET STRUCTURE FLOW ENGINE | lre inventory-demo CONFIG DIRECTORY [recovery|forced] | lre inventory-replay DIRECTORY | lre flow-demo MARKET_CONFIG STRUCTURE_CONFIG FLOW_CONFIG DIRECTORY | lre flow-replay DIRECTORY | lre demo FILE | lre replay FILE [max|step|1x|2x|10x|100x] | lre multi-demo CONFIG DIRECTORY | lre multi-replay DIRECTORY | lre structure-demo MARKET_CONFIG STRUCTURE_CONFIG DIRECTORY [revisit|local|refill|continuation] | lre structure-replay DIRECTORY".into()),
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
