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

// Returned when wire format tag bytes doesn't map to a known variant
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTag(pub u8);

use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Clone, PartialEq)]
pub struct PodOrderEvent {
    tag: u8,
    _pad: [u8; 7],
    payload: [u8; 40]
}