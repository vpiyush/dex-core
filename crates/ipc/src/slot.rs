
use crate::sync::{AtomicU64, UnsafeCell};
use bytemuck::{Pod, Zeroable};


// one rung buffer slot, which is seqlock protected cell, isolated on it's
// own cache line
// `version` is the seqlock counter
//  - even = stable (write complete)
//  - odd = write-in-progress.
// `data` is the payload, which can be mutated through unsafeCell

#[repr(C, align(64))]
pub(crate) struct Slot<T> {
    pub(crate) version: AtomicU64,
    pub(crate) data: UnsafeCell<T>
}

impl<T: Pod> Slot<T>{
    pub(crate) fn new() -> Self{
        Self {
            version: AtomicU64::new(0),
            data: UnsafeCell::new(T::zeroed())
        }
    }
}

// UnsafeCell is !Sync, so we assert these by hand
unsafe impl<T: Pod> Send for Slot<T> {}
unsafe impl<T: Pod> Sync for Slot<T> {}
