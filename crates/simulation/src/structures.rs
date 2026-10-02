//! Semantic lifecycle fixtures, not a matching engine or a profitability simulation.
use common::{InstrumentId, Side, Timestamp, VenueId};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureScenario {
    Revisit,
    LocalVoid,
    RefillBeforeRevisit,
    Continuation,
}
/// All sources use a common tick/lot lattice. Print prices deliberately isolate
/// revisit logic; they do not model depth consumption, queue priority, or fills.
pub fn generate<E>(
    venues: [VenueId; 3],
    instruments: [InstrumentId; 3],
    scenario: StructureScenario,
    mut emit: impl FnMut(MarketEvent) -> Result<(), E>,
) -> Result<(), E> {
    let mut sequences = [0_u64; 3];
    let mut now = 0;
    let mut send = |v: usize, kind, side, price, qty| {
        sequences[v] += 1;
        now += 1_000_000;
        emit(MarketEvent {
            venue: venues[v],
            instrument: instruments[v],
            sequence: sequences[v],
            exchange_sequence: sequences[v],
            exchange_ts: now,
            receive_ts: Timestamp(now),
            event_type: kind,
            side,
            price_ticks: PriceTicks(price),
            qty_units: QtyUnits(qty),
        })
    };
    for v in 0..3 {
        send(v, K::SnapshotStart, Side::Buy, 0, 0)?;
        for p in [80, 92, 96, 100] {
            send(v, K::Add, Side::Buy, p, 100)?;
        }
        for p in [104, 108, 112, 143] {
            send(v, K::Add, Side::Sell, p, 100)?;
        }
        send(v, K::SnapshotEnd, Side::Buy, 0, 0)?;
    }
    for _ in 0..5 {
        send(0, K::Trade, Side::Buy, 102, 1)?;
    }
    for v in 0..3 {
        send(v, K::Modify, Side::Buy, 92, 500)?;
    }
    for _ in 0..4 {
        send(0, K::Trade, Side::Buy, 102, 1)?;
    }
    let count = if scenario == StructureScenario::LocalVoid {
        1
    } else {
        3
    };
    for v in 0..count {
        send(v, K::Cancel, Side::Buy, 100, 0)?;
    }
    for _ in 0..6 {
        send(0, K::Trade, Side::Buy, 102, 1)?;
    }
    send(0, K::Trade, Side::Buy, 110, 1)?;
    if scenario == StructureScenario::Continuation {
        send(0, K::Trade, Side::Buy, 120, 1)?;
        send(0, K::Trade, Side::Buy, 130, 1)?;
        return Ok(());
    }
    if scenario == StructureScenario::RefillBeforeRevisit {
        for v in 0..count {
            send(v, K::Add, Side::Buy, 100, 100)?;
        }
    }
    send(0, K::Trade, Side::Sell, 103, 1)?; // Exact upper-bound touch.
    send(0, K::Trade, Side::Sell, 102, 1)?; // Only one-third penetration; no full traversal.
    send(0, K::Trade, Side::Buy, 110, 1)?;
    if scenario != StructureScenario::RefillBeforeRevisit {
        for v in 0..count {
            send(v, K::Add, Side::Buy, 100, 100)?;
        }
    }
    Ok(())
}
