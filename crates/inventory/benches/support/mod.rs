//! Steady benchmark workload: six resting children plus repeated one-quantum recycle cycles.
#![allow(dead_code)]
use common::{Side, Timestamp, VenueId};
use fixed_point::{InventoryUnits, Money, PriceTicks, QtyUnits, Quantum};
use inventory::*;
use risk::InventoryLimits;
pub type Small = Ledger<3, 32, 8>;
pub type Large = Ledger<3, 256, 128>;
/// Age, staleness, episode and loss thresholds are disabled so measurement never halts.
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
            max_liability: Money(1 << 100),
            max_drawdown: Money(1 << 100),
            max_position_age_ns: u64::MAX / 2,
            max_episode_ns: u64::MAX / 2,
            max_adverse_episodes: u32::MAX,
            mark_stale_ns: u64::MAX / 2,
        },
    }
}
pub struct Driver {
    sequence: u64,
    now: u64,
    next_id: u64,
    marks: u64,
    phase: u64,
    mixed: u64,
}
const REBATE: Charges = Charges {
    rebate: Money(1),
    fee: Money(0),
    slippage: Money(0),
};
const EVIDENCE: RevisitEvidence = RevisitEvidence {
    void_id: 1,
    revisited_at: Timestamp(0),
    valid_until: Timestamp(u64::MAX),
    qualified: true,
};
impl Driver {
    fn event(&mut self, kind: InventoryEventKind) -> InventoryEvent {
        self.sequence += 1;
        self.now += 1;
        InventoryEvent {
            sequence: self.sequence,
            timestamp: Timestamp(self.now),
            kind,
        }
    }
    /// Round-robin venue mark; bid oscillates 99..=101 with a one-tick spread.
    pub fn mark(&mut self) -> InventoryEvent {
        let venue = VenueId((self.marks % 3) as u16 + 1);
        let bid = 99 + (self.marks / 3 % 3) as i64;
        self.marks += 1;
        self.event(InventoryEventKind::Mark {
            venue,
            bid: PriceTicks(bid),
            ask: PriceTicks(bid + 1),
        })
    }
    /// Reserve, fill, profitable close reservation and close fill of one long child.
    pub fn cycle(&mut self) -> InventoryEvent {
        let id = self.next_id;
        let kind = match self.phase {
            0 => InventoryEventKind::ReserveOpen {
                venue: VenueId(1),
                side: Side::Buy,
                role: InventoryRole::Harvest,
                evidence: EVIDENCE,
            },
            1 => InventoryEventKind::FillOpen {
                id,
                price: PriceTicks(100),
                maker: true,
                charges: REBATE,
            },
            2 => InventoryEventKind::ReserveClose {
                id,
                expected_price: PriceTicks(101),
                expected_charges: REBATE,
                mode: CloseMode::Normal,
            },
            _ => {
                self.next_id += 1;
                InventoryEventKind::FillClose {
                    id,
                    price: PriceTicks(101),
                    maker: true,
                    charges: REBATE,
                }
            }
        };
        self.phase = (self.phase + 1) % 4;
        self.event(kind)
    }
    /// Four marks then one cycle step: market data dominates inventory commands.
    pub fn mixed(&mut self) -> InventoryEvent {
        self.mixed += 1;
        if self.mixed % 5 == 0 {
            self.cycle()
        } else {
            self.mark()
        }
    }
    pub fn tick(&mut self) -> InventoryEvent {
        self.event(InventoryEventKind::Tick)
    }
}
/// Marks every venue, then holds one long and one short child per venue (gross 6, net 0).
pub fn setup<const L: usize, const E: usize>() -> (Ledger<3, L, E>, Driver) {
    let mut ledger = Ledger::<3, L, E>::new(config()).unwrap();
    let mut d = Driver {
        sequence: 0,
        now: 0,
        next_id: 1,
        marks: 0,
        phase: 0,
        mixed: 0,
    };
    for v in 1..=3 {
        let mark = d.event(InventoryEventKind::Mark {
            venue: VenueId(v),
            bid: PriceTicks(100),
            ask: PriceTicks(100),
        });
        ledger.apply(mark).unwrap();
    }
    for v in 1..=3 {
        for side in [Side::Buy, Side::Sell] {
            let reserve = d.event(InventoryEventKind::ReserveOpen {
                venue: VenueId(v),
                side,
                role: InventoryRole::Harvest,
                evidence: EVIDENCE,
            });
            let Outcome::Reserved(id) = ledger.apply(reserve).unwrap() else {
                panic!("reservation expected")
            };
            let fill = d.event(InventoryEventKind::FillOpen {
                id,
                price: PriceTicks(100),
                maker: true,
                charges: REBATE,
            });
            ledger.apply(fill).unwrap();
            d.next_id = id + 1;
        }
    }
    assert_eq!(ledger.exposure().gross(), 6);
    assert!(ledger.active_episode().is_some());
    (ledger, d)
}
