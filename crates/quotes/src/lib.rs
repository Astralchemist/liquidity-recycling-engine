//! Candidate action interface only; no strategy or automatic order generation.
use common::Side;
use fixed_point::{InventoryUnits, PriceTicks};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAction {
    Hold,
    Quote {
        side: Side,
        price: PriceTicks,
        units: InventoryUnits,
    },
    Cancel {
        order_id: u64,
    },
    Replace {
        order_id: u64,
        price: PriceTicks,
    },
}
