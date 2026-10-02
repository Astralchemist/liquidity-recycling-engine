//! Pure inventory approval limits. This is not an exchange gateway or cancel/flatten executor.
use common::Side;
use fixed_point::Money;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    SequenceGap,
    StaleData,
    ClockAnomaly,
    Disconnect,
    AckTimeout,
    InventoryBreach,
    PnlBreach,
    CorruptBook,
    UnhandledState,
    ExcessiveLatency,
    PositionAge,
    EpisodeDuration,
    AdverseEpisodes,
    StructureInvalidated,
    /// Configured environment states (for example a liquidity shock) while inventory is held.
    RegimeChange,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryLimits {
    pub max_net_units: i64,
    pub max_gross_units: i64,
    pub max_venue_units: i64,
    pub max_target_deviation: i64,
    pub max_open_orders: usize,
    pub max_liability: Money,
    pub max_drawdown: Money,
    pub max_position_age_ns: u64,
    pub max_episode_ns: u64,
    pub max_adverse_episodes: u32,
    pub mark_stale_ns: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    InvalidLimits,
    InvalidExposure,
    Net,
    Gross,
    Venue,
    Target,
    Orders,
    Halted,
    NoMark,
    NotQualified,
    NoBenefit,
    Capacity,
    Overflow,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedExposure {
    pub net: i64,
    pub gross: i64,
    pub pending_buys: i64,
    pub pending_sells: i64,
    pub pending_open_units: i64,
    pub venue_gross_and_pending: i64,
    pub open_orders: usize,
}
impl ReservedExposure {
    fn validate(self) -> Result<(), Denial> {
        if self.gross < 0
            || i128::from(self.net).abs() > i128::from(self.gross)
            || self.pending_buys < 0
            || self.pending_sells < 0
            || self.pending_open_units < 0
            || self.venue_gross_and_pending < 0
            || i128::from(self.pending_open_units)
                > i128::from(self.pending_buys) + i128::from(self.pending_sells)
        {
            return Err(Denial::InvalidExposure);
        }
        Ok(())
    }
}
impl InventoryLimits {
    pub fn validate(self) -> Result<(), Denial> {
        if self.max_net_units > self.max_gross_units
            || self.max_venue_units > self.max_gross_units
            || self.max_net_units <= 0
            || self.max_gross_units <= 0
            || self.max_venue_units <= 0
            || self.max_target_deviation < 0
            || self.max_open_orders == 0
            || self.max_liability.0 <= 0
            || self.max_drawdown.0 <= 0
            || self.max_position_age_ns == 0
            || self.max_episode_ns == 0
            || self.max_adverse_episodes == 0
            || self.mark_stale_ns == 0
        {
            return Err(Denial::InvalidLimits);
        }
        Ok(())
    }
    pub fn approve_close(self, e: ReservedExposure, side: Side, target: i64) -> Result<(), Denial> {
        self.validate()?;
        e.validate()?;
        if e.open_orders >= self.max_open_orders {
            return Err(Denial::Orders);
        }
        let upper = i128::from(e.net) + i128::from(e.pending_buys) + i128::from(side == Side::Buy);
        let lower =
            i128::from(e.net) - i128::from(e.pending_sells) - i128::from(side == Side::Sell);
        if lower < -i128::from(self.max_net_units) || upper > i128::from(self.max_net_units) {
            return Err(Denial::Net);
        }
        if (upper - i128::from(target)).abs() > i128::from(self.max_target_deviation)
            || (lower - i128::from(target)).abs() > i128::from(self.max_target_deviation)
        {
            return Err(Denial::Target);
        }
        Ok(())
    }
    /// Pending closes do not release capacity. Check both possible fill-order extremes.
    pub fn approve_open(self, e: ReservedExposure, side: Side, target: i64) -> Result<(), Denial> {
        self.validate()?;
        e.validate()?;
        let buys = e
            .pending_buys
            .checked_add(i64::from(side == Side::Buy))
            .ok_or(Denial::Overflow)?;
        let sells = e
            .pending_sells
            .checked_add(i64::from(side == Side::Sell))
            .ok_or(Denial::Overflow)?;
        let upper = e.net.checked_add(buys).ok_or(Denial::Overflow)?;
        let lower = e.net.checked_sub(sells).ok_or(Denial::Overflow)?;
        if lower < -self.max_net_units || upper > self.max_net_units {
            return Err(Denial::Net);
        }
        if (i128::from(upper) - i128::from(target)).abs() > i128::from(self.max_target_deviation)
            || (i128::from(lower) - i128::from(target)).abs()
                > i128::from(self.max_target_deviation)
        {
            return Err(Denial::Target);
        }
        if i128::from(e.gross) + i128::from(e.pending_open_units) + 1
            > i128::from(self.max_gross_units)
        {
            return Err(Denial::Gross);
        }
        if e.venue_gross_and_pending >= self.max_venue_units {
            return Err(Denial::Venue);
        }
        if e.open_orders >= self.max_open_orders {
            return Err(Denial::Orders);
        }
        Ok(())
    }
}
