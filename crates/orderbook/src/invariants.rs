//! Runtime invariant checks for the OrderBook.
//!
//! Invariants are described in the orderbook LLD §3.3. They are:
//!
//!   I1: every node reachable from a level's head is a valid arena slot
//!   I2: PriceLevel::total_qty == sum of node quantities over the level,
//!       and PriceLevel::len == number of nodes walked
//!   I3: no PriceLevel in either BTreeMap is empty
//!   I4: index keys exactly match the intent_hashes of all resting orders
//!   I5: index[hash] points to the arena slot whose Order.intent_hash == hash
//!   I7: each level's intrusive list is well-formed — head.prev and tail.next
//!       are SENTINEL, the forward walk of `len` steps lands exactly on tail,
//!       and every node's back-link points at its predecessor
//!
//! `assert_invariants` is called via `#[cfg(debug_assertions)]` at the tail
//! of every mutating method on OrderBook. Cost is O(total resting orders)
//! in debug builds and zero in release builds.

use crate::book::OrderBook;
use crate::level::PriceLevel;
use arena::ArenaIdx;

/// Walks a level head→next, asserting I1 (arena validity) and I7 (list shape)
/// as it goes, and returns the node indices in FIFO order. The walk is bounded
/// by `level.len` steps, so a corrupt (cyclic) list fails loudly on the length
/// check instead of looping forever.
fn walk_level(book: &OrderBook, price: u64, level: &PriceLevel) -> Vec<ArenaIdx> {
    let mut idxs = Vec::with_capacity(level.len());

    // I7 — an empty level has both ends SENTINEL and len 0.
    if !level.head.is_valid() {
        assert!(
            !level.tail.is_valid() && level.len == 0,
            "I7 violated: level @ {price} has no head but tail/len are set"
        );
        return idxs;
    }

    // Forward walk, bounded by len; verify each back-link inline (I7).
    let mut cur = level.head;
    let mut prev = ArenaIdx::SENTINEL;
    for _ in 0..level.len {
        let node = book.arena.get(cur).unwrap_or_else(|| {
            panic!("I1 violated: ArenaIdx {cur:?} in level @ {price} not valid in arena")
        });
        assert_eq!(
            node.prev, prev,
            "I7 violated: back-link mismatch at {cur:?} in level @ {price}"
        );
        idxs.push(cur);
        prev = cur;
        cur = node.next;
    }

    // I7 — exactly `len` nodes, the list terminates, and it ends at tail.
    assert!(
        !cur.is_valid(),
        "I7 violated: level @ {price} has more nodes than len={}",
        level.len
    );
    assert_eq!(
        prev, level.tail,
        "I7 violated: forward walk of level @ {price} ended at {prev:?}, not tail {:?}",
        level.tail
    );

    idxs
}

/// Walks the book and asserts I1..=I7 hold. Panics on violation with a
/// diagnostic naming the invariant number.
pub(crate) fn assert_invariants(book: &OrderBook) {
    let mut total_orders_in_levels = 0usize;

    for (&price, level) in book.bids.iter().chain(book.asks.iter()) {
        // I3 — no empty levels anywhere.
        assert!(
            !level.is_empty(),
            "I3 violated: empty level present at price {price}"
        );

        // I1 + I7 — walk the intrusive list, collecting node indices in order.
        let idxs = walk_level(book, price, level);

        // I2 (len) — the walk visited exactly len nodes.
        assert_eq!(
            idxs.len(),
            level.len(),
            "I2 violated at price {price}: walked {} nodes but len={}",
            idxs.len(),
            level.len()
        );

        // I2 (qty) — total_qty matches the sum across the level.
        let mut sum_qty = 0u64;
        for &idx in &idxs {
            let node = book.arena.get(idx).expect("I1 already checked in walk");
            sum_qty += node.order.quantity;
        }
        assert_eq!(
            sum_qty, level.total_qty,
            "I2 violated at price {price}: computed sum {sum_qty} vs stored total_qty {}",
            level.total_qty
        );

        // I4 (membership) + I5 — every node has an index entry pointing back.
        for &idx in &idxs {
            let node = book.arena.get(idx).expect("I1 already checked in walk");
            let indexed_idx = book.index.get(&node.order.intent_hash).unwrap_or_else(|| {
                panic!(
                    "I4 violated: order at price {price} with intent_hash {:?} not in index",
                    node.order.intent_hash
                )
            });
            assert_eq!(
                *indexed_idx, idx,
                "I5 violated: index entry for {:?} points to {:?} but order lives in {:?}",
                node.order.intent_hash, indexed_idx, idx
            );
        }

        total_orders_in_levels += idxs.len();
    }

    // I4 (count) — index size == total orders held across all levels.
    assert_eq!(
        book.index.len(),
        total_orders_in_levels,
        "I4 violated: index has {} entries but levels hold {} orders total",
        book.index.len(),
        total_orders_in_levels
    );
}
