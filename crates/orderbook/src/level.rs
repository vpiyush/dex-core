use std::collections::VecDeque;
use arena::ArenaIdx;

pub struct  PriceLevel  {
    pub(crate) orders: VecDeque<ArenaIdx>,
    pub(crate) total_qty: u64
}

// we could also just derive default, the default will not be needed then
impl Default for PriceLevel {
    fn default() -> Self {
        Self {
            orders: VecDeque::new(),
            total_qty: 0
        }
    }
}

impl PriceLevel {
    pub fn len(&self) -> usize {
        self.orders.len()
    }
    pub fn total_qty(&self) -> u64 {
        self.total_qty
    }
    // top of the price level
    pub fn head(&self) -> Option<ArenaIdx> {
        self.orders.front().copied()
    }
    // lifetime is optional here, but good for the clarity
    pub fn iter(&self) -> impl Iterator<Item=ArenaIdx> + '_{
        self.orders.iter().copied()
    }
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}