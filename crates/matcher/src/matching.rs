
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
//!
use orderbook::{OrderBook, PriceLevel};
use types::{Order, OrderEvent, OrderId, OrderRequest, Side};

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
///
/// # Complexity
/// O(N Log L) where N = resting order's consumed, L = price levels on the
/// opposite side. Each consumed order costs 2 BtreeMap accesses (one `peek_top`
/// and one `pop_top`/`reduce_top`. Event emission is O(N) into out. arena operations
/// are O(1) per order.
///
/// # known follow-up(improvement)
/// if the remaining >= level.total_quantity, the whole level can be consumed at once
/// reducing complexity to O(L Log L), try after benchmarks
///
pub fn match_limit(book: &mut OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    let opposite = opposite(req.side);
    let mut remaining = req.quantity;
    while remaining > 0 {
        let (maker_price, maker_qty, maker_id, maker_intent)  = {
            // opposite side is empty
            let Some(top) = book.peek_top(opposite) else {
                break
            };
            // no crossing
            if !crossing(req.side, remaining, top.price) {
                break
            }
            // side is consumable
            (top.price, top.order.quantity, top.order.order_id.0, top.order.intent_hash)
        };

        let fill_qty = remaining.min(maker_qty);
        let fill_price = maker_price;
        let maker_remaining = maker_qty - fill_qty;
        remaining -= fill_qty;

        // emit the fill events
        if remaining == 0 {
            out.push( OrderEvent::Fill {
                id: order_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts,
                intent_hash: req.intent_hash,
            })
        } else {
            out.push( OrderEvent::PartialFill {
                id: order_id,
                fill_qty,
                fill_price,
                remaining_qty: remaining,
                origin_ts: req.origin_ts,
                intent_hash: req.intent_hash,
            })
        }
        // emit maker_fill events also mutate book
        if maker_remaining == 0 {
            out.push(OrderEvent::Fill {
                id: maker_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts, // using the request ts since it caused the event to be fired
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

    // check if there is still residual
    if remaining > 0 {
        let resting = Order{
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
        book.insert(resting).expect("insert error handling deferred");
        out.push(OrderEvent::New(resting));
    }
}

pub fn match_market(book: &mut OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    todo!("market matching: cross opposide side until exhausted or empty")
}

fn opposite(side: Side)-> Side {
    match side {
        Side::Ask => Side::Bid,
        Side::Bid => Side::Ask
    }
}

fn crossing(taker_side: Side, taker_price: u64, maker_price: u64 ) -> bool{
    match taker_side {
        Side::Bid => taker_price >= maker_price,
        Side::Ask => taker_price <= maker_price
    }
}