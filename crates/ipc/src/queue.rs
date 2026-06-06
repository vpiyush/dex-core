use std::cell::Cell;
use crate::sync::{Ordering, AtomicU64, fence};
use std::marker::PhantomData;
use std::sync::Arc;
use crate::policy::LapPolicy;
use crate::slot::Slot;
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


impl <T: Pod> Queue<T> {
    // queue new exactly one producer is returned with queue
    fn new(capacity: u32) -> (Arc<Self>, Producer<T>) {
        #[cfg(not(loom))]
        const {
            assert!(
                core::mem::size_of::<Slot<T>>() <=64,
                "Slot<T> must be less than 64 bytes, must fit in one cache line"
            );
        }
        assert!(capacity.is_power_of_two(), "capacity should be a power of two");
        let mut vec = Vec::with_capacity(capacity as usize);
        for i in 0..capacity {
            vec.push( Slot::new() )
        }
        let slots = vec.into_boxed_slice();
        let mask = capacity - 1;
        let log2_cap = capacity.trailing_zeros() as u8;
        let queue = Arc::new(Self {
            slots,
            capacity,
            mask,
            log2_cap,
            published: CachePadded::new(AtomicU64::new(0))
        });
        let producer = Producer {
            queue: Arc::clone(&queue),
            cursor: 0,
            mask,
            log2_cap,
            _not_sync: PhantomData,
        };
        (queue, producer)
    }
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Acquire)
    }

    // subscribe returns a new consumer for the queue
    pub fn subscribe(self: &Arc<Self>, policy: LapPolicy) -> Consumer<T>{
        Consumer {
            queue: self.clone(),
            cursor: self.published(),
            mask: self.mask,
            log2_cap: self.log2_cap,
            policy,
            halted: false,
            _not_sync: PhantomData,
        }
    }

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

impl <T> Producer<T> {
    pub fn publish(&mut self, value: T) -> u64{
        // producer gets an acces to the queue,
        // get to the current cursor, picks the slot
        // before writing increments the version
        // writes on to the unsafe data pointer
        // incrments the version again
        // increments the published again
        let seq = self.cursor;
        let slot = &self.queue.slots[(seq as usize) & (self.mask as usize)];

        let v =  2 *(seq>> self.log2_cap);
        slot.version.store(v+1, Ordering::Relaxed);
        // the odd version store, would be globally visible, just before the data write.
        // release fence here ensures that the data write does not leak before the version store
        // we could use the fetch_add, but we do not need that strong atomicity here, because
        // there is always one producer here
        // note: we use stand alone fence, which is not tied to any store, so it acts as
        // a complete barrier for all kind of `Store` operations, and no store can
        // be reordered across this
        fence(Ordering::Release);
        // exactly one producer exists, so we are the sole writer here, the version
        // is odd for the whole window.
        slot.data.with_mut(|ptr|unsafe{ ptr.write(value) });
        // version updated even. Release publishes this data write to any consumer
        // that acquire-loads this version and sees v+2
        slot.version.store(v+2, Ordering::Release);
        self.cursor = seq + 1;
        self.queue.published.store(self.cursor, Ordering::Release);
        seq
    }
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