use common::VenueId;
use fixed_point::{InventoryUnits, Money, QtyUnits, Quantum};
use inventory::{
    InventoryConfig, InventoryEvent, InventoryEventKind, Ledger,
    fixtures::{Scenario, generate},
    journal::{Reader, Writer},
};
use risk::InventoryLimits;
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::Path,
};
type Engine = Ledger<3, 32, 8>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    inventory: Settings,
    risk: Limits,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    venues: [u16; 3],
    parent_quantity: i64,
    quantum_numerator: u32,
    quantum_denominator: u32,
    target_units: i64,
    target_tolerance: i64,
    completion_max_gross: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_net_units: i64,
    max_gross_units: i64,
    max_venue_units: i64,
    max_target_deviation: i64,
    max_open_orders: usize,
    max_liability_atoms: String,
    max_drawdown_atoms: String,
    max_position_age_ns: u64,
    max_episode_ns: u64,
    max_adverse_episodes: u32,
    mark_stale_ns: u64,
}
fn money(text: &str) -> Result<Money, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("money limits require positive integer atom strings".into());
    }
    text.parse::<i128>().map(Money).map_err(|e| e.to_string())
}
fn parse(text: &str) -> Result<InventoryConfig<3>, String> {
    let c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let (s, r) = (c.inventory, c.risk);
    let cfg = InventoryConfig {
        venues: s.venues.map(VenueId),
        quantum: Quantum::new(
            QtyUnits(s.parent_quantity),
            s.quantum_numerator,
            s.quantum_denominator,
        )
        .map_err(|e| format!("{e:?}"))?,
        target: InventoryUnits(s.target_units),
        target_tolerance: s.target_tolerance,
        completion_max_gross: s.completion_max_gross,
        limits: InventoryLimits {
            max_net_units: r.max_net_units,
            max_gross_units: r.max_gross_units,
            max_venue_units: r.max_venue_units,
            max_target_deviation: r.max_target_deviation,
            max_open_orders: r.max_open_orders,
            max_liability: money(&r.max_liability_atoms)?,
            max_drawdown: money(&r.max_drawdown_atoms)?,
            max_position_age_ns: r.max_position_age_ns,
            max_episode_ns: r.max_episode_ns,
            max_adverse_episodes: r.max_adverse_episodes,
            mark_stale_ns: r.mark_stale_ns,
        },
    };
    Engine::new(cfg).map_err(|e| format!("{e:?}"))?;
    Ok(cfg)
}
fn ratio(n: u64, d: u64) -> Option<u128> {
    if d == 0 {
        None
    } else {
        Some(u128::from(n) * 1_000_000 / u128::from(d))
    }
}
fn label(kind: InventoryEventKind) -> &'static str {
    match kind {
        InventoryEventKind::Mark { .. } => "mark",
        InventoryEventKind::ReserveOpen { .. } => "reserve_open",
        InventoryEventKind::FillOpen { .. } => "fill_open",
        InventoryEventKind::CancelOpen { .. } => "cancel_open",
        InventoryEventKind::ReserveClose { .. } => "reserve_close",
        InventoryEventKind::FillClose { .. } => "fill_close",
        InventoryEventKind::CancelClose { .. } => "cancel_close",
        InventoryEventKind::Funding { .. } => "funding",
        InventoryEventKind::Halt { .. } => "halt",
        InventoryEventKind::Tick => "tick",
    }
}
/// Recovery_t = H_t - D_total after every accepted command.
fn trace(event: InventoryEvent, e: &Engine) -> Result<(), String> {
    let x = e.exposure();
    println!(
        "seq={} t_ns={} command={} net={} gross={} open_orders={} liability_atoms={} harvest_atoms={} recovery_atoms={} equity_atoms={} halt={:?}",
        event.sequence,
        event.timestamp.0,
        label(event.kind),
        x.net(),
        x.gross(),
        x.open_orders,
        e.liability().0,
        e.accounts().harvest().map_err(|e| format!("{e:?}"))?.0,
        e.recovery().map_err(|e| format!("{e:?}"))?.0,
        e.equity().map_err(|e| format!("{e:?}"))?.0,
        e.halt_reason()
    );
    Ok(())
}
fn summary(e: &Engine) -> Result<(), String> {
    let counts = e.counts();
    let (completed, profitable, pnl) = e.episode_totals();
    println!(
        "commands={} quantum_qty={} exposure={:?} halt={:?} safety_work_remaining={}",
        e.last_sequence(),
        e.config().quantum.unit_quantity().0,
        e.exposure(),
        e.halt_reason(),
        e.safety_work_remaining()
    );
    println!(
        "accounts={:?} cash_atoms={} unrealized_atoms={} liability_atoms={} harvest_atoms={} recovery_atoms={} equity_atoms={}",
        e.accounts(),
        e.cash().0,
        e.unrealized().0,
        e.liability().0,
        e.accounts().harvest().map_err(|e| format!("{e:?}"))?.0,
        e.recovery().map_err(|e| format!("{e:?}"))?.0,
        e.equity().map_err(|e| format!("{e:?}"))?.0
    );
    println!(
        "completed_episodes={completed} profitable_episode_ppm={:?} completed_episode_pnl_atoms={} unassigned_pnl_atoms={} history_evictions={:?}",
        ratio(profitable, completed),
        pnl.0,
        e.unassigned_pnl().map_err(|e| format!("{e:?}"))?.0,
        e.history_evictions()
    );
    println!(
        "counts={counts:?} maker_fill_ppm={:?} maker_to_maker_ppm={:?} taker_escape_per_cycle_ppm={:?}",
        ratio(counts.maker_fills, counts.fills),
        ratio(counts.maker_to_maker, counts.completed_cycles),
        ratio(counts.taker_escapes, counts.completed_cycles)
    );
    for episode in e
        .episodes()
        .iter()
        .flatten()
        .copied()
        .chain(e.active_episode())
    {
        let (a, c) = (episode.accounts, episode.counts);
        println!(
            "episode={} void={} start_ns={} end_ns={:?} duration_ns={} forced={:?} peak_long={} peak_short={} peak_gross={} peak_net={} peak_target_deviation={} peak_liability_atoms={} realized_atoms={} rebate_atoms={} fee_atoms={} slippage_atoms={} funding_atoms={} residual_mtm_atoms={} net_pnl_atoms={} inventory_recovery_yield_ppm={:?} first_recovery_ns={:?} orders={} fills={} maker_fill_ppm={:?} maker_to_maker_ppm={:?} taker_escapes={}",
            episode.id,
            episode.void_id,
            episode.started_at.0,
            episode.ended_at.map(|t| t.0),
            episode.ended_at.unwrap_or(e.now()).0 - episode.started_at.0,
            episode.forced,
            episode.peak_long,
            episode.peak_short,
            episode.peak_gross,
            episode.peak_net,
            episode.peak_net_deviation,
            episode.peak_liability.0,
            a.realized.0,
            a.rebates.0,
            a.fees.0,
            a.slippage.0,
            a.funding.0,
            episode.residual_mtm.0,
            episode.net_pnl.0,
            episode.recovery_yield_ppm().map_err(|e| format!("{e:?}"))?,
            episode
                .first_recovery_at
                .map(|t| t.0 - episode.started_at.0),
            c.orders,
            c.fills,
            ratio(c.maker_fills, c.fills),
            ratio(c.maker_to_maker, c.completed_cycles),
            c.taker_escapes
        );
    }
    Ok(())
}
fn replay_at(dir: &Path, text: &str, cfg: InventoryConfig<3>) -> Result<Engine, String> {
    let file = File::open(dir.join("inventory.lri")).map_err(|e| e.to_string())?;
    let mut reader = Reader::new(BufReader::new(file), recorder::checksum(text.as_bytes()))
        .map_err(|e| e.to_string())?;
    let mut e = Engine::new(cfg).map_err(|e| format!("{e:?}"))?;
    while let Some(event) = reader.next_event().map_err(|e| e.to_string())? {
        e.apply(event).map_err(|e| format!("{e:?}"))?;
    }
    Ok(e)
}
pub fn demo(config: &str, directory: &str, scenario: &str) -> Result<(), String> {
    let scenario = match scenario {
        "recovery" => Scenario::Recovery,
        "forced" => Scenario::ForcedExit,
        _ => return Err("scenario must be recovery or forced".into()),
    };
    let text = fs::read_to_string(config).map_err(|e| e.to_string())?;
    let cfg = parse(&text)?;
    if !cfg.venues.contains(&VenueId(1)) {
        return Err("accounting fixtures require venue 1".into());
    }
    let mut direct = Engine::new(cfg).map_err(|e| format!("{e:?}"))?;
    let dir = Path::new(directory);
    fs::create_dir(dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("inventory.toml"), &text).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("inventory.lri"))
        .map_err(|e| e.to_string())?;
    let mut writer = Writer::new(BufWriter::new(file), recorder::checksum(text.as_bytes()))
        .map_err(|e| e.to_string())?;
    println!("Per-command trace; recovery = harvest - liability:");
    generate(scenario, |event| {
        direct.apply(event).map_err(|e| format!("{e:?}"))?;
        trace(event, &direct)?;
        writer.append(event).map_err(|e| e.to_string())
    })
    .map_err(|e: String| e)?;
    let file = writer.finish().map_err(|e| e.to_string())?;
    file.get_ref().sync_all().map_err(|e| e.to_string())?;
    let replayed = replay_at(dir, &text, cfg)?;
    if direct != replayed {
        return Err("direct and replayed inventory states differ".into());
    }
    println!(
        "Direct and binary replayed complete inventory state match. Stipulated fills; no matching/queue model."
    );
    summary(&replayed)
}
pub fn replay(directory: &str) -> Result<(), String> {
    let dir = Path::new(directory);
    let text = fs::read_to_string(dir.join("inventory.toml")).map_err(|e| e.to_string())?;
    let e = replay_at(dir, &text, parse(&text)?)?;
    summary(&e)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_inventory_configuration() {
        let text = include_str!("../../../config/phase6-inventory.toml");
        assert!(parse(text).is_ok());
        assert!(parse(&text.replace("parent_quantity = 100", "parent_quantity = 101")).is_err());
        assert!(parse(&text.replace("[1, 2, 3]", "[1, 1, 3]")).is_err());
        assert!(parse(&text.replace("\"50\"", "\"0.5\"")).is_err());
        assert!(parse(&format!("extra = true\n{text}")).is_err());
    }
}
