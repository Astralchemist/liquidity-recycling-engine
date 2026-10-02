use common::*;
use fixed_point::*;
use inventory::{
    fixtures::{Scenario, generate},
    journal::*,
    *,
};
use risk::KillReason;
mod support;
#[test]
fn binary_replay_matches_ledger_after_every_command() {
    for scenario in [Scenario::Recovery, Scenario::ForcedExit] {
        let mut writer = Writer::new(Vec::new(), 123).unwrap();
        let mut input = Vec::new();
        generate(scenario, |e| {
            writer.append(e).unwrap();
            input.push(e);
            Ok::<_, ()>(())
        })
        .unwrap();
        let bytes = writer.finish().unwrap();
        let mut reader = Reader::new(bytes.as_slice(), 123).unwrap();
        let (mut direct, mut replayed) = (support::engine(), support::engine());
        for expected in input {
            let actual = reader.next_event().unwrap().unwrap();
            assert_eq!(actual, expected);
            direct.apply(expected).unwrap();
            replayed.apply(actual).unwrap();
            assert_eq!(direct, replayed);
        }
        assert!(reader.next_event().unwrap().is_none());
        assert!(Reader::new(bytes.as_slice(), 124).is_err());
        let mut truncated = Reader::new(&bytes[..bytes.len() - 1], 123).unwrap();
        let mut failed = false;
        loop {
            match truncated.next_event() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed);
    }
}
#[test]
fn every_variant_roundtrips_and_single_bit_corruption_is_rejected() {
    let cases = [
        InventoryEventKind::Mark {
            venue: VenueId(2),
            bid: PriceTicks(99),
            ask: PriceTicks(101),
        },
        InventoryEventKind::FillOpen {
            id: 5,
            price: PriceTicks(100),
            maker: true,
            charges: Charges {
                rebate: Money(1),
                fee: Money(0),
                slippage: Money(0),
            },
        },
        InventoryEventKind::FillClose {
            id: 5,
            price: PriceTicks(98),
            maker: false,
            charges: Charges {
                rebate: Money(0),
                fee: Money(2),
                slippage: Money(1),
            },
        },
        InventoryEventKind::CancelOpen { id: 7 },
        InventoryEventKind::CancelClose { id: 9 },
        InventoryEventKind::Funding {
            id: 1,
            cost: Money(-123),
        },
        InventoryEventKind::Halt {
            reason: KillReason::AckTimeout,
        },
        InventoryEventKind::Tick,
        InventoryEventKind::ReserveOpen {
            venue: VenueId(3),
            side: Side::Sell,
            role: InventoryRole::Residual,
            evidence: support::evidence(),
        },
        InventoryEventKind::ReserveClose {
            id: 42,
            expected_price: PriceTicks(999),
            expected_charges: Charges {
                rebate: Money(3),
                fee: Money(4),
                slippage: Money(5),
            },
            mode: CloseMode::Emergency,
        },
    ];
    for kind in cases {
        let e = InventoryEvent {
            sequence: 9,
            timestamp: Timestamp(33),
            kind,
        };
        let bytes = encode(e);
        assert_eq!(decode(bytes).unwrap(), e);
        for offset in 0..RECORD_SIZE {
            let mut bad = bytes;
            bad[offset] ^= 1;
            assert!(decode(bad).is_err());
        }
    }
    let mut padding = encode(InventoryEvent {
        sequence: 1,
        timestamp: Timestamp(1),
        kind: InventoryEventKind::Tick,
    });
    padding[40] = 1;
    let crc = recorder::checksum(&padding[..156]);
    padding[156..].copy_from_slice(&crc.to_le_bytes());
    assert!(decode(padding).is_err());
}
