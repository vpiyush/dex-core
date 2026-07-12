//! Single-instrument continuous central limit order book (CLOB).
//!
//! [`OrderBook`] owns one [`Arena<OrderNode>`](arena::Arena) plus two side-keyed
//! `BTreeMap<Price, PriceLevel>` (bids descending-by-best, asks ascending) and
//! a `FxHashMap<IntentHash, ArenaIdx>` for O(1) cancel lookup. Each
//! [`PriceLevel`] is an intrusive doubly-linked FIFO threaded through the
//! arena: the level holds `head`/`tail` handles and each node carries
//! `prev`/`next` links, so an order located by intent_hash is spliced out in
//! O(1) with no scan. Orders live in the arena and never move.
//!
//! The book enforces price-time priority: the BTreeMap orders levels by price;
//! the per-level linked list preserves arrival order at each price. The
//! [`TopView`] returned by `peek_top` is the read-only handle the matcher
//! uses to inspect the resting head of either side before deciding to
//! `pop_top` (full consumption) or `reduce_top` (partial fill).
//!
//! Invariants (level total_qty agrees with the sum of arena order quantities,
//! every indexed `IntentHash` resolves to a live arena slot, no level is left
//! empty in the map) are checked in debug builds by
//! `invariants::assert_invariants` after every mutating operation.
//!
//! See `docs/lld/orderbook.md` for the full design.

mod book;
mod cursor;
mod level;
// Debug-only invariant checker: called under #[cfg(debug_assertions)] in book.rs
// and exercised by a #[cfg(test)] test. Gated so it isn't dead code in release.
#[cfg(any(debug_assertions, test))]
mod invariants;

pub use book::{InsertError, OrderBook, TopView};
pub use cursor::LevelCursor;
pub use level::PriceLevel;
