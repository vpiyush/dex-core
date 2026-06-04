
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LapPolicy {
    Halt, // consumer stops permanently - will be used by persist consumer
    Panic, // panic immediately on lap - dev/tests
    SkipAndAlert, // Skip to the producers' current position, emit a telemetry event
    SkipToLatest // Skip to the producer's current position, silently
}

#[derive(Debug, PartialEq, Eq)]
pub enum PollResult<T> {
    Ready(T), // next value is available
    Empty, // no new data yet
    Halted {last_safe_seq: u64, slots_last: u64}, // lapped under halt, cursor frozen at `last_safe_seq`
    Skipped {slots_last: u64, new_seq: u64 } // lapped under skip, cursor advanced to new_seq
}