use types::{ OrderEvent, OrderRequest, RejectReason};

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
    out: &mut Vec<OrderEvent>,
) {
    let ev = OrderEvent::Reject {
        id: 0,
        reason,
        remaining_qty,
        origin_ts: req.origin_ts,
        intent_hash: req.intent_hash,
    };
    out.push(ev)
}

pub(crate) fn push_cancel(id:u64, req: &OrderRequest, out: &mut Vec<OrderEvent>) {
    let ev = OrderEvent::Cancel {
        id,
        origin_ts: req.origin_ts,
        intent_hash: req.intent_hash,
    };
    out.push(ev)
}