//! Exact one-quantum child ledger with bounded history and transactional command processing.
#![forbid(unsafe_code)]
pub mod fixtures;
pub mod journal;
pub mod ledger;
use common::{Side, Timestamp, VenueId};
use fixed_point::{ArithmeticError, InventoryUnits, Money, PriceTicks, Quantum};
pub use ledger::*;
use risk::{Denial, InventoryLimits, KillReason};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryRole {
    Harvest,
    Rebalance,
    Residual,
    ExitPending,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InventoryTarget(pub InventoryUnits);
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Charges {
    pub rebate: Money,
    pub fee: Money,
    /// Separate cash debit ONLY; do not repeat slippage already embedded in fill price.
    pub slippage: Money,
}
impl Charges {
    pub fn validate(self) -> Result<(), InventoryError> {
        if self.rebate.0 < 0 || self.fee.0 < 0 || self.slippage.0 < 0 {
            return Err(InventoryError::InvalidInput);
        }
        Ok(())
    }
    pub fn net(self) -> Result<Money, InventoryError> {
        Ok(self
            .rebate
            .checked_sub(self.fee)?
            .checked_sub(self.slippage)?)
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Accounts {
    pub realized: Money,
    pub rebates: Money,
    pub fees: Money,
    pub slippage: Money,
    pub funding: Money,
}
impl Accounts {
    pub fn harvest(self) -> Result<Money, InventoryError> {
        Ok(self
            .realized
            .checked_add(self.rebates)?
            .checked_sub(self.fees)?
            .checked_sub(self.slippage)?
            .checked_sub(self.funding)?)
    }
    pub(crate) fn charge(&mut self, c: Charges) -> Result<(), InventoryError> {
        self.rebates = self.rebates.checked_add(c.rebate)?;
        self.fees = self.fees.checked_add(c.fee)?;
        self.slippage = self.slippage.checked_add(c.slippage)?;
        Ok(())
    }
    pub fn difference(self, start: Self) -> Result<Self, InventoryError> {
        Ok(Self {
            realized: self.realized.checked_sub(start.realized)?,
            rebates: self.rebates.checked_sub(start.rebates)?,
            fees: self.fees.checked_sub(start.fees)?,
            slippage: self.slippage.checked_sub(start.slippage)?,
            funding: self.funding.checked_sub(start.funding)?,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisitEvidence {
    pub void_id: u64,
    pub revisited_at: Timestamp,
    pub valid_until: Timestamp,
    pub qualified: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseMode {
    Normal,
    Emergency,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryConfig<const V: usize> {
    pub venues: [VenueId; V],
    pub quantum: Quantum,
    pub target: InventoryUnits,
    pub target_tolerance: i64,
    pub completion_max_gross: i64,
    pub limits: InventoryLimits,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryError {
    InvalidConfig,
    InvalidInput,
    SequenceGap,
    ClockRegression,
    UnknownVenue,
    UnknownId,
    AlreadyPending,
    NotPending,
    Denied(Denial),
    Arithmetic(ArithmeticError),
}
impl From<ArithmeticError> for InventoryError {
    fn from(e: ArithmeticError) -> Self {
        Self::Arithmetic(e)
    }
}
impl From<Denial> for InventoryError {
    fn from(e: Denial) -> Self {
        Self::Denied(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryEventKind {
    Mark {
        venue: VenueId,
        bid: PriceTicks,
        ask: PriceTicks,
    },
    ReserveOpen {
        venue: VenueId,
        side: Side,
        role: InventoryRole,
        evidence: RevisitEvidence,
    },
    FillOpen {
        id: u64,
        price: PriceTicks,
        maker: bool,
        charges: Charges,
    },
    CancelOpen {
        id: u64,
    },
    ReserveClose {
        id: u64,
        expected_price: PriceTicks,
        expected_charges: Charges,
        mode: CloseMode,
    },
    FillClose {
        id: u64,
        price: PriceTicks,
        maker: bool,
        charges: Charges,
    },
    CancelClose {
        id: u64,
    },
    Funding {
        id: u64,
        cost: Money,
    },
    Halt {
        reason: KillReason,
    },
    Tick,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryEvent {
    pub sequence: u64,
    pub timestamp: Timestamp,
    pub kind: InventoryEventKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Reserved(u64),
    Opened(u64),
    Closed(u64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub bid: PriceTicks,
    pub ask: PriceTicks,
    pub timestamp: Timestamp,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub id: u64,
    pub venue: VenueId,
    pub side: Side,
    pub role: InventoryRole,
    pub evidence: RevisitEvidence,
    pub timestamp: Timestamp,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lot {
    pub id: u64,
    pub venue: VenueId,
    pub side: Side,
    pub entry_price: PriceTicks,
    pub quantity: fixed_point::QtyUnits,
    pub entered_at: Timestamp,
    pub entry_charges: Charges,
    pub maker_entry: bool,
    pub funding: Money,
    pub unrealized: Money,
    pub role: InventoryRole,
    pub pending_close: Option<CloseMode>,
    pub(crate) previous_role: InventoryRole,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedLot {
    pub lot: Lot,
    pub exit_price: PriceTicks,
    pub exited_at: Timestamp,
    pub exit_charges: Charges,
    pub maker_exit: bool,
    pub realized: Money,
    pub net_pnl: Money,
    pub mode: CloseMode,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Exposure {
    pub long: i64,
    pub short: i64,
    pub pending_buys: i64,
    pub pending_sells: i64,
    pub closing_buys: i64,
    pub closing_sells: i64,
    pub open_orders: usize,
}
impl Exposure {
    pub fn net(self) -> i64 {
        self.long - self.short
    }
    pub fn gross(self) -> i64 {
        self.long + self.short
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionCounts {
    pub fills: u64,
    pub maker_fills: u64,
    pub completed_cycles: u64,
    pub maker_to_maker: u64,
    pub maker_to_taker: u64,
    pub taker_escapes: u64,
    pub orders: u64,
}
impl ExecutionCounts {
    pub fn difference(self, s: Self) -> Self {
        Self {
            fills: self.fills - s.fills,
            maker_fills: self.maker_fills - s.maker_fills,
            completed_cycles: self.completed_cycles - s.completed_cycles,
            maker_to_maker: self.maker_to_maker - s.maker_to_maker,
            maker_to_taker: self.maker_to_taker - s.maker_to_taker,
            taker_escapes: self.taker_escapes - s.taker_escapes,
            orders: self.orders - s.orders,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    pub id: u64,
    pub void_id: u64,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub forced: Option<KillReason>,
    pub baseline_accounts: Accounts,
    pub baseline_unrealized: Money,
    pub baseline_counts: ExecutionCounts,
    pub peak_long: i64,
    pub peak_short: i64,
    pub peak_gross: i64,
    pub peak_net_deviation: i64,
    pub peak_net: i64,
    pub peak_liability: Money,
    pub first_recovery_at: Option<Timestamp>,
    pub experienced_deficit: bool,
    pub accounts: Accounts,
    pub residual_mtm: Money,
    pub net_pnl: Money,
    pub counts: ExecutionCounts,
}
impl Episode {
    pub fn recovery_yield_ppm(self) -> Result<Option<i128>, InventoryError> {
        if self.peak_liability.0 == 0 {
            return Ok(None);
        }
        Ok(Some(
            self.accounts
                .harvest()?
                .0
                .checked_mul(1_000_000)
                .ok_or(ArithmeticError::Overflow)?
                / self.peak_liability.0,
        ))
    }
}
