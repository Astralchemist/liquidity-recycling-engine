use engine::{
    EngineConfig, FillConfig, MARKET_WIDE, MULTI_VENUE, PolicyConfig, VENUE_LOCAL,
    fixtures::SyntheticEngine,
};
use inventory::{
    InventoryConfig, InventoryEvent, Ledger,
    journal::{Reader, Writer},
};
use recorder::Recorder;
use replay::merged::MergedReplay;
use serde::Deserialize;
use simulation::{Scenario, scenarios::generate};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::Path,
};
use toxicity::{Environment, EnvironmentConfig};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    environment: EnvironmentToml,
    policy: Policy,
    fills: Fills,
    inventory: super::inventory_demo::Settings,
    risk: super::inventory_demo::Limits,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentToml {
    window_ns: u64,
    shock_spread_ticks: i64,
    shock_min_top_qty: i64,
    chaotic_spread_changes: u32,
    trend_min_move_ticks: i64,
    trend_efficiency_ppm: u32,
    trend_min_aggressive_qty: i64,
    trend_aggression_ppm: u32,
    active_min_moves: u32,
    active_min_prints: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    allowed_scopes: Vec<String>,
    entry_levels: u32,
    entry_max_abs_net: i64,
    harvest_ticks: i64,
    rebalance_threshold_units: i64,
    rebalance_age_ns: u64,
    rebalance_improve_ticks: i64,
    reprice_ticks: i64,
    evidence_ttl_ns: u64,
    exit_on_environment: Vec<String>,
    mark_refresh_ns: u64,
    assess_interval_ns: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fills {
    rule: String,
    maker_rebate_atoms: String,
    maker_fee_atoms: String,
    taker_fee_atoms: String,
    emergency_slippage_ticks: i64,
}
fn scope(name: &str) -> Result<u8, String> {
    Ok(match name {
        "venue_local" => VENUE_LOCAL,
        "multi_venue" => MULTI_VENUE,
        "market_wide" => MARKET_WIDE,
        _ => return Err(format!("unknown scope {name}")),
    })
}
fn environment(name: &str) -> Result<Environment, String> {
    Ok(match name {
        "dead" => Environment::Dead,
        "balanced_active" => Environment::BalancedActive,
        "trending" => Environment::Trending,
        "liquidity_shock" => Environment::LiquidityShock,
        "chaotic" => Environment::Chaotic,
        _ => return Err(format!("unknown environment {name}")),
    })
}
pub(super) fn parse(text: &str) -> Result<(InventoryConfig<3>, EngineConfig), String> {
    let c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    if c.fills.rule != "strict_trade_through" {
        return Err("fills.rule must be strict_trade_through (the only implemented rule)".into());
    }
    let e = c.environment;
    let p = c.policy;
    let mut allowed_scopes = 0;
    for name in &p.allowed_scopes {
        allowed_scopes |= scope(name)?;
    }
    let mut exit_on_environment = 0;
    for name in &p.exit_on_environment {
        exit_on_environment |= environment(name)?.bit();
    }
    let engine = EngineConfig {
        environment: EnvironmentConfig {
            window_ns: e.window_ns,
            shock_spread_ticks: e.shock_spread_ticks,
            shock_min_top_qty: i128::from(e.shock_min_top_qty),
            chaotic_spread_changes: e.chaotic_spread_changes,
            trend_min_move_ticks: e.trend_min_move_ticks,
            trend_efficiency_ppm: e.trend_efficiency_ppm,
            trend_min_aggressive_qty: i128::from(e.trend_min_aggressive_qty),
            trend_aggression_ppm: e.trend_aggression_ppm,
            active_min_moves: e.active_min_moves,
            active_min_prints: e.active_min_prints,
        },
        policy: PolicyConfig {
            allowed_scopes,
            entry_levels: p.entry_levels,
            entry_max_abs_net: p.entry_max_abs_net,
            harvest_ticks: p.harvest_ticks,
            rebalance_threshold_units: p.rebalance_threshold_units,
            rebalance_age_ns: p.rebalance_age_ns,
            rebalance_improve_ticks: p.rebalance_improve_ticks,
            reprice_ticks: p.reprice_ticks,
            evidence_ttl_ns: p.evidence_ttl_ns,
            exit_on_environment,
            mark_refresh_ns: p.mark_refresh_ns,
            assess_interval_ns: p.assess_interval_ns,
        },
        fills: FillConfig {
            maker_rebate: super::inventory_demo::money(&c.fills.maker_rebate_atoms)?,
            maker_fee: super::inventory_demo::money(&c.fills.maker_fee_atoms)?,
            taker_fee: super::inventory_demo::money(&c.fills.taker_fee_atoms)?,
            emergency_slippage_ticks: c.fills.emergency_slippage_ticks,
        },
    };
    let inventory = super::inventory_demo::inventory_config(c.inventory, c.risk)?;
    Ok((inventory, engine))
}
struct Texts {
    market: String,
    structures: String,
    flow: String,
    engine: String,
}
fn build(t: &Texts) -> Result<Box<SyntheticEngine>, String> {
    let (grid, venues) = super::multi::parse(&t.market)?;
    let (liquidity, voids) = super::structures::parse(&t.structures)?;
    let (flow, _) = super::flow::parse(&t.flow)?;
    let (inventory, engine) = parse(&t.engine)?;
    Ok(Box::new(
        SyntheticEngine::new(grid, venues, liquidity, voids, flow, inventory, engine)
            .map_err(|e| format!("engine configuration: {e:?}"))?,
    ))
}
fn read(market: &str, structures: &str, flow: &str, engine: &str) -> Result<Texts, String> {
    let read = |p: &str| fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    Ok(Texts {
        market: read(market)?,
        structures: read(structures)?,
        flow: read(flow)?,
        engine: read(engine)?,
    })
}
fn scenario(name: &str) -> Result<Scenario, String> {
    Scenario::ALL
        .into_iter()
        .find(|s| {
            s.letter()
                .eq_ignore_ascii_case(&name.chars().next().unwrap_or(' '))
                && name.len() == 1
        })
        .ok_or_else(|| "scenario must be one of a, b, c, d, e, f".into())
}
fn run(t: &Texts, s: Scenario) -> Result<(Box<SyntheticEngine>, Vec<InventoryEvent>), String> {
    let mut engine = build(t)?;
    let mut journal = Vec::new();
    for e in generate(s) {
        engine
            .apply(&e, &mut |c| journal.push(*c))
            .map_err(|e| format!("engine: {e:?}"))?;
    }
    Ok((engine, journal))
}
fn yield_text(e: &SyntheticEngine) -> String {
    e.ledger()
        .episodes()
        .iter()
        .flatten()
        .chain(e.ledger().active_episode().iter())
        .map(|ep| match ep.recovery_yield_ppm() {
            Ok(Some(y)) => y.to_string(),
            _ => "-".into(),
        })
        .collect::<Vec<_>>()
        .join("/")
}
/// Engine summary for any capacity set with 3 venues, 32 inventory slots and 8 episodes.
pub(super) fn summary<
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
    const W: usize,
>(
    e: &engine::Engine<3, N, G, P, B, Z, W, 32, 8>,
    label: &str,
) -> Result<(), String> {
    let m = e.metrics();
    let ms = e.environment().time_in_state().map(|t| t / 1_000_000);
    println!(
        "scenario={label} market_events={} environment_ms dead={} balanced_active={} trending={} liquidity_shock={} chaotic={} transitions={}",
        m.market_events,
        ms[0],
        ms[1],
        ms[2],
        ms[3],
        ms[4],
        e.environment().transitions()
    );
    println!(
        "void_metrics={:?}",
        e.research().research().voids().metrics()
    );
    for z in e.research().research().voids().zones().iter().flatten() {
        let traded = e
            .ledger()
            .episodes()
            .iter()
            .flatten()
            .chain(e.ledger().active_episode().iter())
            .any(|ep| ep.void_id == z.id);
        println!(
            "zone id={} region=[{},{}] scope={:?} mask={} state={:?} revisits={} max_penetration_ppm={} traded={traded}",
            z.id,
            z.region.lower.0,
            z.region.upper.0,
            z.scope,
            z.venue_mask,
            z.state,
            z.revisit_count,
            z.max_penetration_ppm
        );
    }
    println!(
        "policy entries_placed={} entries_cancelled={} reprices={} harvests={} rebalances={} maker_fills={} taker_fills={} disallowed_scope_steps={} unqualified_revisit_steps={} kill_steps={} denials={:?}",
        m.entries_placed,
        m.entries_cancelled,
        m.reprices,
        m.harvests_placed,
        m.rebalances_placed,
        m.maker_fills,
        m.taker_fills,
        m.disallowed_scope_steps,
        m.unqualified_revisit_steps,
        m.kill_steps,
        m.denials
    );
    println!(
        "cross_venue max_abs_divergence_x2={} divergent_events={} research_fault={:?} environment_fault={:?}",
        m.max_abs_divergence_x2,
        m.divergent_events,
        e.research_fault(),
        e.environment_fault()
    );
    if e.research_fault().is_none() {
        println!(
            "toxicity_components={:?}",
            e.toxicity().map_err(|e| format!("{e:?}"))?
        );
    }
    super::inventory_demo::summary(e.ledger())
}
fn replay_at(dir: &Path, t: &Texts) -> Result<(Box<SyntheticEngine>, Vec<InventoryEvent>), String> {
    let (_, venues) = super::multi::parse(&t.market)?;
    let files: Vec<_> = venues
        .iter()
        .map(|c| File::open(dir.join(format!("venue-{}.lre", c.venue.0))).map(BufReader::new))
        .collect::<std::io::Result<_>>()
        .map_err(|e| e.to_string())?;
    let files: [BufReader<File>; 3] = files.try_into().map_err(|_| "expected three files")?;
    let mut merged = MergedReplay::new(files, true).map_err(|e| format!("{e:?}"))?;
    let mut engine = build(t)?;
    merged
        .validate(engine.research().research().market())
        .map_err(|e| format!("{e:?}"))?;
    let mut journal = Vec::new();
    while let Some(e) = merged.next_event().map_err(|e| format!("{e:?}"))? {
        engine
            .apply(&e, &mut |c| journal.push(*c))
            .map_err(|e| format!("engine: {e:?}"))?;
    }
    // The journal file alone must rebuild the identical ledger.
    let file = File::open(dir.join("inventory.lri")).map_err(|e| e.to_string())?;
    let mut reader = Reader::new(
        BufReader::new(file),
        recorder::checksum(t.engine.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    let mut ledger =
        Ledger::<3, 32, 8>::new(engine.ledger().config()).map_err(|e| format!("{e:?}"))?;
    let mut recorded = Vec::new();
    while let Some(c) = reader.next_event().map_err(|e| e.to_string())? {
        ledger.apply(c).map_err(|e| format!("{e:?}"))?;
        recorded.push(c);
    }
    if recorded != journal || ledger != *engine.ledger() {
        return Err("inventory journal differs from the replayed engine".into());
    }
    Ok((engine, journal))
}
pub fn demo(
    market: &str,
    structures: &str,
    flow: &str,
    engine: &str,
    directory: &str,
    name: &str,
) -> Result<(), String> {
    let s = scenario(name)?;
    let t = read(market, structures, flow, engine)?;
    let (direct, journal) = run(&t, s)?;
    let (_, venues) = super::multi::parse(&t.market)?;
    let dir = Path::new(directory);
    fs::create_dir(dir).map_err(|e| e.to_string())?;
    for (file, text) in [
        ("market.toml", &t.market),
        ("structures.toml", &t.structures),
        ("flow.toml", &t.flow),
        ("engine.toml", &t.engine),
        ("scenario.txt", &s.letter().to_string()),
    ] {
        fs::write(dir.join(file), text).map_err(|e| e.to_string())?;
    }
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
    for e in generate(s) {
        let v = venues
            .iter()
            .position(|c| c.venue == e.venue)
            .ok_or("fixture venue")?;
        writers[v].append(&e).map_err(|e| e.to_string())?;
    }
    for w in writers {
        w.finish()
            .map_err(|e| e.to_string())?
            .get_ref()
            .sync_all()
            .map_err(|e| e.to_string())?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("inventory.lri"))
        .map_err(|e| e.to_string())?;
    let mut writer = Writer::new(
        BufWriter::new(file),
        recorder::checksum(t.engine.as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    for c in &journal {
        writer.append(*c).map_err(|e| e.to_string())?;
    }
    writer
        .finish()
        .map_err(|e| e.to_string())?
        .get_ref()
        .sync_all()
        .map_err(|e| e.to_string())?;
    let (replayed, replayed_journal) = replay_at(dir, &t)?;
    if *replayed != *direct || replayed_journal != journal {
        return Err("direct and replayed engine states differ".into());
    }
    println!(
        "Direct and recorded-market replay match: complete engine state and {} inventory commands. Rule-based simulated fills (strict trade-through); not a queue model.",
        journal.len()
    );
    summary(&replayed, &s.letter().to_string())
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let read = |f: &str| fs::read_to_string(dir.join(f)).map_err(|e| format!("{f}: {e}"));
    let t = Texts {
        market: read("market.toml")?,
        structures: read("structures.toml")?,
        flow: read("flow.toml")?,
        engine: read("engine.toml")?,
    };
    let label = read("scenario.txt")?;
    let (engine, _) = replay_at(dir, &t)?;
    summary(&engine, label.trim())
}
pub fn suite(market: &str, structures: &str, flow: &str, engine: &str) -> Result<(), String> {
    let t = read(market, structures, flow, engine)?;
    println!(
        "scenario | events | halt | flat | cycles | maker fills | taker fills | peak liability | net pnl atoms | recovery yield ppm | zone scope | traded"
    );
    for s in Scenario::ALL {
        let (e, _) = run(&t, s)?;
        let l = e.ledger();
        let x = l.exposure();
        let peak = l
            .episodes()
            .iter()
            .flatten()
            .chain(l.active_episode().iter())
            .map(|ep| ep.peak_liability.0)
            .max()
            .unwrap_or(0);
        let z = e
            .research()
            .research()
            .voids()
            .zones()
            .iter()
            .flatten()
            .next()
            .copied();
        println!(
            "{} {:?} | {} | {:?} | {} | {} | {} | {} | {} | {} | {} | {:?} | {}",
            s.letter(),
            s,
            e.metrics().market_events,
            l.halt_reason(),
            x.gross() == 0 && x.open_orders == 0,
            l.counts().completed_cycles,
            e.metrics().maker_fills,
            e.metrics().taker_fills,
            peak,
            l.equity().map_err(|e| format!("{e:?}"))?.0,
            yield_text(&e),
            z.map(|z| z.scope),
            e.metrics().entries_placed > 0
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use engine::fixtures;
    #[test]
    fn toml_matches_engine_fixtures_and_is_strict() {
        let text = include_str!("../../../config/phase7-engine.toml");
        assert_eq!(parse(text), Ok((fixtures::inventory(), fixtures::engine())));
        let market = include_str!("../../../config/phase4-market.toml");
        assert_eq!(super::super::multi::parse(market), Ok(fixtures::market()));
        let structures = include_str!("../../../config/phase7-structures.toml");
        assert_eq!(
            super::super::structures::parse(structures),
            Ok(fixtures::structures())
        );
        let flow = include_str!("../../../config/phase7-flow.toml");
        assert_eq!(
            super::super::flow::parse(flow).map(|f| f.0),
            Ok(fixtures::flow())
        );
        assert!(parse(&text.replace("strict_trade_through", "touch")).is_err());
        assert!(parse(&text.replace("\"chaotic\"]", "\"stormy\"]")).is_err());
        assert!(parse(&text.replace("\"market_wide\"]", "\"global\"]")).is_err());
        assert!(parse(&format!("extra = true\n{text}")).is_err());
        assert_eq!(scenario("e"), Ok(Scenario::ToxicRecovery));
        assert!(scenario("g").is_err() && scenario("ab").is_err());
    }
}
