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
/// # Algorithm - level-cursor cross loop
///
/// 1. Open a `LevelCursor` on the opposing best level (one BTreeMap descent).
/// 2. If that level crosses the taker's price, fill its resting orders
///    head-first at `min(remaining, maker.qty)`, always at the maker's price.
///    Every order at a level shares one price, so the cross is checked once per
///    level, not once per order.
/// 3. Emit a fill event per party per trade (`Fill` when that side is fully
///    consumed, `PartialFill` otherwise).
/// 4. `pop_head` when the maker is fully consumed, `reduce_head` on the partial
///    that exhausts the taker.
/// 5. When a level drains, `finish` drops it and the loop re-descends to the
///    next-best level. Repeat until the taker is exhausted, the opposing side is
///    empty, or the best opposing level no longer crosses.
/// 6. Apply the time-in-force residual policy to any unfilled quantity:
///    - GTC: rest on the book, emit `New` (insert failures surface as a
///      Reject for the unfilled portion).
///    - IOC: reject the unfilled remainder with `InsufficientLiquidity`.
///    - FOK: gated before step 1 — full fill is pre-verified, so no residual
///      can survive (reject up front if the book can't fully fill).
///
/// # Complexity
/// O(N + L Log L) where N = resting orders consumed and L = opposing price
/// levels touched. One BTreeMap descent per level (via the cursor), not per
/// order; each consumed order is O(1) (arena free + link advance + aggregate
/// update). Event emission is O(N) into `out`.
///
/// # known follow-up(improvement)
/// 1. if remaining >= level.total_qty the whole level fills — the per-order
///    aggregate bookkeeping could collapse into a single bulk drain. Marginal
///    on top of the cursor; try after benchmarks.
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
    // Cursor mutations skip the book's per-op invariant check (a half-drained
    // level transiently sits empty in the map until finish); assert the settled
    // book here. No-op in release.
    book.debug_check_invariants();
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

/// Shared cross loop driven by [`match_limit`].
///
/// Two nested loops. The outer opens a `LevelCursor` on the opposing best level
/// (one BTreeMap descent) and checks the cross once for the whole level. The
/// inner walks that level's intrusive list, filling head orders in O(1) each
/// (`pop_head` on full consumption, `reduce_head` on the taker-exhausting
/// partial). When a level drains, `finish` drops it and the outer loop descends
/// to the next-best level. Continues until the taker is exhausted, the opposing
/// side is empty, or the best opposing level no longer crosses.
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

    // Outer loop: one cursor (one BTreeMap descent) per opposing level.
    while remaining > 0 {
        let Some(mut cursor) = book.best_level_cursor(opposite) else {
            break; // opposing side empty
        };
        // Every order at this level shares one price — cross once, not per order.
        if !crossing(req.side, req.price, cursor.price()) {
            break; // best level doesn't cross, so no worse level will either
        }
        let fill_price = cursor.price();

        // Inner loop: walk this level's intrusive list, O(1) per resting order.
        while remaining > 0 {
            let Some(maker) = cursor.head() else {
                break; // level drained
            };
            // Copy the maker's fields out before mutating through the cursor.
            let maker_qty = maker.quantity;
            let maker_id = maker.order_id.0;
            let maker_intent = maker.intent_hash;

            let fill_qty = remaining.min(maker_qty);
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
                if !cursor.pop_head() {
                    break; // level now empty
                }
            } else {
                out.emit(OrderEvent::PartialFill {
                    id: maker_id,
                    fill_qty,
                    fill_price,
                    remaining_qty: maker_remaining,
                    origin_ts: req.origin_ts,
                    intent_hash: maker_intent,
                });
                cursor.reduce_head(fill_qty);
                break; // maker_remaining > 0 means the taker is exhausted
            }
        }
        cursor.finish(); // drops the level from the book iff it drained
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
