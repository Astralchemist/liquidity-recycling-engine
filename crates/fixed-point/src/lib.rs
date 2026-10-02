//! Exact arithmetic. Money is in tick × quantity-unit atoms for ONE instrument.
//! Cross-instrument/venue money requires an explicit settlement normalization layer.
use common::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticError {
    Overflow,
    InvalidScale,
    Inexact,
    InvalidDecimal,
}

macro_rules! integer_type {
    ($(#[$attr:meta])* $name:ident, $inner:ty) => {
        $(#[$attr])*
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(pub $inner);
        impl $name {
            pub fn checked_add(self, rhs: Self) -> Result<Self, ArithmeticError> {
                self.0.checked_add(rhs.0).map(Self).ok_or(ArithmeticError::Overflow)
            }
            pub fn checked_sub(self, rhs: Self) -> Result<Self, ArithmeticError> {
                self.0.checked_sub(rhs.0).map(Self).ok_or(ArithmeticError::Overflow)
            }
        }
    }
}
integer_type!(PriceTicks, i64);
integer_type!(QtyUnits, i64);
integer_type!(Money, i128);
integer_type!(InventoryUnits, i64);
integer_type!(
    #[doc = "Millionths of one basis point; division rounds toward zero."]
    BasisPoints,
    i64
);

impl BasisPoints {
    pub const SCALE: i128 = 1_000_000;
    pub fn from_ratio(numerator: i64, denominator: i64) -> Result<Self, ArithmeticError> {
        if denominator <= 0 {
            return Err(ArithmeticError::InvalidScale);
        }
        let raw = i128::from(numerator) * 10_000 * Self::SCALE / i128::from(denominator);
        i64::try_from(raw)
            .map(Self)
            .map_err(|_| ArithmeticError::Overflow)
    }
}

pub fn notional(price: PriceTicks, qty: QtyUnits) -> Money {
    Money(i128::from(price.0) * i128::from(qty.0))
}

pub fn realized_pnl(
    side: Side,
    entry: PriceTicks,
    exit: PriceTicks,
    qty: QtyUnits,
) -> Result<Money, ArithmeticError> {
    if qty.0 < 0 {
        return Err(ArithmeticError::InvalidScale);
    }
    let movement = match side {
        Side::Buy => i128::from(exit.0) - i128::from(entry.0),
        Side::Sell => i128::from(entry.0) - i128::from(exit.0),
    };
    movement
        .checked_mul(i128::from(qty.0))
        .map(Money)
        .ok_or(ArithmeticError::Overflow)
}

/// A parent quantity must divide EXACTLY into configured fractional child units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quantum {
    qty: QtyUnits,
}
impl Quantum {
    pub fn new(
        parent: QtyUnits,
        numerator: u32,
        denominator: u32,
    ) -> Result<Self, ArithmeticError> {
        if parent.0 <= 0 || numerator == 0 || denominator == 0 || numerator > denominator {
            return Err(ArithmeticError::InvalidScale);
        }
        let product = i128::from(parent.0) * i128::from(numerator);
        if product % i128::from(denominator) != 0 {
            return Err(ArithmeticError::Inexact);
        }
        let qty = i64::try_from(product / i128::from(denominator))
            .map_err(|_| ArithmeticError::Overflow)?;
        Ok(Self { qty: QtyUnits(qty) })
    }
    pub fn quantity(self, units: InventoryUnits) -> Result<QtyUnits, ArithmeticError> {
        self.qty
            .0
            .checked_mul(units.0)
            .map(QtyUnits)
            .ok_or(ArithmeticError::Overflow)
    }
    pub fn unit_quantity(self) -> QtyUnits {
        self.qty
    }
}

/// Boundary-only strict decimal parser. No float conversion or silent rounding.
pub fn parse_ticks(
    input: &str,
    decimal_places: u32,
    tick_atoms: i64,
) -> Result<PriceTicks, ArithmeticError> {
    if decimal_places > 18 || tick_atoms <= 0 {
        return Err(ArithmeticError::InvalidScale);
    }
    let (negative, body) = match input.strip_prefix('-') {
        Some(s) => (true, s),
        None => (false, input),
    };
    let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || !fraction.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() > decimal_places as usize
    {
        return Err(ArithmeticError::InvalidDecimal);
    }
    let scale = 10_i128.pow(decimal_places);
    let w: i128 = whole.parse().map_err(|_| ArithmeticError::Overflow)?;
    let f: i128 = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse()
            .map_err(|_| ArithmeticError::InvalidDecimal)?
    };
    let atoms = w
        .checked_mul(scale)
        .and_then(|x| x.checked_add(f * 10_i128.pow(decimal_places - fraction.len() as u32)))
        .ok_or(ArithmeticError::Overflow)?;
    let atoms = if negative { -atoms } else { atoms };
    if atoms % i128::from(tick_atoms) != 0 {
        return Err(ArithmeticError::Inexact);
    }
    i64::try_from(atoms / i128::from(tick_atoms))
        .map(PriceTicks)
        .map_err(|_| ArithmeticError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimals_are_exact() {
        assert_eq!(parse_ticks("61742.53", 2, 1), Ok(PriceTicks(6_174_253)));
        assert_eq!(parse_ticks("1.23", 2, 5), Err(ArithmeticError::Inexact));
        assert_eq!(
            parse_ticks("NaN", 2, 1),
            Err(ArithmeticError::InvalidDecimal)
        );
        assert!(parse_ticks("1.001", 2, 1).is_err());
        assert_eq!(
            parse_ticks("-9223372036854775808", 0, 1),
            Ok(PriceTicks(i64::MIN))
        );
    }
    #[test]
    fn quantum_and_overflow() {
        let q = Quantum::new(QtyUnits(100), 1, 10).unwrap();
        assert_eq!(q.quantity(InventoryUnits(-2)), Ok(QtyUnits(-20)));
        assert_eq!(
            Quantum::new(QtyUnits(101), 1, 10),
            Err(ArithmeticError::Inexact)
        );
        assert!(q.quantity(InventoryUnits(i64::MAX)).is_err());
        assert!(Money(i128::MAX).checked_add(Money(1)).is_err());
    }
    #[test]
    fn pnl_extremes_and_basis_points() {
        assert_eq!(
            realized_pnl(
                Side::Buy,
                PriceTicks(i64::MIN),
                PriceTicks(i64::MAX),
                QtyUnits(1)
            ),
            Ok(Money(u64::MAX as i128))
        );
        assert_eq!(
            BasisPoints::from_ratio(1, 10_000),
            Ok(BasisPoints(1_000_000))
        );
        assert!(BasisPoints::from_ratio(1, 0).is_err());
    }
}
