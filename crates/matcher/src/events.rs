use types::{ OrderEvent, OrderRequest, RejectReason};

pub(crate) fn push_reject(reason:RejectReason, req: &OrderRequest,  out: &mut Vec<OrderEvent>) {
    let ev = OrderEvent::Reject {
        id: 0,
        reason,
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