use rustc_hash::FxHashMap;
use orderbook::OrderBook;
use types::{OrderEvent, OrderRequest, RejectReason, RequestType};
use crate::events::{push_cancel, push_reject};
#[cfg(debug_assertions)]
use crate::invariants::assert_invariants;
use crate::matching::{match_limit};

#[derive(Debug, PartialEq, Eq)]
pub enum AddInstrumentError {
    AlreadyRegistered,
    InvalidCapacity, // capacity must be in (0, u32::MAX)
}

pub struct Engine {
    books: FxHashMap<u32, orderbook::OrderBook>,
    next_order_id: u64, // monotonically increasing unique ID across all instruments
}

impl Engine {
    pub fn new() -> Self {
        Self {
            books: FxHashMap::default(),
            next_order_id: 0
        }
    }

    pub fn add_instrument(&mut self, instrument_id: u32, book_capacity: u32) -> Result<(), AddInstrumentError> {
        if book_capacity == 0 || book_capacity == u32::MAX {
            return Err(AddInstrumentError::InvalidCapacity);
        }
        if self.books.contains_key(&instrument_id) {
            return Err(AddInstrumentError::AlreadyRegistered);
        }

        self.books.insert(instrument_id, OrderBook::new(instrument_id, book_capacity));
        Ok(())
    }

    pub fn book(&self, instrument_id: u32) -> Option<&OrderBook> {
        self.books.get(&instrument_id)
    }

    fn mint_order_id(&mut self) -> u64 {
        let id = self.next_order_id;
        self.next_order_id += 1;
        id
    }

    pub fn process(&mut self, req: &OrderRequest, out: &mut Vec<OrderEvent>) {
        match req.request_type {
           RequestType::New => {
               self.process_new(req, out);
           }
            RequestType::Cancel => {
                self.process_cancel(req, out);
            }
            RequestType::Amend => {
                self.process_amend(req, out);
            }
        }
        #[cfg(debug_assertions)]
        assert_invariants(self);
    }

    fn process_new(&mut self, req: &OrderRequest, out: &mut Vec<OrderEvent>) {
        // instrument exists
        if !self.books.contains_key(&req.instrument_id) {
            push_reject(RejectReason::UnknownInstrument, req.quantity, req, out);
            return;
        }

        // quantity check
        if req.quantity == 0 {
            return push_reject(RejectReason::InvalidQuantity, req.quantity, req, out);
        };

        // every order is a limit order; price must be > 0
        if req.price == 0 {
            return push_reject(RejectReason::InvalidPrice, req.quantity, req, out);
        }

        // validation passed - burn an orderID
        let order_id = self.mint_order_id();
        let book = self.books .get_mut(&req.instrument_id)
            .expect("invariant: book existence checked at step 1");
        match_limit(book, req, order_id, out)
    }

    // process order cancel request
    fn process_cancel(&mut self, req: &OrderRequest, out: &mut Vec<OrderEvent>) {
        let Some(book) = self.books.get_mut(&req.instrument_id) else {
            // Cancel-path reject: no fill semantics, remaining_qty = 0.
            return push_reject(RejectReason::UnknownInstrument, 0, req, out);
        };

        match book.cancel(&req.intent_hash) {
            Some(order) => {
                push_cancel(order.order_id.0, req, out)
            }
            None => {
                push_reject(RejectReason::UnknownOrder, 0, req, out)
            }
        }
    }

    fn process_amend(&mut self, _req: &OrderRequest, _out: &mut Vec<OrderEvent>) {
        todo!("amend deferred to later")
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{IntentHash, Order, OrderType, Side, TimeInForce};

    const INSTR: u32 = 1;

    /// Build an OrderRequest. `hash_byte` keeps intent_hashes distinct so the
    /// book's dedup index doesn't reject our resting makers.
    fn req(
        side: Side,
        price: u64,
        qty: u64,
        tif: TimeInForce,
        request_type: RequestType,
        hash_byte: u8,
    ) -> OrderRequest {
        let mut h = [0u8; 32];
        h[0] = hash_byte;
        OrderRequest {
            price,
            quantity: qty,
            origin_ts: 1_000,
            instrument_id: INSTR,
            side,
            order_type: OrderType::Limit,
            tif,
            request_type,
            intent_hash: IntentHash(h),
        }
    }

    fn engine_with_book() -> Engine {
        let mut e = Engine::new();
        e.add_instrument(INSTR, 64).unwrap();
        e
    }

    /// Rest a GTC maker on the book and assert it actually rested.
    fn rest_maker(e: &mut Engine, side: Side, price: u64, qty: u64, hash_byte: u8) {
        let mut out = Vec::new();
        e.process(&req(side, price, qty, TimeInForce::GTC, RequestType::New, hash_byte), &mut out);
        assert!(matches!(out.as_slice(), [OrderEvent::New(_)]), "maker should rest, got {out:?}");
    }

    /// Process one request and return only the events it produced.
    fn run(e: &mut Engine, r: &OrderRequest) -> Vec<OrderEvent> {
        let mut out = Vec::new();
        e.process(r, &mut out);
        out
    }

    // --- assertion helpers -------------------------------------------------

    /// Number of fill events (taker + maker, Fill or PartialFill).
    fn fill_count(ev: &[OrderEvent]) -> usize {
        ev.iter()
            .filter(|e| matches!(e, OrderEvent::Fill { .. } | OrderEvent::PartialFill { .. }))
            .count()
    }

    /// (reason, remaining_qty) of the single Reject, if any.
    fn reject(ev: &[OrderEvent]) -> Option<(RejectReason, u64)> {
        ev.iter().find_map(|e| match e {
            OrderEvent::Reject { reason, remaining_qty, .. } => Some((*reason, *remaining_qty)),
            _ => None,
        })
    }

    fn new_event(ev: &[OrderEvent]) -> Option<Order> {
        ev.iter().find_map(|e| match e {
            OrderEvent::New(o) => Some(*o),
            _ => None,
        })
    }

    // --- GTC ---------------------------------------------------------------

    #[test]
    fn gtc_with_no_cross_rests_whole_order() {
        let mut e = engine_with_book();
        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::GTC, RequestType::New, 1));

        assert_eq!(fill_count(&out), 0);
        assert!(reject(&out).is_none());
        let rested = new_event(&out).expect("should emit New");
        assert_eq!((rested.price, rested.quantity), (100, 10));

        let (price, level) = e.book(INSTR).unwrap().best_bid().unwrap();
        assert_eq!((price, level.total_qty()), (100, 10));
    }

    #[test]
    fn gtc_partial_cross_rests_remainder() {
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 100, 4, 1);

        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::GTC, RequestType::New, 2));

        // 4 crossed (one taker + one maker event); remainder 6 rests.
        assert_eq!(fill_count(&out), 2);
        assert!(reject(&out).is_none());
        assert_eq!(new_event(&out).unwrap().quantity, 6);

        let book = e.book(INSTR).unwrap();
        assert!(book.best_ask().is_none(), "ask fully consumed");
        assert_eq!(book.best_bid().unwrap().1.total_qty(), 6, "remainder rests as bid");
    }

    // --- IOC ---------------------------------------------------------------

    #[test]
    fn ioc_partial_fill_rejects_only_unfilled_remainder() {
        // Guards the bug where IOC reported req.quantity instead of `remaining`.
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 100, 4, 1);

        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::IOC, RequestType::New, 2));

        assert_eq!(fill_count(&out), 2, "4 units crossed");
        assert_eq!(
            reject(&out),
            Some((RejectReason::InsufficientLiquidity, 6)),
            "reject must report the 6 unfilled units, not the original 10"
        );
        assert!(e.book(INSTR).unwrap().best_bid().is_none(), "IOC never rests");
    }

    #[test]
    fn ioc_with_no_liquidity_rejects_full_quantity_no_fills() {
        let mut e = engine_with_book();
        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::IOC, RequestType::New, 1));

        assert_eq!(fill_count(&out), 0);
        assert_eq!(reject(&out), Some((RejectReason::InsufficientLiquidity, 10)));
        assert!(e.book(INSTR).unwrap().best_bid().is_none());
    }

    // --- FOK ---------------------------------------------------------------

    #[test]
    fn fok_insufficient_liquidity_rejects_with_zero_fills() {
        // Guards the missing-`return` bug: without it, FOK would emit partial
        // fills and then hit `debug_assert!(false)` -> panic in this test.
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 100, 4, 1); // only 4 available, need 10

        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::FOK, RequestType::New, 2));

        assert_eq!(fill_count(&out), 0, "FOK is all-or-nothing: no partial fills");
        assert_eq!(reject(&out), Some((RejectReason::InsufficientLiquidity, 10)));
        assert_eq!(
            e.book(INSTR).unwrap().best_ask().unwrap().1.total_qty(),
            4,
            "resting maker must be untouched"
        );
    }

    #[test]
    fn fok_sufficient_liquidity_fills_completely() {
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 100, 12, 1); // more than enough

        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::FOK, RequestType::New, 2));

        assert_eq!(fill_count(&out), 2, "taker fully filled + maker partially");
        assert!(reject(&out).is_none());
        assert_eq!(
            e.book(INSTR).unwrap().best_ask().unwrap().1.total_qty(),
            2,
            "12 resting - 10 consumed = 2 remain"
        );
    }

    #[test]
    fn fok_pre_check_respects_price_limit() {
        // Liquidity exists, but only above the taker's limit -> FOK must reject.
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 110, 50, 1);

        let out = run(&mut e, &req(Side::Bid, 100, 10, TimeInForce::FOK, RequestType::New, 2));

        assert_eq!(fill_count(&out), 0);
        assert_eq!(reject(&out), Some((RejectReason::InsufficientLiquidity, 10)));
        assert_eq!(e.book(INSTR).unwrap().best_ask().unwrap().1.total_qty(), 50);
    }

    // --- price-time priority ----------------------------------------------

    #[test]
    fn trade_executes_at_resting_maker_price() {
        // Aggressive taker crosses; the trade prints at the maker's resting
        // price (price improvement for the taker), not the taker's limit.
        let mut e = engine_with_book();
        rest_maker(&mut e, Side::Ask, 100, 10, 1);

        let out = run(&mut e, &req(Side::Bid, 105, 10, TimeInForce::GTC, RequestType::New, 2));

        assert_eq!(fill_count(&out), 2);
        for ev in &out {
            match ev {
                OrderEvent::Fill { fill_price, .. }
                | OrderEvent::PartialFill { fill_price, .. } => assert_eq!(*fill_price, 100),
                _ => {}
            }
        }
    }
}