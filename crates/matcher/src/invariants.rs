use crate::Engine;

pub(crate) fn assert_invariants(_engine: &Engine) {
    // todo: cross-book invariants (e.g. next_order_id monotonicity across emits)
    // currently empty — per-book invariants are checked inside OrderBook itself.
}
