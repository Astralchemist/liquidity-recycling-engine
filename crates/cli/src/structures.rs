use common::Timestamp;
use consolidator::{VenueConfig, normalization::InstrumentMetadata};
use fixed_point::PriceTicks;
use liquidity::{LiquidityConfig, PriceReference, research::ResearchEngine};
use recorder::Recorder;
use replay::merged::MergedReplay;
use serde::Deserialize;
use simulation::structures::{StructureScenario, generate};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::Path,
};
use voids::VoidConfig;
type Engine = ResearchEngine<3, 128, 384, 64, 6, 16>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    liquidity: Liquidity,
    void: Void,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Liquidity {
    lower_price: i64,
    cell_width: usize,
    bucket_edges_ppm: [u32; 6],
    sample_interval_ns: u64,
    warmup_samples: u32,
    baseline_alpha_ppm: u32,
    minimum_baseline_units: i64,
    minimum_covered_venues: u32,
    assume_contiguous_l2_coverage: bool,
    pool_depth_ratio_ppm: u32,
    pool_persistence_ns: u64,
    pool_minimum_score_ppm: u32,
    pool_weights: [u32; 3],
    price_reference: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Void {
    minimum_width_ticks: i64,
    minimum_persistence_ns: u64,
    maximum_age_ns: u64,
    low_depth_ppm: u32,
    refill_depth_ppm: u32,
    minimum_score_ppm: u32,
}
pub(super) fn parse(input: &str) -> Result<(LiquidityConfig<6>, VoidConfig), String> {
    let cfg: Config = toml::from_str(input).map_err(|e| e.to_string())?;
    let l = cfg.liquidity;
    let v = cfg.void;
    let price_reference = match l.price_reference.as_str() {
        "midpoint" => PriceReference::Midpoint,
        "last_trade" => PriceReference::LastTrade,
        _ => return Err("price_reference must be midpoint or last_trade".into()),
    };
    Ok((
        LiquidityConfig {
            lower_price: PriceTicks(l.lower_price),
            cell_width: l.cell_width,
            bucket_edges_ppm: l.bucket_edges_ppm,
            sample_interval_ns: l.sample_interval_ns,
            warmup_samples: l.warmup_samples,
            baseline_alpha_ppm: l.baseline_alpha_ppm,
            minimum_baseline_units: l.minimum_baseline_units,
            minimum_covered_venues: l.minimum_covered_venues,
            assume_contiguous_l2_coverage: l.assume_contiguous_l2_coverage,
            pool_depth_ratio_ppm: l.pool_depth_ratio_ppm,
            pool_persistence_ns: l.pool_persistence_ns,
            pool_minimum_score_ppm: l.pool_minimum_score_ppm,
            pool_weights: l.pool_weights,
            price_reference,
        },
        VoidConfig {
            minimum_width_ticks: v.minimum_width_ticks,
            minimum_persistence_ns: v.minimum_persistence_ns,
            maximum_age_ns: v.maximum_age_ns,
            low_depth_ppm: v.low_depth_ppm,
            refill_depth_ppm: v.refill_depth_ppm,
            minimum_score_ppm: v.minimum_score_ppm,
        },
    ))
}
fn summary(engine: &Engine, count: u64, now: Timestamp) -> Result<(), String> {
    let m = engine.voids().metrics();
    println!(
        "events={count} registered_voids={} revisited_zones={} revisit_events={} refilled_before_revisit={} pool_activations={}",
        m.registered,
        m.revisited_zones,
        m.revisits,
        m.refilled_before_revisit,
        engine.liquidity().metrics().pool_activations
    );
    println!("void_metrics={m:?}");
    println!("liquidity_metrics={:?}", engine.liquidity().metrics());
    for zone in engine.voids().zones().iter().flatten() {
        println!(
            "zone id={} region=[{},{}] scope={:?} mask={} state={:?} revisits={} maximum_penetration_ppm={} first_revisit_at={:?}",
            zone.id,
            zone.region.lower.0,
            zone.region.upper.0,
            zone.scope,
            zone.venue_mask,
            zone.state,
            zone.revisit_count,
            zone.max_penetration_ppm,
            zone.first_revisit_at
        );
    }
    if let Some(mid) = engine
        .market()
        .midpoint_x2()
        .map_err(|e| format!("{e:?}"))?
    {
        for (i, b) in engine
            .liquidity()
            .buckets(mid, now)
            .map_err(|e| format!("{e:?}"))?
            .iter()
            .enumerate()
        {
            println!("bucket={i} {b:?}");
        }
    }
    Ok(())
}
fn replay_at(
    dir: &Path,
    grid: InstrumentMetadata,
    venues: [VenueConfig; 3],
    l: LiquidityConfig<6>,
    v: VoidConfig,
) -> Result<(Engine, u64, Timestamp), String> {
    let files: Vec<_> = venues
        .iter()
        .map(|c| File::open(dir.join(format!("venue-{}.lre", c.venue.0))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut replay = MergedReplay::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = Engine::new(grid, venues, l, v).map_err(|e| format!("{e:?}"))?;
    replay
        .validate(engine.market())
        .map_err(|e| format!("{e:?}"))?;
    let (mut count, mut now) = (0, Timestamp(0));
    while let Some(e) = replay.next_event().map_err(|e| format!("{e:?}"))? {
        engine.apply(&e).map_err(|e| format!("{e:?}"))?;
        count += 1;
        now = e.receive_ts;
    }
    for c in venues {
        if matches!(
            engine
                .market()
                .venue_book(c.venue)
                .map_err(|e| format!("{e:?}"))?
                .state(),
            book::BookState::AwaitingSnapshot | book::BookState::BuildingSnapshot
        ) {
            return Err("incomplete snapshot at end of recording".into());
        }
    }
    Ok((engine, count, now))
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let text = fs::read_to_string(dir.join("config.toml")).map_err(|e| e.to_string())?;
    let (grid, venues) = super::multi::parse(&text)?;
    let text = fs::read_to_string(dir.join("structures.toml")).map_err(|e| e.to_string())?;
    let (l, v) = parse(&text)?;
    let (engine, count, now) = replay_at(dir, grid, venues, l, v)?;
    summary(&engine, count, now)
}
pub fn demo(
    market_config: &str,
    structure_config: &str,
    directory: &str,
    scenario: &str,
) -> Result<(), String> {
    let scenario = match scenario {
        "revisit" => StructureScenario::Revisit,
        "local" => StructureScenario::LocalVoid,
        "refill" => StructureScenario::RefillBeforeRevisit,
        "continuation" => StructureScenario::Continuation,
        _ => return Err("scenario must be revisit, local, refill, or continuation".into()),
    };
    let market_text = fs::read_to_string(market_config).map_err(|e| e.to_string())?;
    let (grid, venues) = super::multi::parse(&market_text)?;
    let structures_text = fs::read_to_string(structure_config).map_err(|e| e.to_string())?;
    let (l, v) = parse(&structures_text)?;
    let mut direct = Engine::new(grid, venues, l, v).map_err(|e| format!("{e:?}"))?;
    if venues
        .iter()
        .any(|c| c.metadata.price != grid.price || c.metadata.quantity != grid.quantity)
    {
        return Err("this synthetic fixture requires common native tick/lot scales; replay supports differing scales".into());
    }
    let dir = Path::new(directory);
    fs::create_dir(dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("config.toml"), market_text).map_err(|e| e.to_string())?;
    fs::write(dir.join("structures.toml"), structures_text).map_err(|e| e.to_string())?;
    let mut writers: Vec<_> = venues
        .iter()
        .map(|&c| {
            let f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(format!("venue-{}.lre", c.venue.0)))?;
            Recorder::new(BufWriter::new(f), super::multi::metadata(c))
        })
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    generate(
        venues.map(|c| c.venue),
        venues.map(|c| c.metadata.instrument),
        scenario,
        |event| {
            direct.apply(&event).map_err(|e| format!("{e:?}"))?;
            let i = venues
                .iter()
                .position(|c| c.venue == event.venue)
                .expect("fixture venue");
            writers[i].append(&event).map_err(|e| e.to_string())
        },
    )?;
    for writer in writers {
        let writer = writer.finish().map_err(|e| e.to_string())?;
        writer.get_ref().sync_all().map_err(|e| e.to_string())?;
    }
    let (replayed, count, now) = replay_at(dir, grid, venues, l, v)?;
    if direct != replayed {
        return Err("direct and replay research state differ".into());
    }
    println!("Direct and recorded/replayed complete research state match.");
    summary(&replayed, count, now)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_is_strict() {
        let text = include_str!("../../../config/phase4-structures.toml");
        assert!(parse(text).is_ok());
        assert!(parse(&text.replace("last_trade", "unknown")).is_err());
        assert!(parse(&format!("extra = true\n{text}")).is_err());
    }
}
