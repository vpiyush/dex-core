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
//! The matcher is limit-only. Every order carries a price and a
//! [`TimeInForce`] ({GTC, IOC, FOK}); aggressive/market-style fills are
//! expressed upstream as a marketable limit price + IOC at the gateway, not as
//! a separate order type. [`match_limit`] drives the shared `cross_loop` and
//! then applies TIF-specific residual handling: GTC rests the remainder, IOC
//! rejects it, and FOK is gated up front so it either fully fills or never
//! touches the book.
//!
use crate::{EventSink, events::push_reject};
use orderbook::{InsertError, OrderBook};
use types::{Order, OrderEvent, OrderId, OrderRequest, RejectReason, Side, TimeInForce};

/// Match a limit order against the opposite side, then apply its time-in-force residual policy.
///
/// # Algorithm - per-order cross loop
///
/// 1. peek the opposing top.
/// 2. If it crosses the taker's price. fill `min(remaining, maker.qty)`
///    at the maker's price.
/// 3. Emit the fill event for each side (`Fill` if that side is fully
///    consumed or `PartialFill` otherwise.
/// 4. `pop_top` if the maker is fully consumed, else `reduce_top`
/// 5. Repeat from 1. until the taker is fully exhausted or no more orders
///    to cross.
/// 6. Apply the time-in-force residual policy to any unfilled quantity:
///    - GTC: rest on the book, emit `New` (insert failures surface as a
///      Reject for the unfilled portion).
///    - IOC: reject the unfilled remainder with `InsufficientLiquidity`.
///    - FOK: gated before step 1 — full fill is pre-verified, so no residual
///      can survive (reject up front if the book can't fully fill).
///
/// # Complexity
/// O(N Log L) where N = resting order's consumed, L = price levels on the
/// opposite side. Each consumed order costs 2 BtreeMap accesses (one `peek_top`
/// and one `pop_top`/`reduce_top`. Event emission is O(N) into out. arena operations
/// are O(1) per order.
///
/// # known follow-up(improvement)
/// 1. if the remaining >= level.total_quantity, the whole level can be consumed at once
///    reducing complexity to O(L Log L), try after benchmarks
/// 2. pushing to out vector is not ideal from performance point of view, there are two
///    possible alternatives.
///    - direct use the seqlock ipc here, but increase coupling with different crate.
///    - use an event sink, can caller can choose where it lands. - resolved
///
pub(crate) fn match_limit(
    book: &mut OrderBook,
    req: &OrderRequest,
    order_id: u64,
    out: &mut impl EventSink,
) {
    // FOK (Fill-or-Kill) is all-or-nothing: prove the full quantity is fillable
    // BEFORE we touch the book, so a failure emits zero fills instead of
    // partials we'd have to unwind. Nothing has filled yet, so the rejected
    // remainder is the whole req.quantity.
    if req.tif == TimeInForce::FOK && !has_sufficient_liquidity(book, req) {
        push_reject(RejectReason::InsufficientLiquidity, req.quantity, req, out);
        return;
    }

    let remaining = cross_loop(book, req, order_id, out);
    if remaining == 0 {
        return;
    }

    match req.tif {
        // pre-check done, everything should have been consumed
        TimeInForce::FOK => {
            debug_assert!(
                false,
                "FOK left a residual, must never happen, pre-check must have prevented this"
            )
        }
        // take what crossed and reject the unfilled remainder, never rest
        TimeInForce::IOC => {
            push_reject(RejectReason::InsufficientLiquidity, remaining, req, out);
        }
        // rest the unfilled portion, Insert failure rejects only thar portion,
        // fills already done are real trades we don't roll back
        TimeInForce::GTC => {
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
                Ok(_) => out.emit(OrderEvent::New(resting)),
                Err(InsertError::ArenaFull) => {
                    push_reject(RejectReason::SystemAtCapacity, remaining, req, out);
                }
                Err(InsertError::DuplicateIntent) => {
                    push_reject(RejectReason::DuplicateIntent, remaining, req, out);
                }
            }
        }
    }
}

/// FOK gate: can the opposing side fully fill `req.quantity` at a crossing price?
///
/// Walks opposing *levels* (not orders) in priority order, summing
/// `level.total_qty` until it reaches the requirement or the price stops
/// crossing — O(L) with early exit, strictly cheaper than the fill it guards.
/// FOK is all-or-nothing, so this must run before `cross_loop` emits anything:
/// a failure here means zero book mutation and zero events to unwind. This is
/// why FOK reads the book twice (level-granular verify, then order-granular
/// fill) — the passes differ in granularity and the verify is required for
/// atomicity, not redundant work.
fn has_sufficient_liquidity(book: &OrderBook, req: &OrderRequest) -> bool {
    match req.side {
        Side::Bid => liquidity_reaches(
            book.ask_depth(usize::MAX),
            Side::Bid,
            req.price,
            req.quantity,
        ),
        Side::Ask => liquidity_reaches(
            book.bid_depth(usize::MAX),
            Side::Ask,
            req.price,
            req.quantity,
        ),
    }
}

#[inline]
fn liquidity_reaches(
    levels: impl Iterator<Item = (u64, u64)>,
    taker_side: Side,
    taker_price: u64,
    required: u64,
) -> bool {
    let mut acc: u64 = 0;
    for (price, qty) in levels {
        if !crossing(taker_side, taker_price, price) {
            break;
        }
        acc += qty;
        if acc >= required {
            return true;
        }
    }
    false
}

/// Shared per-order cross loop driven by [`match_limit`].
///
/// Walks the opposing side of the book one resting order at a time, filling
/// `min(remaining, maker.qty)` at the maker's price. Emits one fill event
/// per party per trade (taker + maker) and mutates the book (`pop_top` for
/// full consumption, `reduce_top` for partial). Continues until the taker
/// is exhausted, the opposing side is empty, or `req.price` no longer crosses
/// the opposing top.
///
/// Returns the unfilled remaining quantity. The caller applies the
/// time-in-force residual policy (GTC rests, IOC rejects the remainder).
fn cross_loop(
    book: &mut OrderBook,
    req: &OrderRequest,
    order_id: u64,
    out: &mut impl EventSink,
) -> u64 {
    let opposite = opposite(req.side);
    let mut remaining = req.quantity;

    while remaining > 0 {
        let (maker_price, maker_qty, maker_id, maker_intent) = {
            let Some(top) = book.peek_top(opposite) else {
                break;
            };
            if !crossing(req.side, req.price, top.price) {
                break;
            }
            (
                top.price,
                top.order.quantity,
                top.order.order_id.0,
                top.order.intent_hash,
            )
        };

        let fill_qty = remaining.min(maker_qty);
        let fill_price = maker_price;
        let maker_remaining = maker_qty - fill_qty;
        remaining -= fill_qty;

        // Taker fill event
        if remaining == 0 {
            out.emit(OrderEvent::Fill {
                id: order_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts,
                intent_hash: req.intent_hash,
            });
        } else {
            out.emit(OrderEvent::PartialFill {
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
            out.emit(OrderEvent::Fill {
                id: maker_id,
                fill_qty,
                fill_price,
                origin_ts: req.origin_ts,
                intent_hash: maker_intent,
            });
            book.pop_top(opposite);
        } else {
            out.emit(OrderEvent::PartialFill {
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
