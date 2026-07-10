use arena::ArenaIdx;
use types::Order;

pub struct OrderNode {
    pub(crate) order: Order,
    pub(crate) next: ArenaIdx,
    pub(crate) prev: ArenaIdx,
}

pub struct PriceLevel {
    pub(crate) head: ArenaIdx,
    pub(crate) tail: ArenaIdx,
    pub(crate) len: u32,
    pub(crate) total_qty: u64,
}

impl Default for PriceLevel {
    fn default() -> Self {
        Self {
            head: ArenaIdx::SENTINEL,
            tail: ArenaIdx::SENTINEL,
            len: 0,
            total_qty: 0,
        }
    }
}

impl PriceLevel {
    pub fn len(&self) -> usize {
        self.len as usize
    }
    pub fn total_qty(&self) -> u64 {
        self.total_qty
    }
    // top of the price level
    pub fn head(&self) -> Option<ArenaIdx> {
        self.head.is_valid().then_some(self.head)
    }

    pub fn is_empty(&self) -> bool {
        !self.head.is_valid()
    }
}

