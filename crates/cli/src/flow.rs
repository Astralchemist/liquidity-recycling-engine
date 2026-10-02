use common::{Timestamp, VenueId};
use consolidator::{VenueConfig, normalization::InstrumentMetadata};
use liquidity::LiquidityConfig;
use orderflow::{research::FlowResearchEngine, *};
use replay::merged::MergedReplay;
use serde::Deserialize;
use simulation::structures::{StructureScenario, generate};
use std::{
    fs::{self, File},
    io::BufReader,
    path::Path,
};
use voids::VoidConfig;
type Engine = FlowResearchEngine<3, 128, 384, 64, 6, 16, 128>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    orderflow: Settings,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    event_window: usize,
    time_window_ns: u64,
    qi_levels: usize,
    removal_attribution: String,
    probability_horizon_ns: u64,
}
pub(super) fn parse(text: &str) -> Result<(FlowConfig, u64), String> {
    let settings: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let s = settings.orderflow;
    let removal_attribution = match s.removal_attribution.as_str() {
        "unknown" => RemovalAttribution::Unknown,
        "assume_cancellation" => RemovalAttribution::AssumeCancellation,
        _ => return Err("removal_attribution must be unknown or assume_cancellation".into()),
    };
    let config = FlowConfig {
        event_window: s.event_window,
        time_window_ns: s.time_window_ns,
        qi_levels: s.qi_levels,
        removal_attribution,
    };
    FlowEngine::<128, 6>::validate(config).map_err(|e| format!("{e:?}"))?;
    if s.probability_horizon_ns == 0 {
        return Err("probability_horizon_ns must be positive".into());
    }
    Ok((config, s.probability_horizon_ns))
}
fn summary(
    e: &Engine,
    count: u64,
    now: Timestamp,
    horizon: u64,
    venues: [VenueId; 3],
) -> Result<(), String> {
    println!(
        "flow_events={count} weighted_time_ofi_and_mask={:?}",
        e.weighted_time_ofi().map_err(|e| format!("{e:?}"))?
    );
    for venue in venues {
        let Some(s) = e.snapshot(venue).map_err(|e| format!("{e:?}"))? else {
            println!("venue={} flow=unavailable", venue.0);
            continue;
        };
        println!(
            "venue={} epoch={} event_count={} time_count={} exposure_ns={} event_ofi={} time_ofi={} time_depth_delta={} qi_best_ppm={:?} qi_top_n_ppm={:?}",
            venue.0,
            e.epoch(venue).map_err(|e| format!("{e:?}"))?,
            s.event_count,
            s.time_count,
            s.exposure_ns,
            s.event_totals.get(Metric::Ofi),
            s.time_totals.get(Metric::Ofi),
            s.time_totals.get(Metric::DepthDelta),
            e.queue_imbalance(venue, true)
                .map_err(|e| format!("{e:?}"))?,
            e.queue_imbalance(venue, false)
                .map_err(|e| format!("{e:?}"))?
        );
        for metric in [
            Metric::AddedQty,
            Metric::RemovedQty,
            Metric::BuyQty,
            Metric::SellQty,
            Metric::AddEvents,
            Metric::RemovalEvents,
            Metric::BuyEvents,
            Metric::SellEvents,
        ] {
            println!(
                "  {metric:?}: quantity_or_count={} rate_micro_per_second={:?}",
                s.time_totals.get(metric),
                s.time_rate(metric).map_err(|e| format!("{e:?}"))?
            );
        }
        let rate = s
            .time_rate(Metric::BuyEvents)
            .map_err(|e| format!("{e:?}"))?;
        let probability = rate
            .map(|r| u64::try_from(r).map(|r| poisson_event_probability_ppm(r, horizon)))
            .transpose()
            .map_err(|e| e.to_string())?;
        println!(
            "  cancellation_rate_micro_per_second={:?} buy_event_probability_ppm={probability:?} horizon_ns={horizon}",
            s.cancellation_rate().map_err(|e| format!("{e:?}"))?
        );
        for (i, b) in s.time_buckets.iter().enumerate() {
            println!(
                "  bucket={i} add_rate={:?} removal_rate={:?} execution_qty_rate={:?}",
                b.rate(Metric::AddEvents, s.exposure_ns)
                    .map_err(|e| format!("{e:?}"))?,
                b.rate(Metric::RemovalEvents, s.exposure_ns)
                    .map_err(|e| format!("{e:?}"))?,
                match (
                    b.rate(Metric::BuyQty, s.exposure_ns)
                        .map_err(|e| format!("{e:?}"))?,
                    b.rate(Metric::SellQty, s.exposure_ns)
                        .map_err(|e| format!("{e:?}"))?
                ) {
                    (Some(b), Some(a)) => Some(b + a),
                    _ => None,
                }
            );
        }
    }
    println!(
        "bucket_qi_ppm={:?}",
        e.bucket_queue_imbalance(now)
            .map_err(|e| format!("{e:?}"))?
    );
    Ok(())
}
fn read_replay(
    dir: &Path,
    grid: InstrumentMetadata,
    venues: [VenueConfig; 3],
    l: LiquidityConfig<6>,
    v: VoidConfig,
    f: FlowConfig,
) -> Result<(Engine, u64, Timestamp), String> {
    let files: Vec<_> = venues
        .iter()
        .map(|c| File::open(dir.join(format!("venue-{}.lre", c.venue.0))).map(BufReader::new))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected 3 files")?;
    let mut replay = MergedReplay::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut e = Engine::new(grid, venues, l, v, f).map_err(|e| format!("{e:?}"))?;
    replay
        .validate(e.research().market())
        .map_err(|e| format!("{e:?}"))?;
    let (mut count, mut now) = (0, Timestamp(0));
    while let Some(event) = replay.next_event().map_err(|e| format!("{e:?}"))? {
        e.apply(&event).map_err(|e| format!("{e:?}"))?;
        count += 1;
        now = event.receive_ts;
    }
    for c in venues {
        if matches!(
            e.research()
                .market()
                .venue_book(c.venue)
                .map_err(|e| format!("{e:?}"))?
                .state(),
            book::BookState::AwaitingSnapshot | book::BookState::BuildingSnapshot
        ) {
            return Err("incomplete snapshot at end of recording".into());
        }
    }
    Ok((e, count, now))
}
pub fn demo(market: &str, structures: &str, flow: &str, directory: &str) -> Result<(), String> {
    let (grid, venues) =
        super::multi::parse(&fs::read_to_string(market).map_err(|e| e.to_string())?)?;
    let (l, v) =
        super::structures::parse(&fs::read_to_string(structures).map_err(|e| e.to_string())?)?;
    let text = fs::read_to_string(flow).map_err(|e| e.to_string())?;
    let (f, horizon) = parse(&text)?;
    let mut direct = Engine::new(grid, venues, l, v, f).map_err(|e| format!("{e:?}"))?;
    super::structures::demo(market, structures, directory, "revisit")?;
    fs::write(Path::new(directory).join("flow.toml"), text).map_err(|e| e.to_string())?;
    generate(
        venues.map(|c| c.venue),
        venues.map(|c| c.metadata.instrument),
        StructureScenario::Revisit,
        |event| direct.apply(&event),
    )
    .map_err(|e| format!("{e:?}"))?;
    let (recorded, count, now) = read_replay(Path::new(directory), grid, venues, l, v, f)?;
    if direct != recorded {
        return Err("direct and replay flow state differ".into());
    }
    println!("Direct and recorded/replayed complete flow + research state match.");
    summary(&recorded, count, now, horizon, venues.map(|c| c.venue))
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let (grid, venues) = super::multi::parse(
        &fs::read_to_string(dir.join("config.toml")).map_err(|e| e.to_string())?,
    )?;
    let (l, v) = super::structures::parse(
        &fs::read_to_string(dir.join("structures.toml")).map_err(|e| e.to_string())?,
    )?;
    let (f, horizon) =
        parse(&fs::read_to_string(dir.join("flow.toml")).map_err(|e| e.to_string())?)?;
    let (e, count, now) = read_replay(dir, grid, venues, l, v, f)?;
    summary(&e, count, now, horizon, venues.map(|c| c.venue))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_flow_configuration() {
        let text = include_str!("../../../config/phase5-flow.toml");
        assert!(parse(text).is_ok());
        assert!(parse(&text.replace("unknown", "guessed")).is_err());
        assert!(parse(&text.replace("event_window = 16", "event_window = 129")).is_err());
        assert!(parse(&format!("surprise = 1\n{text}")).is_err());
    }
}
