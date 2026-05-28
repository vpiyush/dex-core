use rustc_hash::FxHashMap;
use orderbook::OrderBook;
use types::{OrderEvent, OrderRequest, OrderType, RejectReason, RequestType};
use crate::events::{push_cancel, push_reject};
use crate::invariants::assert_invariants;
use crate::matching::{match_limit, match_market};

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

        // price must be > 0 for limit orders, market orders will carry price 0 by convention
        if matches!(req.order_type, OrderType::Limit) && req.price == 0 {
            return push_reject(RejectReason::InvalidPrice, req.quantity, req, out);
        }

        // validation passed - burn an orderID
        let order_id = self.mint_order_id();
        let book = self.books .get_mut(&req.instrument_id)
            .expect("invariant: book existence checked at step 1");

        match req.order_type {
            OrderType::Limit => {
                match_limit(book, req, order_id, out);
            }
            OrderType::Market => {
                match_market(book, req, order_id, out);
            }
        }
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

    fn process_amend(&mut self, req: &OrderRequest, out: &mut Vec<OrderEvent>) {
        todo!("amend deferred to later")
    }

}