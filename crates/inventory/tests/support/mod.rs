#![allow(dead_code)]
use common::{Timestamp, VenueId};
use fixed_point::*;
use inventory::*;
use risk::InventoryLimits;
pub type Engine = Ledger<3, 32, 8>;
pub fn config() -> InventoryConfig<3> {
    InventoryConfig {
        venues: [VenueId(1), VenueId(2), VenueId(3)],
        quantum: Quantum::new(QtyUnits(100), 1, 10).unwrap(),
        target: InventoryUnits(0),
        target_tolerance: 0,
        completion_max_gross: 0,
        limits: InventoryLimits {
            max_net_units: 4,
            max_gross_units: 8,
            max_venue_units: 6,
            max_target_deviation: 4,
            max_open_orders: 8,
            max_liability: Money(50),
            max_drawdown: Money(1000),
            max_position_age_ns: 1_000_000_000,
            max_episode_ns: 1_000_000_000,
            max_adverse_episodes: 3,
            mark_stale_ns: 1_000_000_000,
        },
    }
}
pub fn engine() -> Engine {
    Engine::new(config()).unwrap()
}
pub fn apply(e: &mut Engine, kind: InventoryEventKind) -> Result<Outcome, InventoryError> {
    e.apply(InventoryEvent {
        sequence: e.last_sequence() + 1,
        timestamp: Timestamp(e.now().0 + 1),
        kind,
    })
}
pub fn evidence() -> RevisitEvidence {
    RevisitEvidence {
        void_id: 1,
        revisited_at: Timestamp(0),
        valid_until: Timestamp(u64::MAX),
        qualified: true,
    }
}
pub fn mark(e: &mut Engine, p: i64) {
    apply(
        e,
        InventoryEventKind::Mark {
            venue: VenueId(1),
            bid: PriceTicks(p),
            ask: PriceTicks(p),
        },
    )
    .unwrap();
}
pub fn open(e: &mut Engine, side: common::Side, price: i64) -> u64 {
    let Outcome::Reserved(id) = apply(
        e,
        InventoryEventKind::ReserveOpen {
            venue: VenueId(1),
            side,
            role: InventoryRole::Harvest,
            evidence: evidence(),
        },
    )
    .unwrap() else {
        panic!()
    };
    apply(
        e,
        InventoryEventKind::FillOpen {
            id,
            price: PriceTicks(price),
            maker: true,
            charges: Charges::default(),
        },
    )
    .unwrap();
    id
}
pub fn reserve_close(
    e: &mut Engine,
    id: u64,
    price: i64,
    mode: CloseMode,
) -> Result<Outcome, InventoryError> {
    apply(
        e,
        InventoryEventKind::ReserveClose {
            id,
            expected_price: PriceTicks(price),
            expected_charges: Charges::default(),
            mode,
        },
    )
}
pub fn close(e: &mut Engine, id: u64, price: i64, mode: CloseMode) {
    reserve_close(e, id, price, mode).unwrap();
    apply(
        e,
        InventoryEventKind::FillClose {
            id,
            price: PriceTicks(price),
            maker: true,
            charges: Charges::default(),
        },
    )
    .unwrap();
}
