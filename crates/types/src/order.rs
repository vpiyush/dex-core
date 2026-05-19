use bytemuck::{NoUninit, CheckedBitPattern};
use crate::Side;
use crate::OrderType;
use crate::TimeInForce;
use crate::RequestType;

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct Order {
    pub id: u64,
    pub price: u64,
    pub quantity: u64,
    pub origin_ts: u64,
    pub instrument_id: u32,
    pub side: Side,
    pub order_type: OrderType,
    pub tif: TimeInForce,
    pub  _padding: u8
}

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct OrderRequest {
    id: u64,
    price: u64,
    quantity: u64,
    origin_ts: u64,
    instrument_id: u32,
    side: Side,
    order_type: OrderType,
    tif: TimeInForce,
    request_type: RequestType,
}
