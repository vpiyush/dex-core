use std::collections::BTreeMap;
use rustc_hash::{FxBuildHasher, FxHashMap};
use arena::{Arena, ArenaIdx};
use types::{IntentHash, Order, OrderId, Side};
use crate::level::PriceLevel;

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    ArenaFull,
    DuplicateIntent
}

pub struct TopView <'a>{
    pub price: u64,
    pub arena_idx: ArenaIdx,
    pub order: &'a Order,
}

pub struct OrderBook {
    pub(crate) instrument_id: u32,
    pub(crate) arena: Arena<Order>,
    pub(crate) bids: BTreeMap<u64,PriceLevel>,
    pub(crate) asks: BTreeMap<u64,PriceLevel>,
    pub(crate) index: FxHashMap<IntentHash, ArenaIdx>,
}

impl OrderBook {
    pub fn new(instrument_id: u32, capacity: u32) -> Self {
        Self {
            instrument_id,
            arena: Arena::new(capacity),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            index: FxHashMap::with_capacity_and_hasher(capacity as usize, FxBuildHasher::default()),
        }
    }
    pub fn instrument_id(&self) -> u32 {
        self.instrument_id
    }
    pub fn best_bid(&self) -> Option<(u64, &PriceLevel)> {
        self.bids.last_key_value().map(|(&p, l)| (p, l))
    }
    pub fn best_ask(&self) -> Option<(u64, &PriceLevel)> {
        self.asks.first_key_value().map(|(&p, l)| (p, l))
    }

    pub fn is_empty(&self, side: Side) -> bool {
        match side {
            Side::Bid => self.bids.is_empty(),
            Side::Ask => self.asks.is_empty(),
        }
    }

    pub fn is_crossed(&self) -> bool {
        match (self.bids.last_key_value(), self.asks.first_key_value()) {
            (Some((&bid, _)), Some((&ask, _))) => bid >=ask,
            _ => false
        }
    }

    pub fn level_qty(&self, side: Side, price: u64) -> u64 {
        let level = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        level.get(&price).map(| l|l.total_qty()).unwrap_or(0)
    }

    pub fn peek_top(&self, side: Side) -> Option<TopView<'_>>{
        let (&price, level) = match side {
            Side::Bid => self.bids.last_key_value()?,
            Side::Ask => self.asks.first_key_value()?,
        };
        let arena_idx = level.head()?;
        let order = self.arena.get(arena_idx)?;
        Some(TopView { price, arena_idx, order })
    }

    // returns bid depth upto max_level, since btree is ascending we start reverse
    pub fn bid_depth(&self, max_levels: usize) -> impl Iterator <Item=(u64, u64)> + '_ {
        self.bids.iter().rev().take(max_levels).map(|(&p, l)|(p, l.total_qty()))
    }

    // returns ask depth upto max level Or whatever is available if len is less than max_level
    pub fn ask_depth(&self, max_levels: usize) -> impl Iterator <Item=(u64, u64)> + '_ {
        self.asks.iter().take(max_levels).map(|(&p, l)|(p, l.total_qty()))
    }

    pub fn insert(&mut self, order: Order) -> Result<ArenaIdx, InsertError> {
        // reject if the order is already in index
        if self.index.contains_key(&order.intent_hash) {
            return Err(InsertError::DuplicateIntent);
        }
        // allocate a slot in the arena
        let arena_idx = self.arena.alloc(order).ok_or(InsertError::ArenaFull)?;

        // get correct side
        let level_map = match order.side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        // find the entry in the level map
        let level = level_map.entry(order.price).or_default();
        level.orders.push_back(arena_idx);
        level.total_qty += order.quantity;

        self.index.insert(order.intent_hash, arena_idx);

        #[cfg(debug_assertions)]
        crate::invariants::assert_invariants(self);

        Ok(arena_idx)
    }

    pub fn cancel(&mut self, intent_hash: &IntentHash) -> Option<Order> {
       let arena_idx =  self.index.remove(intent_hash)?;
        let order = self.arena.remove(arena_idx)?;
        let level_map = match order.side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let level = level_map.get_mut(&order.price).expect("invariant: price level must exist");

        // removing from level position (O(n))  - needs improvement
        let pos = level.orders.iter().position(|&x| x == arena_idx).expect("invariant: order index must exist");
        level.orders.remove(pos);
        level.total_qty -= order.quantity;

        // check if the level is empty now
        let is_empty = level.is_empty();
        if is_empty {
            level_map.remove(&order.price);
        }

        #[cfg(debug_assertions)]
        crate::invariants::assert_invariants(self);

        Some(order)
    }

    // pop the best price from the given side
    // for bids => the largest
    //  asks => the smallest
    pub fn pop_top(&mut self, side: Side) -> Option<Order> {
        let level_map = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let price =  *match side {
            Side::Bid => level_map.last_key_value()?.0,
            Side::Ask => level_map.first_key_value()?.0,
        };

        let level = level_map.get_mut(&price).expect("invariant: price level must exist");
        // get the first index from arena
        let arena_idx = level.orders.pop_front().expect("invariant: order index must exist");
        // remove order from arena
        let order = self.arena.remove(arena_idx).expect("invariant: level's head's arena slot must be valid");

        level.total_qty -= order.quantity;
        let is_empty = level.is_empty();
        if is_empty {
            level_map.remove(&price);
        }
        self.index.remove(&order.intent_hash);

        #[cfg(debug_assertions)]
        crate::invariants::assert_invariants(self);

        Some(order)
    }

    pub fn reduce_top(&mut self, side: Side, qty: u64) {
        let level_map = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let kv =  match side {
            Side::Bid => level_map.last_key_value(),
            Side::Ask => level_map.first_key_value(),
        };
        let &price = match kv {
            Some((p, _)) => p,
            None => {
                debug_assert!(false, "reduce_top called on empty side");
                return
            }
        };
        let level = level_map.get_mut(&price).expect("invariant: just-confirmed level missing");
        let arena_idx = level.orders.front().copied().expect("invariant: order index must exist");
        let order = self.arena.get_mut(arena_idx).expect("invariant: level's head's arena slot must be valid");

        debug_assert!(qty < order.quantity, "invariant: reduce top called with full qty, use pop_top for full consumption");
        order.quantity -= qty;
        level.total_qty -= qty;

        #[cfg(debug_assertions)]
        crate::invariants::assert_invariants(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{Side, OrderType, TimeInForce};

    fn make_order(price: u64, qty: u64, side: Side, hash_byte: u8) -> Order {
        let mut hash = [0u8; 32];
        hash[0] = hash_byte;
        Order {
            order_id: OrderId(1),
            price,
            quantity: qty,
            origin_ts: 0,
            instrument_id: 1,
            side,
            order_type: OrderType::Limit,
            tif: TimeInForce::GTC,
            _padding: 0,
            intent_hash: IntentHash(hash),
        }
    }

    #[test]
    fn insert_one_bid_makes_it_best() {
        let mut book = OrderBook::new(1, 64);
        let _ = book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        let (price, level) = book.best_bid().unwrap();
        assert_eq!(price, 100);
        assert_eq!(level.total_qty(), 10);
        assert_eq!(level.len(), 1);
    }

    #[test]
    fn duplicate_intent_rejected() {
        let mut book = OrderBook::new(1, 64);
        book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        let err = book.insert(make_order(101, 5, Side::Bid, 1)).unwrap_err();
        assert_eq!(err, InsertError::DuplicateIntent);
    }

    #[test]
    fn two_orders_same_price_share_level() {
        let mut book = OrderBook::new(1, 64);
        book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        book.insert(make_order(100, 7,  Side::Bid, 2)).unwrap();
        let (_, level) = book.best_bid().unwrap();
        assert_eq!(level.total_qty(), 17);
        assert_eq!(level.len(), 2);
    }

    #[test]
    fn higher_bid_beats_lower() {
        let mut book = OrderBook::new(1, 64);
        book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        book.insert(make_order(105, 5,  Side::Bid, 2)).unwrap();
        let (price, _) = book.best_bid().unwrap();
        assert_eq!(price, 105);
    }

    #[test]
    fn cancel_existing_order_returns_it() {
        let mut book = OrderBook::new(1, 64);
        let original = make_order(100, 10, Side::Bid, 1);
        book.insert(original).unwrap();
        let cancelled = book.cancel(&original.intent_hash).unwrap();
        assert_eq!(cancelled, original);
    }

    #[test]
    fn cancel_nonexistent_returns_none() {
        let mut book = OrderBook::new(1, 64);
        let hash = IntentHash([99u8; 32]);
        assert!(book.cancel(&hash).is_none());
    }

    #[test]
    fn cancel_updates_total_qty() {
        let mut book = OrderBook::new(1, 64);
        let a = make_order(100, 10, Side::Bid, 1);
        let b = make_order(100, 7,  Side::Bid, 2);
        book.insert(a).unwrap();
        book.insert(b).unwrap();
        book.cancel(&a.intent_hash).unwrap();
        let (_, level) = book.best_bid().unwrap();
        assert_eq!(level.total_qty(), 7);
        assert_eq!(level.len(), 1);
    }

    #[test]
    fn cancel_empties_level_removes_it() {
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        assert!(book.best_bid().is_some());
        book.cancel(&order.intent_hash).unwrap();
        assert!(book.best_bid().is_none(), "empty level should be removed");
    }

    #[test]
    fn cancel_one_of_many_levels_preserves_others() {
        let mut book = OrderBook::new(1, 64);
        let a = make_order(100, 10, Side::Bid, 1);
        let b = make_order(105, 5,  Side::Bid, 2);
        book.insert(a).unwrap();
        book.insert(b).unwrap();
        book.cancel(&b.intent_hash).unwrap();  // cancels the best bid
        let (price, _) = book.best_bid().unwrap();
        assert_eq!(price, 100, "after canceling best, second-best becomes best");
    }

    #[test]
    fn cancel_then_reinsert_same_hash_works() {
        // Tests that intent_hash isn't "blacklisted" after cancel — the index
        // entry is fully removed, so re-insert with the same hash is fine.
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        book.cancel(&order.intent_hash).unwrap();
        assert!(book.insert(order).is_ok(), "should be re-insertable after cancel");
    }

    #[test]
    fn pop_top_single_order_returns_it() {
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        let popped = book.pop_top(Side::Bid).unwrap();
        assert_eq!(popped, order);
        assert!(book.best_bid().is_none(), "level should be removed after popping only order");
    }

    #[test]
    fn pop_top_fifo_order() {
        // a inserted before b at same price → a pops first
        let mut book = OrderBook::new(1, 64);
        let a = make_order(100, 10, Side::Bid, 1);
        let b = make_order(100, 7,  Side::Bid, 2);
        book.insert(a).unwrap();
        book.insert(b).unwrap();
        assert_eq!(book.pop_top(Side::Bid).unwrap(), a);
        assert_eq!(book.pop_top(Side::Bid).unwrap(), b);
    }

    #[test]
    fn pop_top_empty_side_returns_none() {
        let mut book = OrderBook::new(1, 64);
        assert!(book.pop_top(Side::Bid).is_none());
        assert!(book.pop_top(Side::Ask).is_none());
    }

    #[test]
    fn pop_top_then_reinsert_works() {
        // Arena slot should be freed; intent_hash should be removable
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        book.pop_top(Side::Bid).unwrap();
        assert!(book.insert(order).is_ok());
    }

    #[test]
    fn pop_top_walks_levels() {
        // Two levels; popping best exposes second-best
        let mut book = OrderBook::new(1, 64);
        let a = make_order(100, 10, Side::Bid, 1);
        let b = make_order(105, 5,  Side::Bid, 2);
        book.insert(a).unwrap();
        book.insert(b).unwrap();
        let popped = book.pop_top(Side::Bid).unwrap();
        assert_eq!(popped.price, 105);  // best bid first
        let (price, _) = book.best_bid().unwrap();
        assert_eq!(price, 100);
    }

    #[test]
    fn reduce_top_partial_qty() {
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        book.reduce_top(Side::Bid, 3);
        let (_, level) = book.best_bid().unwrap();
        assert_eq!(level.total_qty(), 7);
        assert_eq!(level.len(), 1, "order stays — partial fill doesn't remove");
    }

    #[test]
    fn reduce_top_preserves_head_identity() {
        // After reduce, peek_top should return the same arena_idx + intent_hash
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        let top_before = book.peek_top(Side::Bid).unwrap();
        let idx_before = top_before.arena_idx;
        book.reduce_top(Side::Bid, 4);
        let top_after = book.peek_top(Side::Bid).unwrap();
        assert_eq!(top_after.arena_idx, idx_before);
        assert_eq!(top_after.order.quantity, 6);
    }

    #[test]
    fn reduce_top_multiple_calls_compound() {
        let mut book = OrderBook::new(1, 64);
        let order = make_order(100, 10, Side::Bid, 1);
        book.insert(order).unwrap();
        book.reduce_top(Side::Bid, 2);
        book.reduce_top(Side::Bid, 3);
        let (_, level) = book.best_bid().unwrap();
        assert_eq!(level.total_qty(), 5);
    }

    #[test]
    #[should_panic(expected = "reduce_top called on empty side")]
    fn reduce_top_empty_side_panics_in_debug() {
        // Only runs the assertion in debug builds; #[should_panic] expects the
        // panic. In release, this test would fail (no panic) — accepted limitation.
        let mut book = OrderBook::new(1, 64);
        book.reduce_top(Side::Bid, 1);
    }

    #[test]
    #[should_panic(expected = "qty")]
    fn reduce_top_overfill_panics_in_debug() {
        // qty == order.quantity should use pop_top instead; this is the contract.
        let mut book = OrderBook::new(1, 64);
        book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        book.reduce_top(Side::Bid, 10);
    }

    // ----- meta-test: the invariants checker actually catches violations -----

    #[test]
    #[should_panic(expected = "I2 violated")]
    fn invariant_check_catches_total_qty_drift() {
        // Deliberately desync level.total_qty from the actual sum, then call
        // the checker directly. The panic message must name I2.
        let mut book = OrderBook::new(1, 64);
        book.insert(make_order(100, 10, Side::Bid, 1)).unwrap();
        let level = book.bids.get_mut(&100).unwrap();
        level.total_qty = 999;
        crate::invariants::assert_invariants(&book);
    }
}

