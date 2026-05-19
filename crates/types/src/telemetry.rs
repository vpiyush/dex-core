use bytemuck::{NoUninit, CheckedBitPattern};
#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Copy, Clone, PartialEq,  Debug)]
pub struct TimingDelta {
    origin_ts: u64,
    current_ts: u64,
    publisher_id: u32,
    msg_type: u8,
    /// Low 8 bits of TSC_AUX register from rdtscp; identifies the publishing
    /// core for taint detection in the aggregator. 0xFF = unknown/uncaptured.
    /// See docs/lld/telemetry.md §5.1.
    core_id: u8,
    _padding: [u8; 2]
}
