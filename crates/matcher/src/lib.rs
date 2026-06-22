//! multi-instrument CLOB matching engine
//!
//! Owns one orderbook [`OrderBook`](orderbook::OrderBook) per registered instrument,
//! Mints server side orderIDs and emits an [`OrderEvent`](types::OrderEvent)
//! stream to a caller-supplied sink ([`EventSink`]).
//!
//! See `docs/lld/matcher.md` for the design.

mod engine;
mod events;
#[cfg(debug_assertions)]
mod invariants;
mod matching;
mod sink;

pub use engine::*;
pub use sink::EventSink;

