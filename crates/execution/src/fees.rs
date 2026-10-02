//! Exact fee schedule in parts per million of notional. Fees round UP and rebates round DOWN,
//! so rounding never flatters results. Fixed per-fill atoms may be added on top.
use fixed_point::{Money, PriceTicks, QtyUnits, notional};
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FeeSchedule {
    pub maker_fee_ppm: u32,
    pub maker_rebate_ppm: u32,
    pub taker_fee_ppm: u32,
}
/// Fee and rebate of one fill, both non-negative.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FillCost {
    pub fee: Money,
    pub rebate: Money,
}
pub const PPM: i128 = 1_000_000;
fn ceil_div(n: i128, d: i128) -> i128 {
    n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)
}
impl FeeSchedule {
    pub fn validate(self) -> bool {
        self.maker_fee_ppm <= 100_000
            && self.maker_rebate_ppm <= 100_000
            && self.taker_fee_ppm <= 100_000
    }
    /// `None` when the notional is not positive.
    pub fn cost(self, price: PriceTicks, qty: QtyUnits, maker: bool) -> Option<FillCost> {
        let n = notional(price, qty).0;
        if n <= 0 {
            return None;
        }
        let (fee, rebate) = if maker {
            (self.maker_fee_ppm, self.maker_rebate_ppm)
        } else {
            (self.taker_fee_ppm, 0)
        };
        Some(FillCost {
            fee: Money(ceil_div(n * i128::from(fee), PPM)),
            rebate: Money(n * i128::from(rebate) / PPM),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fees_round_up_and_rebates_round_down() {
        let s = FeeSchedule {
            maker_fee_ppm: 200,
            maker_rebate_ppm: 50,
            taker_fee_ppm: 500,
        };
        // Notional 867_801 ticks x 10 units = 8_678_010 atoms.
        let maker = s.cost(PriceTicks(867_801), QtyUnits(10), true).unwrap();
        assert_eq!(maker.fee, Money(1_736)); // 1735.602 rounded up
        assert_eq!(maker.rebate, Money(433)); // 433.9005 rounded down
        let taker = s.cost(PriceTicks(867_801), QtyUnits(10), false).unwrap();
        assert_eq!((taker.fee, taker.rebate), (Money(4_340), Money(0))); // 4339.005 up
        let exact = s.cost(PriceTicks(1_000_000), QtyUnits(10), true).unwrap();
        assert_eq!(exact.fee, Money(2_000));
        assert!(s.cost(PriceTicks(0), QtyUnits(10), true).is_none());
        assert!(
            !FeeSchedule {
                taker_fee_ppm: 100_001,
                ..s
            }
            .validate()
        );
    }
}
