use types::{OrderEvent, OrderRequest, RejectReason};

use crate::EventSink;

/// Push a `Reject` event onto the emit buffer.
///
/// `remaining_qty` policy:
///   - New-path rejects (whole request invalid pre-mint): pass `req.quantity`.
///   - Cancel-path rejects (no fill semantics): pass `0`.
///   - Market-order trailing reject (partial fill then liquidity exhausted):
///     pass the unfilled `remaining` quantity.
pub(crate) fn push_reject(
    reason: RejectReason,
    remaining_qty: u64,
    req: &OrderRequest,
    out: &mut impl EventSink,
) {
    let ev = OrderEvent::Reject {
        id: 0,
        reason,
        remaining_qty,
        origin_ts: req.origin_ts,
        intent_hash: req.intent_hash,
    };
    out.emit(ev)
}

pub(crate) fn push_cancel(id: u64, req: &OrderRequest, out: &mut impl EventSink) {
    let ev = OrderEvent::Cancel {
        id,
        origin_ts: req.origin_ts,
        intent_hash: req.intent_hash,
    };
    out.emit(ev)
}

