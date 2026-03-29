use bytemuck::{NoUninit, CheckedBitPattern};

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum Side {
    Bid,
    Ask
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum  OrderType {
    Limit,
    Market
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum TimeInForce {
    GTC,
    IOC,
    FOK
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum RequestType {
    New,
    Cancel,
    Amend
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum RejectReason {
    InsufficientBalance, // spot balance too low
    InsufficientMargin, // margin requirement not met
    PositionLimitExceeded, // position size limit reached
    RateLimitExceeded, // order rate too high
    InvalidPrice, // price out of valid range
    InvalidQuantity, // quantity zero or invalid
    UnknownInstrument // unknown trading pair
}


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

