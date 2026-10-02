use common::Side;
use fixed_point::*;

// Exhaustive bounded property domain, supplemented by i64 boundary unit tests.
#[test]
fn pnl_conservation_and_inventory_quantization() {
    for entry in 1..=100 {
        for exit in 1..=100 {
            for qty in [0, 1, 10, 999, i64::MAX] {
                let long = realized_pnl(
                    Side::Buy,
                    PriceTicks(entry),
                    PriceTicks(exit),
                    QtyUnits(qty),
                )
                .unwrap();
                let short = realized_pnl(
                    Side::Sell,
                    PriceTicks(entry),
                    PriceTicks(exit),
                    QtyUnits(qty),
                )
                .unwrap();
                assert_eq!(long.checked_add(short), Ok(Money(0)));
                assert_eq!(
                    long,
                    notional(PriceTicks(exit), QtyUnits(qty))
                        .checked_sub(notional(PriceTicks(entry), QtyUnits(qty)))
                        .unwrap()
                );
            }
        }
    }
    for parent in 1..=1000 {
        let q = Quantum::new(QtyUnits(parent * 10), 1, 10).unwrap();
        for units in -100..=100 {
            assert_eq!(
                q.quantity(InventoryUnits(units)),
                Ok(QtyUnits(parent * units))
            );
        }
    }
}
#[test]
fn decimal_conversion_round_trip() {
    for atoms in -10_000_i64..=10_000 {
        let sign = if atoms < 0 { "-" } else { "" };
        let absolute = atoms.abs();
        let s = format!("{sign}{}.{:02}", absolute / 100, absolute % 100);
        assert_eq!(parse_ticks(&s, 2, 1), Ok(PriceTicks(atoms)));
    }
}
