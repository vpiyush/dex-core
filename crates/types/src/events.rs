use crate::Order;
use crate::RejectReason;

#[derive(Clone, Debug, PartialEq)]
pub enum OrderEvent {
    New(Order),
    Fill {
        id: u64,
        fill_qty: u64,
        fill_price: u64
    },
    PartialFill {
        id: u64,
        fill_qty: u64,
        fill_price: u64,
        remaining_qty: u64
    },
    Cancel {
        id: u64,
    },
    Reject {
        id: u64,
        reason: RejectReason
    }
}