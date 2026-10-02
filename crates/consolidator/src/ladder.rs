use crate::ConsolidationError;
use common::Side;
use fixed_point::PriceTicks;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConsolidatedLevel {
    pub price: PriceTicks,
    pub weighted_quantity_microunits: i128,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ladder<const N: usize> {
    levels: [ConsolidatedLevel; N],
    len: usize,
    side: Side,
    total: i128,
}
impl<const N: usize> Ladder<N> {
    pub fn new(side: Side) -> Self {
        Self {
            levels: [ConsolidatedLevel::default(); N],
            len: 0,
            side,
            total: 0,
        }
    }
    pub fn levels(&self) -> &[ConsolidatedLevel] {
        &self.levels[..self.len]
    }
    pub fn total(&self) -> i128 {
        self.total
    }
    pub fn change(&mut self, price: PriceTicks, delta: i128) -> Result<(), ConsolidationError> {
        if delta == 0 {
            return Ok(());
        }
        let total = self
            .total
            .checked_add(delta)
            .ok_or(ConsolidationError::Overflow)?;
        if total < 0 {
            return Err(ConsolidationError::Invariant);
        }
        let at = self.levels().binary_search_by(|l| match self.side {
            Side::Buy => price.cmp(&l.price),
            Side::Sell => l.price.cmp(&price),
        });
        match at {
            Ok(i) => {
                let qty = self.levels[i]
                    .weighted_quantity_microunits
                    .checked_add(delta)
                    .ok_or(ConsolidationError::Overflow)?;
                if qty < 0 {
                    return Err(ConsolidationError::Invariant);
                }
                if qty == 0 {
                    self.levels.copy_within(i + 1..self.len, i);
                    self.len -= 1;
                    self.levels[self.len] = ConsolidatedLevel::default();
                } else {
                    self.levels[i].weighted_quantity_microunits = qty;
                }
            }
            Err(i) => {
                if delta < 0 {
                    return Err(ConsolidationError::Invariant);
                }
                if self.len == N {
                    return Err(ConsolidationError::Capacity);
                }
                self.levels.copy_within(i..self.len, i + 1);
                self.levels[i] = ConsolidatedLevel {
                    price,
                    weighted_quantity_microunits: delta,
                };
                self.len += 1;
            }
        }
        self.total = total;
        Ok(())
    }
}
