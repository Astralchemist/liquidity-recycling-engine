//! Gateway boundary only. No authenticated or live order routing implementation.
use common::Side;
use fixed_point::{PriceTicks, QtyUnits};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderIntent {
    pub client_id: u64,
    pub side: Side,
    pub price: PriceTicks,
    pub qty: QtyUnits,
    pub post_only: bool,
}
pub trait OrderGateway {
    type Error;
    fn submit(&mut self, order: OrderIntent) -> Result<(), Self::Error>;
    fn cancel(&mut self, client_id: u64) -> Result<(), Self::Error>;
}
