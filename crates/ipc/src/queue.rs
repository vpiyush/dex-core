use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Arc;
use crate::policy::LapPolicy;
use crate::slot::Slot;
use crate::sync::AtomicU64;
use bytemuck::Pod;

// A `T` - Sized filed forced into its own cache line, so a hot writer
// can't false share it with read-mostly neighbours
#[repr(align(64))]
pub(crate) struct CachePadded<T>(T);
impl<T> CachePadded<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(value)
    }
}
impl<T>  core::ops::Deref for CachePadded<T>{
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}


#[repr(C)] // no need of aligned64 here, as struct alignment is the max of it's fields
// since we have CachePadded align(64) queue is automatically align(64)
pub struct Queue<T> {
    slots: Box<[Slot<T>]>,
    capacity: u32,
    mask: u32,
    log2_cap: u8,
    // producer committed cursor, mirrored for subscribe. own cache line
    published: CachePadded<AtomicU64>
}


// Unique write, exactly one per queue, not cloneable
pub struct Producer<T> {
    queue: Arc<Queue<T>>,
    cursor: u64,
    mask: u32,
    log2_cap: u8,
    // phantom marker to make it !sync, (Cell is Send + !Sync), without this the struct would be send + sync
    // and we do not to threads sharing same Producer, while the ownership transfer is allowed
    _not_sync: PhantomData<Cell<()>>
}

// Independent reader, every reader owns it's cursor and lapPolicy
pub struct Consumer<T> {
    queue: Arc<Queue<T>>,
    cursor: u64,
    mask: u32,
    log2_cap: u8,
    policy: LapPolicy,
    halted: bool,
    _not_sync: PhantomData<Cell<()>>
}