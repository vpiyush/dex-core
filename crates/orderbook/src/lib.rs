//! Single-instrument continuous central limit order book (CLOB).
//!
//! [`OrderBook`] owns one [`Arena<Order>`](arena::Arena) plus two side-keyed
//! `BTreeMap<Price, PriceLevel>` (bids descending-by-best, asks ascending) and
//! a `FxHashMap<IntentHash, ArenaIdx>` for O(1) cancel lookup. Each
//! [`PriceLevel`] is a FIFO `VecDeque<ArenaIdx>` over arena handles —
//! orders themselves live in the arena and never move.
//!
//! The book enforces price-time priority: the BTreeMap orders levels by price;
//! the per-level VecDeque preserves arrival order at each price. The
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
mod level;
mod invariants;

pub use book::{OrderBook, InsertError, TopView};
pub use level::PriceLevel;