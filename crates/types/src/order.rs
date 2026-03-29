use bytemuck::{NoUninit, CheckedBitPattern};
use crate::Side;
use crate::OrderType;
use crate::TimeInForce;
use crate::RequestType;

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct Order {
    id: u64,
    price: u64,
    quantity: u64,
    timestamp: u64,
    instrument_id: u32,
    side: Side,
    order_type: OrderType,
    tif: TimeInForce,
    _padding: u8
}

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct OrderRequest {
    id: u64,
    price: u64,
    quantity: u64,
    timestamp: u64,
    instrument_id: u32,
    side: Side,
    order_type: OrderType,
    tif: TimeInForce,
    request_type: RequestType,
}
