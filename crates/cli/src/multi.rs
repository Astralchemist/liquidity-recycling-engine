use common::{InstrumentId, Side, Timestamp, VenueId};
use consolidator::{Consolidator, VenueConfig, normalization::*};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
use recorder::{Recorder, RecordingMetadata};
use replay::merged::MergedReplay;
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::Path,
};
type Engine = Consolidator<3, 128, 384>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    shared_clock_domain: bool,
    market: Market,
    grid: Grid,
    venues: Vec<Venue>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Market {
    base_asset: u32,
    quote_asset: u32,
    settlement_asset: u32,
    kind: String,
    equivalence_group: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grid {
    instrument: u32,
    price_atoms: i64,
    price_decimals: u8,
    quantity_atoms: i64,
    quantity_decimals: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Venue {
    id: u16,
    instrument: u32,
    price_atoms: i64,
    price_decimals: u8,
    quantity_atoms: i64,
    quantity_decimals: u8,
    weight_ppm: u32,
    stale_after_ns: u64,
}
pub(super) fn parse(input: &str) -> Result<(InstrumentMetadata, [VenueConfig; 3]), String> {
    let cfg: Config = toml::from_str(input).map_err(|e| e.to_string())?;
    if !cfg.shared_clock_domain {
        return Err("multi-venue replay requires one shared local monotonic clock domain".into());
    }
    let kind = match cfg.market.kind.as_str() {
        "spot" => ContractKind::Spot,
        "linear" => ContractKind::Linear,
        _ => return Err("kind must be spot or linear; inverse contracts are unsupported".into()),
    };
    let market = MarketIdentity {
        base_asset: cfg.market.base_asset,
        quote_asset: cfg.market.quote_asset,
        settlement_asset: cfg.market.settlement_asset,
        kind,
        equivalence_group: cfg.market.equivalence_group,
    };
    let grid = InstrumentMetadata {
        instrument: InstrumentId(cfg.grid.instrument),
        market,
        price: UnitScale {
            atoms: cfg.grid.price_atoms,
            decimals: cfg.grid.price_decimals,
        },
        quantity: UnitScale {
            atoms: cfg.grid.quantity_atoms,
            decimals: cfg.grid.quantity_decimals,
        },
    };
    let configs: Vec<_> = cfg
        .venues
        .into_iter()
        .map(|v| VenueConfig {
            venue: VenueId(v.id),
            metadata: InstrumentMetadata {
                instrument: InstrumentId(v.instrument),
                market,
                price: UnitScale {
                    atoms: v.price_atoms,
                    decimals: v.price_decimals,
                },
                quantity: UnitScale {
                    atoms: v.quantity_atoms,
                    decimals: v.quantity_decimals,
                },
            },
            weight_ppm: v.weight_ppm,
            stale_after_ns: v.stale_after_ns,
        })
        .collect();
    let configs: [VenueConfig; 3] = configs
        .try_into()
        .map_err(|_| "exactly three venues are required by this CLI milestone".to_string())?;
    Engine::new(grid, configs).map_err(|e| format!("configuration: {e:?}"))?;
    Ok((grid, configs))
}
pub(super) fn metadata(c: VenueConfig) -> RecordingMetadata {
    RecordingMetadata {
        venue: c.venue,
        instrument: c.metadata.instrument,
        price_decimals: c.metadata.price.decimals,
        quantity_decimals: c.metadata.quantity.decimals,
        tick_atoms: c.metadata.price.atoms,
        quantity_atoms: c.metadata.quantity.atoms,
    }
}
fn summary(engine: &Engine, count: u64, configs: &[VenueConfig; 3]) -> Result<(), String> {
    println!(
        "events={count} state={:?} midpoint_x2={:?} spread_ticks={:?} weighted_bid_depth_microunits={} weighted_ask_depth_microunits={}",
        engine.market_state().map_err(|e| format!("{e:?}"))?,
        engine.midpoint_x2().map_err(|e| format!("{e:?}"))?,
        engine.spread().map_err(|e| format!("{e:?}"))?,
        engine.depth(Side::Buy).map_err(|e| format!("{e:?}"))?,
        engine.depth(Side::Sell).map_err(|e| format!("{e:?}"))?
    );
    for c in configs {
        println!(
            "venue={} weight_ppm={} included={} divergence={:?}",
            c.venue.0,
            c.weight_ppm,
            engine.included(c.venue).map_err(|e| format!("{e:?}"))?,
            engine.divergence(c.venue).map_err(|e| format!("{e:?}"))?
        );
    }
    Ok(())
}
fn replay_at(
    dir: &Path,
    grid: InstrumentMetadata,
    configs: [VenueConfig; 3],
) -> Result<(Engine, u64), String> {
    let files: Vec<_> = configs
        .iter()
        .map(|c| File::open(dir.join(format!("venue-{}.lre", c.venue.0))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "file count mismatch")?;
    let mut replay = MergedReplay::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = Engine::new(grid, configs).map_err(|e| format!("{e:?}"))?;
    let count = replay.run(&mut engine).map_err(|e| format!("{e:?}"))?;
    Ok((engine, count))
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let text = fs::read_to_string(dir.join("config.toml")).map_err(|e| e.to_string())?;
    let (grid, configs) = parse(&text)?;
    let (engine, count) = replay_at(dir, grid, configs)?;
    summary(&engine, count, &configs)
}
pub fn demo(config_path: &str, directory: &str) -> Result<(), String> {
    let text = fs::read_to_string(config_path).map_err(|e| e.to_string())?;
    let (grid, configs) = parse(&text)?;
    let mut direct = Engine::new(grid, configs).map_err(|e| format!("{e:?}"))?;
    let mut prices = [(PriceTicks(0), PriceTicks(0)); 3];
    for (i, c) in configs.iter().enumerate() {
        let inverse = Normalizer::new(grid, c.metadata).map_err(|e| format!("{e:?}"))?;
        prices[i] = (
            inverse
                .price(PriceTicks(9900))
                .map_err(|e| format!("synthetic bid not representable: {e:?}"))?,
            inverse
                .price(PriceTicks(10100))
                .map_err(|e| format!("synthetic ask not representable: {e:?}"))?,
        );
    }
    let dir = Path::new(directory);
    fs::create_dir(dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("config.toml"), text).map_err(|e| e.to_string())?;
    let mut writers: Vec<_> = configs
        .iter()
        .map(|&c| {
            let f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(format!("venue-{}.lre", c.venue.0)))?;
            Recorder::new(BufWriter::new(f), metadata(c))
        })
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    for sequence in 1..=1004 {
        for (i, c) in configs.iter().enumerate() {
            let ts = ((sequence - 1) * 3 + i as u64 + 1) * 1000;
            let (kind, side, price, qty) = match sequence {
                1 => (K::SnapshotStart, Side::Buy, PriceTicks(0), 0),
                2 => (K::Add, Side::Buy, prices[i].0, 100),
                3 => (K::Add, Side::Sell, prices[i].1, 100),
                4 => (K::SnapshotEnd, Side::Buy, PriceTicks(0), 0),
                _ => (
                    K::Modify,
                    Side::Buy,
                    prices[i].0,
                    100 + (sequence % 7) as i64,
                ),
            };
            let e = MarketEvent {
                venue: c.venue,
                instrument: c.metadata.instrument,
                sequence,
                exchange_sequence: sequence,
                exchange_ts: ts,
                receive_ts: Timestamp(ts),
                event_type: kind,
                side,
                price_ticks: price,
                qty_units: QtyUnits(qty),
            };
            direct
                .apply(&e)
                .map_err(|e| format!("direct input: {e:?}"))?;
            writers[i].append(&e).map_err(|e| e.to_string())?;
        }
    }
    for writer in writers {
        let writer = writer.finish().map_err(|e| e.to_string())?;
        writer.get_ref().sync_all().map_err(|e| e.to_string())?;
    }
    let (replayed, count) = replay_at(dir, grid, configs)?;
    if direct != replayed {
        return Err("direct and recorded replay states differ".into());
    }
    println!("Recorded three synthetic venue streams; direct/replay full-state equality verified.");
    summary(&replayed, count, &configs)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_validates_before_any_io() {
        let text = include_str!("../../../config/phase3.toml");
        assert!(parse(text).is_ok());
        for (from, to) in [
            ("shared_clock_domain = true", "shared_clock_domain = false"),
            ("kind = \"spot\"", "kind = \"inverse\""),
            ("weight_ppm = 1000000", "weight_ppm = 1000001"),
        ] {
            assert!(parse(&text.replace(from, to)).is_err());
        }
        assert!(parse(&format!("unknown = 1\n{text}")).is_err());
    }
}
