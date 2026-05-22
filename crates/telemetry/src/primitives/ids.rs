#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PublisherId(pub u32);

impl PublisherId {
    pub const UNKNOWN: Self = Self(0);
    pub const BENCHMARK: Self = Self(999);
}

#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MsgType {
    Unknown = 0,
    Benchmark = 1,
}

#[repr(u16)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CounterId {
    Unknown = 0,
    TelemetryDroppedCum = 5,
}

#[repr(u16)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EventType {
    Unknown = 0,
    CorePinningViolation = 3,
}
