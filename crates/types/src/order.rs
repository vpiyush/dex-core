use bytemuck::{NoUninit, CheckedBitPattern};
use crate::{IntentHash, OrderId, Side};
use crate::OrderType;
use crate::TimeInForce;
use crate::RequestType;

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct Order {
    pub order_id: OrderId,
    pub price: u64,
    pub quantity: u64,
    pub origin_ts: u64,
    pub instrument_id: u32,
    pub side: Side,
    pub order_type: OrderType,
    pub tif: TimeInForce,
    pub  _padding: u8,
    pub intent_hash: IntentHash // 32B - last to keep the alignment 8-byte
}

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct OrderRequest {
    pub price: u64,
    pub quantity: u64,
    pub origin_ts: u64,
    pub instrument_id: u32,
    pub side: Side,
    pub order_type: OrderType,
    pub tif: TimeInForce,
    pub request_type: RequestType,
    pub intent_hash: IntentHash
}
