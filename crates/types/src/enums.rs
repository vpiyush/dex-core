use bytemuck::{NoUninit, CheckedBitPattern};

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum Side {
    Bid = 0,
    Ask = 1
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum  OrderType {
    Limit = 0,
    Market = 1
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum TimeInForce {
    GTC = 0,
    IOC = 1,
    FOK = 2
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum RequestType {
    New = 0,
    Cancel = 1,
    Amend = 2
}

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq, Eq, Debug)]
pub enum RejectReason {
    InsufficientBalance = 0, // spot balance too low
    InsufficientMargin = 1, // margin requirement not met
    PositionLimitExceeded = 2, // position size limit reached
    RateLimitExceeded = 3, // order rate too high
    InvalidPrice = 4, // price out of valid range
    InvalidQuantity = 5, // quantity zero or invalid
    UnknownInstrument = 6, // unknown trading pair
    UnknownOrder = 7, // no order with this intent_hash exists
    SystemAtCapacity = 8, // system is overloaded
    InsufficientLiquidity = 9 // not enough liquidity to fullfill the order
}