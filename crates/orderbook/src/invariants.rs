//! Runtime invariant checks for the OrderBook.
//!
//! Invariants are described in the orderbook LLD §3.3. They are:
//!
//!   I1: every ArenaIdx stored in any PriceLevel::orders is valid in arena
//!   I2: PriceLevel::total_qty == sum(arena[idx].quantity for idx in orders)
//!   I3: no PriceLevel in either BTreeMap is empty
//!   I4: index keys exactly match the intent_hashes of all resting orders
//!   I5: index[hash] points to the arena slot whose Order.intent_hash == hash
//!
//! `assert_invariants` is called via `#[cfg(debug_assertions)]` at the tail
//! of every mutating method on OrderBook. Cost is O(total resting orders)
//! in debug builds and zero in release builds.

use crate::book::OrderBook;

/// Walks the book and asserts I1..=I5 hold. Panics on violation with a
/// diagnostic naming the invariant number.
pub(crate) fn assert_invariants(book: &OrderBook) {
    // I3 — no empty levels anywhere.
    for (price, level) in book.bids.iter().chain(book.asks.iter()) {
        assert!(
            !level.orders.is_empty(),
            "I3 violated: empty level present at price {price}"
        );
    }

    // I1 + I2 — every ArenaIdx valid; total_qty matches sum across the level.
    let mut total_orders_in_levels = 0usize;
    for (price, level) in book.bids.iter().chain(book.asks.iter()) {
        let mut sum_qty = 0u64;
        for &idx in level.orders.iter() {
            let order = book.arena.get(idx).unwrap_or_else(|| {
                panic!(
                    "I1 violated: ArenaIdx {idx:?} in level @ {price} is not valid in arena"
                )
            });
            sum_qty += order.quantity;
        }
        assert_eq!(
            sum_qty, level.total_qty,
            "I2 violated at price {price}: computed sum {sum_qty} vs stored total_qty {}",
            level.total_qty
        );
        total_orders_in_levels += level.orders.len();
    }

    // I4 (count) — index size == total orders held across all levels.
    assert_eq!(
        book.index.len(),
        total_orders_in_levels,
        "I4 violated: index has {} entries but levels hold {} orders total",
        book.index.len(),
        total_orders_in_levels
    );

    // I4 (membership) + I5 — every level order has an index entry,
    // and the entry points back to the same arena slot.
    for (price, level) in book.bids.iter().chain(book.asks.iter()) {
        for &idx in level.orders.iter() {
            let order = book.arena.get(idx).expect("I1 already checked above");
            let indexed_idx = book.index.get(&order.intent_hash).unwrap_or_else(|| {
                panic!(
                    "I4 violated: order at price {price} with intent_hash {:?} not in index",
                    order.intent_hash
                )
            });
            assert_eq!(
                *indexed_idx, idx,
                "I5 violated: index entry for {:?} points to {:?} but order lives in {:?}",
                order.intent_hash, indexed_idx, idx
            );
        }
    }
}
