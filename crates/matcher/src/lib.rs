//! multi-instrument CLOB matching engine
//!
//! Owns one orderbook [`OrderBook`](orderbook::OrderBook) per registered instrument,
//! Mints server side orderIDs and emits an [`OrderEvent`](types::OrderEvent)
//! stream to a caller supplied buffer
//!
//! See `docs/lld/matcher.md` for the design.

mod engine;
mod matching;
mod events;
#[cfg(debug_assertions)]
mod invariants;

pub use engine::*;