use std::collections::btree_map::OccupiedEntry;

use arena::{Arena, ArenaIdx};
use rustc_hash::FxHashMap;
use types::{IntentHash, Order};

use crate::{PriceLevel, level::OrderNode};

pub struct LevelCursor<'a> {
    entry: OccupiedEntry<'a, u64, PriceLevel>,
    arena: &'a mut Arena<OrderNode>,
    index: &'a mut FxHashMap<IntentHash, ArenaIdx>,
    head: ArenaIdx, // cached current head
}

impl<'a> LevelCursor<'a> {
    pub(crate) fn new(
        entry: OccupiedEntry<'a, u64, PriceLevel>,
        arena: &'a mut Arena<OrderNode>,
        index: &'a mut FxHashMap<IntentHash, ArenaIdx>,
    ) -> Self {
        let head = entry.get().head;
        Self {
            entry,
            arena,
            index,
            head,
        }
    }
    pub fn price(&self) -> u64 {
        *self.entry.key()
    }

    pub fn head(&self) -> Option<&Order> {
        let head = self.head;
        if head.is_valid() {
            self.arena.get(head).map(|node| &node.order)
        } else {
            None
        }
    }

    pub fn pop_head(&mut self) -> bool {
        // remove from the arena, index map, update levelCursor and cursor both
        let head = self.head;
        let node = self.arena.remove(head).expect("cursor head must be live");
        self.index.remove(&node.order.intent_hash);
        let next = node.next;
        if let Some(n) = self.arena.get_mut(next) {
            // this is new head, It will now have any predecessors
            n.prev = ArenaIdx::SENTINEL;
        }
        let level = self.entry.get_mut();
        level.total_qty -= node.order.quantity;
        level.len -= 1;
        level.head = next;
        if !next.is_valid() {
            level.tail = ArenaIdx::SENTINEL;
        };
        self.head = next;
        next.is_valid()
    }

    pub fn reduce_head(&mut self, qty: u64) {
        let node = self
            .arena
            .get_mut(self.head)
            .expect("cursor head must be live");
        debug_assert!(
            qty < node.order.quantity,
            "reduce_head called with full qty; use pop_head for full consumption"
        );
        node.order.quantity -= qty;
        self.entry.get_mut().total_qty -= qty;
    }

    pub fn finish(self) {
        if self.entry.get().len == 0 {
            self.entry.remove();
        }
    }
}
