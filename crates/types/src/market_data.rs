use bytemuck::{CheckedBitPattern, NoUninit};
use crate::Side;

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct L2Update {
    price: u64,
    quantity: u64,
    timestamp: u64,
    instrument_id: u32,
    side: Side,
    _padding: [u8; 3]
}
