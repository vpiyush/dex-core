use crate::Engine;

/// Engine-level invariants (matcher LLD §3.5). Per-book structural invariants
/// (I1–I7) are enforced inside `OrderBook` on every mutation; this asserts the
/// one the book deliberately leaves to the matcher: M1, no book crossed at rest.
///
/// Called under `#[cfg(debug_assertions)]` at the tail of `Engine::process`, so
/// every processed request re-checks it. O(instruments) in debug, zero in
/// release.
pub(crate) fn assert_invariants(engine: &Engine) {
    for (instrument_id, book) in engine.books.iter() {
        // M1 — the matcher fully crosses before resting, so no book may return
        // from process() with best_bid >= best_ask.
        assert!(
            !book.is_crossed(),
            "M1 violated: book {instrument_id} is crossed at rest"
        );
    }
}
