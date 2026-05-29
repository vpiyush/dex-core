//! implements price time matching algorithm against an [`orderbook`]:
//! incoming taker order cross the opposite matching side until the taker is
//! exhausted or the opposite side is empty, OR no more resting orders satisfy the
//! cross condition.
//!
//! Trade price follows the resting (maker's) price. price-time priority means
//! the order that arrived first at a given price gets the cross at that price
//! and the taker pays/receives that price (price improvement for the taker
//! for aggressive crosses).
//!
//! The per-order cross loop is shared between limit and market orders via the
//! private `cross_loop` function. `match_limit` and `match_market` differ only
//! in two ways: the cross-condition check (limit gates by price; market always
//! crosses) and the residual handling (limit rests; market rejects).
//!
use orderbook::{InsertError, OrderBook};
use types::{Order, OrderEvent, OrderId, OrderRequest, RejectReason, Side};
use crate::events::push_reject;

/// match a limit order against the opposite side, then rest any residual.
///
/// # Algorithm - per-order cross loop
///
/// 1. peek the opposing top.
/// 2. If it crosses the taker's price. fill `min(remaining, maker.qty)`
///     at the maker's price.
/// 3. Emit the fill event for each side (`Fill` if that side is fully
///     consumed or `PartialFill` otherwise.
/// 4. `pop_top` if the maker is fully consumed, else `reduce_top`
/// 5. Repeat from 1. until the taker is fully exhausted or no more orders
///     to cross.
/// 6. Residual quantity rests on the order book. emit `New`
///    (Insert failures surface as a Reject for the unfilled portion.)
///
/// # Complexity
/// O(N Log L) where N = resting order's consumed, L = price levels on the
/// opposite side. Each consumed order costs 2 BtreeMap accesses (one `peek_top`
/// and one `pop_top`/`reduce_top`. Event emission is O(N) into out. arena operations
/// are O(1) per order.
///
/// # known follow-up(improvement)
/// 1. if the remaining >= level.total_quantity, the whole level can be consumed at once
///     reducing complexity to O(L Log L), try after benchmarks
/// 2. pushing to out vector is not ideal from performance point of view, there are two
///     possible alternatives.
///     - direct use the seqlock ipc here, but increase coupling with different crate.
///     - use an event sink, can caller can choose where it lands.
///
pub(crate) fn match_limit(book: &mut OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    // TODO(TIF): apply TimeInForce semantics around the cross loop.
    //   - GTC (current behavior): residual rests on the book.
    //   - IOC: drop the residual (do not insert); emit no New event.
    //   - FOK: atomicity gate — verify req.quantity is fully fillable against
    //     the opposing side BEFORE entering cross_loop. If not, reject the
    //     whole request with InsufficientLiquidity (zero fills emitted).
    //   - GTD: same as GTC for v1; expiry mechanism is deferred.
    let remaining = cross_loop(book, req, order_id, Some(req.price), out);

    // Residual: GTC rests on the book. Insert failures surface as a Reject
    // for the unfilled portion — fills already emitted in cross_loop are real
    // trades on the book and are not rolled back.
    if remaining > 0 {
        let resting = Order {
            order_id: OrderId(order_id),
            price: req.price,
            quantity: remaining,
            origin_ts: req.origin_ts,
            instrument_id: req.instrument_id,
            side: req.side,
            order_type: req.order_type,
            tif: req.tif,
            _padding: 0,
            intent_hash: req.intent_hash,
        };
        match book.insert(resting) {
            Ok(_) => out.push(OrderEvent::New(resting)),
            Err(InsertError::ArenaFull) => {
                push_reject(RejectReason::SystemAtCapacity, remaining, req, out);
            }
            Err(InsertError::DuplicateIntent) => {
                push_reject(RejectReason::DuplicateIntent, remaining, req, out);
            }
        }
    }
}

/// match a market order against the opposing side until exhausted or no liquidity remains
///
/// # Algorithm — per-order cross loop
///
/// Identical to `match_limit` except:
///   - No cross-condition check (market crosses any price).
///   - No residual resting: if quantity remains after the opposing side
///     is empty, emit `Reject{InsufficientLiquidity}`.
///
/// 1. Peek the opposing top. If `None` → break.
/// 2. Fill `min(remaining, maker.qty)` at the maker's price.
/// 3. Emit Fill/PartialFill for each side per the same rule as `match_limit`.
/// 4. `pop_top` if maker fully consumed, else `reduce_top`.
/// 5. Repeat until taker exhausted or opposing side empty.
/// 6. If residual remains → emit `Reject{InsufficientLiquidity}`.
///
/// # Complexity
///
/// Same as `match_limit`: O(N log L) where N = resting orders consumed,
/// L = levels on opposing side.
///
pub(crate) fn match_market(book: &mut OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    let remaining = cross_loop(book, req, order_id, None, out);

    // Market orders don't rest; unfilled qty is rejected.
    if remaining > 0 {
        push_reject(RejectReason::InsufficientLiquidity, remaining, req, out);
    }
}

/// Shared per-order cross loop, used by both `match_limit` and `match_market`.
///
/// Walks the opposing side of the book one resting order at a time, filling
/// `min(remaining, maker.qty)` at the maker's price. Emits one fill event
/// per party per trade (taker + maker) and mutates the book (`pop_top` for
/// full consumption, `reduce_top` for partial). Continues until the taker
/// is exhausted, the opposing side is empty, or the cross condition fails.
///
/// `limit_price`:
///   - `Some(p)`: limit caller; loop stops when the opposing top no longer
///     satisfies `crossing(req.side, p, maker_price)`.
///   - `None`: market caller; loop ignores price and crosses whatever exists.
///
/// Returns the unfilled remaining quantity. Callers handle the residual
/// per their own policy (rest for limit GTC, reject for market).
fn cross_loop(
    book: &mut OrderBook,
    req: &OrderRequest,
    order_id: u64,
    limit_price: Option<u64>,
    out: &mut Vec<OrderEvent>,
) -> u64 {
    let opposite = opposite(req.side);
    let mut remaining = req.quantity;

    while remaining > 0 {
        let (maker_price, maker_qty, maker_id, maker_intent) = {
            let Some(top) = book.peek_top(opposite) else { break };
            if let Some(taker_price) = limit_price {
                if !crossing(req.side, taker_price, top.price) { break }
            }
            (top.price, top.order.quantity, top.order.order_id.0, top.order.intent_hash)
        };

        let fill_qty = remaining.min(maker_qty);
        let fill_price = maker_price;
        let maker_remaining = maker_qty - fill_qty;
        remaining -= fill_qty;

        // Taker fill event
        if remaining == 0 {
            out.push(OrderEvent::Fill {
                id: order_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts,
                intent_hash: req.intent_hash,
            });
        } else {
            out.push(OrderEvent::PartialFill {
                id: order_id,
                fill_qty,
                fill_price,
                remaining_qty: remaining,
                origin_ts: req.origin_ts,
                intent_hash: req.intent_hash,
            });
        }

        // Maker fill event + book mutation
        if maker_remaining == 0 {
            out.push(OrderEvent::Fill {
                id: maker_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts,
                intent_hash: maker_intent,
            });
            book.pop_top(opposite);
        } else {
            out.push(OrderEvent::PartialFill {
                id: maker_id,
                fill_qty,
                fill_price,
                remaining_qty: maker_remaining,
                origin_ts: req.origin_ts,
                intent_hash: maker_intent,
            });
            book.reduce_top(opposite, fill_qty);
        }
    }

    remaining
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Ask => Side::Bid,
        Side::Bid => Side::Ask,
    }
}

fn crossing(taker_side: Side, taker_price: u64, maker_price: u64) -> bool {
    match taker_side {
        Side::Bid => taker_price >= maker_price,
        Side::Ask => taker_price <= maker_price,
    }
}
