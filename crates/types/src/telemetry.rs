use bytemuck::{NoUninit, CheckedBitPattern};
#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
struct TimingDelta {
    origin_ts: u64,
    current_ts: u64,
    publisher_id: u32,
    msg_type: u8,
    _padding: [u8; 3]
}
