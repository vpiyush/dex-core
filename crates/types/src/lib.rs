#[repr(u8)]
enum Side {
    Bid,
    Ask
}

#[repr(u8)]
enum  OrderType {
    Limit,
    Market
}

#[repr(u8)]
enum TimeInForce {
    GTC,
    IOC,
    FOK
}


struct Order {

}

struct L2Update {

}

struct OrderEvent {

}
