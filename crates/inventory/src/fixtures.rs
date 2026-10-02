//! Accounting-only fixtures: stipulated fills, not a market/queue simulation.
use super::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    Recovery,
    ForcedExit,
}
pub fn generate<E>(
    scenario: Scenario,
    mut emit: impl FnMut(InventoryEvent) -> Result<(), E>,
) -> Result<(), E> {
    use InventoryEventKind::*;
    let rebate = Charges {
        rebate: Money(1),
        ..Charges::default()
    };
    let evidence = RevisitEvidence {
        void_id: 1,
        revisited_at: Timestamp(0),
        valid_until: Timestamp(1_000_000_000),
        qualified: true,
    };
    let mut sequence = 0;
    let mut send = |kind| {
        sequence += 1;
        emit(InventoryEvent {
            sequence,
            timestamp: Timestamp(sequence * 1_000_000),
            kind,
        })
    };
    send(Mark {
        venue: VenueId(1),
        bid: PriceTicks(100),
        ask: PriceTicks(100),
    })?;
    send(ReserveOpen {
        venue: VenueId(1),
        side: Side::Buy,
        role: InventoryRole::Harvest,
        evidence,
    })?;
    send(FillOpen {
        id: 1,
        price: PriceTicks(100),
        maker: true,
        charges: rebate,
    })?;
    match scenario {
        Scenario::Recovery => {
            send(Mark {
                venue: VenueId(1),
                bid: PriceTicks(97),
                ask: PriceTicks(97),
            })?;
            send(ReserveOpen {
                venue: VenueId(1),
                side: Side::Buy,
                role: InventoryRole::Rebalance,
                evidence,
            })?;
            send(FillOpen {
                id: 2,
                price: PriceTicks(97),
                maker: true,
                charges: rebate,
            })?;
            send(Mark {
                venue: VenueId(1),
                bid: PriceTicks(99),
                ask: PriceTicks(99),
            })?;
            send(ReserveClose {
                id: 2,
                expected_price: PriceTicks(99),
                expected_charges: rebate,
                mode: CloseMode::Normal,
            })?;
            send(FillClose {
                id: 2,
                price: PriceTicks(99),
                maker: true,
                charges: rebate,
            })?;
            send(ReserveClose {
                id: 1,
                expected_price: PriceTicks(99),
                expected_charges: rebate,
                mode: CloseMode::Normal,
            })?;
            send(FillClose {
                id: 1,
                price: PriceTicks(99),
                maker: true,
                charges: rebate,
            })?;
        }
        Scenario::ForcedExit => {
            send(Funding {
                id: 1,
                cost: Money(1),
            })?;
            send(Mark {
                venue: VenueId(1),
                bid: PriceTicks(90),
                ask: PriceTicks(90),
            })?;
            send(Halt {
                reason: KillReason::StructureInvalidated,
            })?;
            let fee = Charges {
                fee: Money(2),
                ..Charges::default()
            };
            send(ReserveClose {
                id: 1,
                expected_price: PriceTicks(89),
                expected_charges: fee,
                mode: CloseMode::Emergency,
            })?;
            send(FillClose {
                id: 1,
                price: PriceTicks(89),
                maker: false,
                charges: fee,
            })?;
        }
    }
    Ok(())
}
