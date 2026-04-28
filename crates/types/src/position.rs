
#[derive(Clone, Debug, PartialEq)]
pub enum Position {
    Flat,
    Long {
        qty: u64,
        avg_price: u64
    },
    Short {
        qty: u64,
        avg_price: u64
    }
}

use bytemuck::{Pod, Zeroable};

// wire format tagged union for position
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct PodPosition {
    tag: u8,
    _pad: [u8; 7],
    qty: u64,
    avg_price: u64,
}