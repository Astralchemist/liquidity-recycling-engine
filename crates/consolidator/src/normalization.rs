//! Exact scale conversion. No FX, inverse contracts, or implicit contract equivalence.
use common::InstrumentId;
use fixed_point::{ArithmeticError, Money, PriceTicks, QtyUnits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    Spot,
    Linear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketIdentity {
    pub base_asset: u32,
    pub quote_asset: u32,
    pub settlement_asset: u32,
    pub kind: ContractKind,
    /// Explicitly curated group: e.g. identical expiry and payoff semantics.
    pub equivalence_group: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitScale {
    pub atoms: i64,
    pub decimals: u8,
}
impl UnitScale {
    fn valid(self) -> bool {
        self.atoms > 0 && self.decimals <= 18
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentMetadata {
    pub instrument: InstrumentId,
    pub market: MarketIdentity,
    /// Quote currency per price tick.
    pub price: UnitScale,
    /// Base asset per quantity unit, including linear contract multiplier.
    pub quantity: UnitScale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationError {
    IncompatibleMarket,
    InvalidScale,
    Arithmetic(ArithmeticError),
}
impl From<ArithmeticError> for NormalizationError {
    fn from(e: ArithmeticError) -> Self {
        Self::Arithmetic(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ratio {
    numerator: i128,
    denominator: i128,
}
impl Ratio {
    fn new(numerator: i128, denominator: i128) -> Self {
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        Self {
            numerator: numerator / a,
            denominator: denominator / a,
        }
    }
    fn between(source: UnitScale, target: UnitScale) -> Result<Self, NormalizationError> {
        if !source.valid() || !target.valid() {
            return Err(NormalizationError::InvalidScale);
        }
        Ok(Self::new(
            i128::from(source.atoms) * 10_i128.pow(u32::from(target.decimals)),
            i128::from(target.atoms) * 10_i128.pow(u32::from(source.decimals)),
        ))
    }
    fn apply(self, value: i128) -> Result<i128, ArithmeticError> {
        if value % self.denominator != 0 {
            return Err(ArithmeticError::Inexact);
        }
        (value / self.denominator)
            .checked_mul(self.numerator)
            .ok_or(ArithmeticError::Overflow)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalizer {
    price: Ratio,
    quantity: Ratio,
}
impl Normalizer {
    pub fn new(
        source: InstrumentMetadata,
        target: InstrumentMetadata,
    ) -> Result<Self, NormalizationError> {
        if source.market != target.market {
            return Err(NormalizationError::IncompatibleMarket);
        }
        Ok(Self {
            price: Ratio::between(source.price, target.price)?,
            quantity: Ratio::between(source.quantity, target.quantity)?,
        })
    }
    pub fn price(self, value: PriceTicks) -> Result<PriceTicks, NormalizationError> {
        Ok(PriceTicks(
            i64::try_from(self.price.apply(i128::from(value.0))?)
                .map_err(|_| ArithmeticError::Overflow)?,
        ))
    }
    pub fn quantity(self, value: QtyUnits) -> Result<QtyUnits, NormalizationError> {
        Ok(QtyUnits(
            i64::try_from(self.quantity.apply(i128::from(value.0))?)
                .map_err(|_| ArithmeticError::Overflow)?,
        ))
    }
    pub fn notional(self, price: PriceTicks, qty: QtyUnits) -> Result<Money, NormalizationError> {
        Ok(fixed_point::notional(
            self.price(price)?,
            self.quantity(qty)?,
        ))
    }
}
/// Convert common-grid Money to an exact number of quote settlement atoms.
/// Physical settlement asset MUST equal quote asset; no implicit FX conversion.
pub fn settlement_money(
    money: Money,
    grid: InstrumentMetadata,
    decimals: u8,
) -> Result<Money, NormalizationError> {
    if !grid.price.valid() || !grid.quantity.valid() || decimals > 18 {
        return Err(NormalizationError::InvalidScale);
    }
    if grid.market.quote_asset != grid.market.settlement_asset {
        return Err(NormalizationError::IncompatibleMarket);
    }
    let scale = Ratio::new(
        i128::from(grid.price.atoms) * i128::from(grid.quantity.atoms),
        10_i128.pow(u32::from(grid.price.decimals) + u32::from(grid.quantity.decimals)),
    );
    // Cancel decimal factors before multiplying, preserving representable exact results.
    let atom_scale = Ratio::new(10_i128.pow(u32::from(decimals)), scale.denominator);
    let numerator = scale
        .numerator
        .checked_mul(atom_scale.numerator)
        .ok_or(ArithmeticError::Overflow)?;
    Ok(Money(
        Ratio::new(numerator, atom_scale.denominator).apply(money.0)?,
    ))
}
