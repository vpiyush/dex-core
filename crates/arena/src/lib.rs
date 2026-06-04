//! Generational arena for stable slot-based storage on the hot path.
//!
//! [`Arena<T>`] owns a fixed-size `Box<[Slot<T>]>` of slots. Allocation returns
//! an [`ArenaIdx`] that pairs `(slot, generation)`; the generation bumps on
//! every `remove`, so stale handles to reused slots are detected and return
//! `None` from `get`/`remove` instead of aliasing fresh data.
//!
//! Two design choices matter for the rest of the workspace:
//!
//! * The backing storage is a `Box<[Slot<T>]>` rather than a `Vec<T>` so the
//!   slots never move — references handed out via `get` stay valid even when
//!   nearby slots are allocated or freed.
//! * The free list is intrusive: unoccupied slots' `MaybeUninit<T>` storage
//!   is reinterpreted as a `u32` "next free index" via raw pointer reads.
//!   This is why the `ASSERT_T_LAYOUT` const requires `T` to be `>= 4 bytes`
//!   and `>= 4-byte aligned`.
//!
//! Used by [`orderbook`](../orderbook/index.html) to store live `Order` records
//! keyed by stable indices that price-level FIFOs hold across re-allocation.
//!
//! See `docs/lld/arena.md` for the full design.

// Arena Idx is the Index of the slot in the arena. It also holds the generation
// to avoid acting on stale entries.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ArenaIdx{
    slot: u32,
    generation: u32, // generation changes on a use-after-free basis
}

impl ArenaIdx {
    // a handle that is guaranteed never to have real value,
    // this is the default for an arena
    pub const SENTINEL:Self = Self{slot: 0, generation: 0};

    // returns false for sentinel values and true if arena ever had
    // an object
    pub fn is_valid(self: Self) -> bool {
        self != Self::SENTINEL
    }

    pub fn new(slot: u32, generation: u32) -> Self {
        Self{slot, generation}
    }
}

use core::mem::MaybeUninit;
const MAX_CAPACITY:usize = u32::MAX as usize;

pub struct Arena<T> {
    // we deliberately don't choose vec, since it can be realloced ad moved into memory, which would invalidated the existing references
    slots : Box<[Slot<T>]>,
    free_head: u32,
    len : usize,
}

impl <T> Arena<T> {

    // assert T layout
    const ASSERT_T_LAYOUT: () =  {
        assert!(core::mem::size_of::<T>() >=4, "Arena <T> must be at least 4 bytes");
        assert!(core::mem::align_of::<T>() >=4, "Arena <T> must be at least 4 byte aligned");
    };

    pub fn new(capacity: u32 ) -> Self {
        // setup arena
        // allocate the chunk with capacity
        // initialize each slot with:
            // generation 0
            // occupied 0
            //  value as the next free slot index
        // layout assertions
        let _ = Self::ASSERT_T_LAYOUT;
        assert!(capacity > 0 && capacity < u32::MAX , "invalid capacity");
        // create the memory directly into heap
        let mut slots: Vec<Slot<T>> = (0..capacity).map(|_| Slot {
            generation: 1,
            occupied: 0,
            value: MaybeUninit::uninit()
        }).collect();

        for i in 0..capacity {
            let next_idx = if i + 1 < capacity {
                i + 1
            } else {
                u32::MAX
            };
            // write next
            unsafe  {
                slots[i as usize].write_next_free(next_idx)
            }
        };
        Self {
            slots: slots.into_boxed_slice(),
            free_head: 0,
            len: 0,
        }
    }

    pub fn get(&self, idx: ArenaIdx) -> Option<&T> {
        if !idx.is_valid() {
            return None;
        }
        self.slots.get(idx.slot as usize).and_then(|slot| {
            slot.get(idx.generation)
        })
    }

    pub fn get_mut(&mut self, idx: ArenaIdx) -> Option<&mut T> {
        if !idx.is_valid() {
            return None;
        }
        self.slots.get_mut(idx.slot as usize).and_then(|slot| {
            slot.get_mut(idx.generation)
        })
    }


    pub fn alloc(&mut self, value: T) ->Option<ArenaIdx> {
        // todo: full arena handling, should we overwrite the old values ?
        if self.len >= MAX_CAPACITY || self.free_head == u32::MAX {
            return None
        }

        let slot_idx = self.free_head;
        let slot = &mut self.slots[slot_idx as usize];
        let next_free_slot = unsafe { slot.read_next_free() };
        let generation = slot.generation;
        slot.insert(value);

        self.free_head = next_free_slot;
        self.len += 1;
        Some( ArenaIdx{slot: slot_idx, generation} )
    }

    pub fn remove(&mut self, arena_idx: ArenaIdx) -> Option<T> {
        let slot = self.slots.get_mut(arena_idx.slot as usize)?;
        let value = slot.remove(arena_idx.generation)?;
        // we successfully removed the value so it's safe to now write the current free head in the slot
        // and update the free head to the slot itself
        unsafe { slot.write_next_free(self.free_head) };
        self.len -= 1;
        self.free_head = arena_idx.slot ;
        Some(value)
    }

}

impl<T> Drop for Arena<T> {
    fn drop(&mut self) {
        // for Order there are no droppable member, we avoid the loop entirely in such cases
        if !core::mem::needs_drop::<T>() {
            return;
        }
        for slot in self.slots.iter_mut() {
            if slot.occupied == 1 {
                unsafe { slot.value.assume_init_drop(); }
            }
        }
    }

}

// a slot in the arena, updates generation every times it's reused
#[repr(C)]
pub struct Slot<T> {
    generation: u32,
    occupied: u8,
    value: MaybeUninit<T>
}

impl<T> Slot<T>{
    fn insert(&mut self, value: T) {
        debug_assert_eq!(self.occupied, 0, "insert on occupied slot");
        self.value.write(value);
        self.occupied = 1;
    }

    fn get(&self, generation: u32) -> Option<&T> {
        if self.occupied == 1 && generation == self.generation {
            Some(unsafe { self.value.assume_init_ref() })
        } else {
            None
        }
    }

    fn get_mut(&mut self, generation: u32) -> Option<&mut T> {
        if self.occupied == 1 && generation == self.generation {
            Some(unsafe { self.value.assume_init_mut() })
        } else { None }
    }

    fn remove(&mut self, generation: u32) -> Option<T> {
        if self.occupied != 1 || self.generation != generation {
            return None;
        }
        self.occupied = 0;
        self.generation = match self.generation.wrapping_add(1) {
            0 => 1,
            generation => generation,
        };
        Some( unsafe { self.value.assume_init_read() })
    }

    unsafe fn write_next_free(&mut self, next:u32) {
        let ptr = self.value.as_mut_ptr() as *mut u32;
        unsafe  {
            ptr.write(next)
        }
    }

    unsafe fn read_next_free(&self) -> u32 {
        let ptr = self.value.as_ptr() as *const u32;
        unsafe  {
            ptr.read()
        }
    }
}

#[cfg(test)]
mod tests {
    use bytemuck::Zeroable;
    use types::{IntentHash, OrderId};
    use super::*;

    // ----- §12.1 Round-trip and basic operations --------------------------------

    #[test]
    fn round_trip_alloc_get_remove() {
        let mut arena = Arena::<u64>::new(4);
        let idx = arena.alloc(42).unwrap();
        assert!(idx.is_valid());
        assert_eq!(arena.get(idx), Some(&42));
        assert_eq!(arena.remove(idx), Some(42));
        assert_eq!(arena.get(idx), None);
    }

    #[test]
    fn get_mut_allows_mutation() {
        let mut arena = Arena::<u64>::new(4);
        let idx = arena.alloc(100).unwrap();
        *arena.get_mut(idx).unwrap() = 200;
        assert_eq!(arena.get(idx), Some(&200));
    }

    #[test]
    fn initial_alloc_handle_is_valid() {
        // Regression guard: slots must initialize with generation >= 1 so the
        // first alloc cannot return a handle that equals ArenaIdx::SENTINEL.
        let mut arena = Arena::<u64>::new(1);
        let idx = arena.alloc(42).unwrap();
        assert!(idx.is_valid(), "initial alloc must not equal SENTINEL");
        assert_ne!(idx.generation, 0, "generation 0 is reserved for SENTINEL");
    }

    // ----- §12.2 Use-after-free detection ---------------------------------------

    #[test]
    fn get_after_remove_returns_none() {
        let mut arena = Arena::<u64>::new(4);
        let idx = arena.alloc(7).unwrap();
        arena.remove(idx);
        assert_eq!(arena.get(idx), None);
        assert_eq!(arena.get_mut(idx), None);
    }

    #[test]
    fn double_remove_returns_none() {
        let mut arena = Arena::<u64>::new(4);
        let idx = arena.alloc(7).unwrap();
        assert_eq!(arena.remove(idx), Some(7));
        assert_eq!(arena.remove(idx), None);
    }

    #[test]
    fn stale_handle_after_slot_reuse_returns_none() {
        let mut arena = Arena::<u64>::new(4);
        let a = arena.alloc(1).unwrap();
        arena.remove(a);
        let b = arena.alloc(2).unwrap();
        if a.slot == b.slot {
            assert_ne!(a.generation, b.generation, "generation must bump on reuse");
        }
        assert_eq!(arena.get(a), None, "old handle must not see new value");
        assert_eq!(arena.get(b), Some(&2));
    }

    // ----- §12.3 Capacity exhaustion --------------------------------------------

    #[test]
    fn alloc_returns_none_when_full() {
        let mut arena = Arena::<u64>::new(2);
        let _a = arena.alloc(1).unwrap();
        let _b = arena.alloc(2).unwrap();
        assert!(arena.alloc(3).is_none(), "alloc on full arena must return None");
    }

    #[test]
    fn remove_makes_capacity_available_again() {
        let mut arena = Arena::<u64>::new(2);
        let a = arena.alloc(1).unwrap();
        let _b = arena.alloc(2).unwrap();
        assert!(arena.alloc(3).is_none(), "arena should be full");
        arena.remove(a);
        let c = arena.alloc(3).unwrap();
        assert_eq!(arena.get(c), Some(&3));
    }

    // ----- §12.4 Free-list LIFO ordering ----------------------------------------

    #[test]
    fn free_list_is_lifo() {
        let mut arena = Arena::<u64>::new(4);
        let a = arena.alloc(1).unwrap();
        let b = arena.alloc(2).unwrap();
        let c = arena.alloc(3).unwrap();
        arena.remove(a);
        arena.remove(b);
        arena.remove(c);
        // most-recently-freed should pop first
        let x = arena.alloc(10).unwrap();
        let y = arena.alloc(20).unwrap();
        let z = arena.alloc(30).unwrap();
        assert_eq!(x.slot, c.slot, "expected LIFO: c popped first");
        assert_eq!(y.slot, b.slot, "expected LIFO: b popped second");
        assert_eq!(z.slot, a.slot, "expected LIFO: a popped third");
    }

    // ----- §12.5 Generation skip-0 wraparound -----------------------------------

    #[test]
    fn generation_wraps_to_1_skipping_0() {
        // Forcing 2^32 removes is infeasible; poke the generation directly via
        // private field access (only legal because tests live in the crate).
        let mut arena = Arena::<u64>::new(1);
        let idx = arena.alloc(42).unwrap();
        let slot_idx = idx.slot as usize;

        arena.slots[slot_idx].generation = u32::MAX;

        // Forge a handle whose generation matches the poked slot.
        let forged = ArenaIdx::new(idx.slot, u32::MAX);
        assert_eq!(arena.remove(forged), Some(42));

        // After remove, generation should have stepped u32::MAX -> 0 -> 1.
        assert_eq!(
            arena.slots[slot_idx].generation, 1,
            "generation must skip 0 on wraparound"
        );

        let new_idx = arena.alloc(99).unwrap();
        assert!(new_idx.is_valid(), "post-wraparound handle must not equal SENTINEL");
        assert_eq!(new_idx.generation, 1);
    }

    // ----- §12.6 Drop runs on remaining occupants -------------------------------

    #[test]
    fn drop_runs_on_remaining_occupants() {
        use core::sync::atomic::{AtomicUsize, Ordering};
        static DROPS: AtomicUsize = AtomicUsize::new(0);

        #[derive(Debug)]
        struct Tracker(u32);
        impl Drop for Tracker {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROPS.store(0, Ordering::Relaxed);
        {
            let mut arena = Arena::<Tracker>::new(4);
            let _ = arena.alloc(Tracker(1)).unwrap();
            let b   = arena.alloc(Tracker(2)).unwrap();
            let _ = arena.alloc(Tracker(3)).unwrap();
            arena.remove(b); // drops Tracker(2) as the returned Option<T> dies
        }                    // arena drops here; expect 2 more drops

        assert_eq!(
            DROPS.load(Ordering::Relaxed), 3,
            "Drop did not run on all live occupants"
        );
    }

    // ----- §12.7 Generic instantiation ------------------------------------------

    #[test]
    fn instantiates_with_u64() {
        let mut a = Arena::<u64>::new(2);
        let i = a.alloc(1).unwrap();
        assert_eq!(a.remove(i), Some(1));
    }

    #[test]
    fn instantiates_with_u32() {
        let mut a = Arena::<u32>::new(2);
        let i = a.alloc(1u32).unwrap();
        assert_eq!(a.remove(i), Some(1u32));
    }

    #[test]
    fn instantiates_with_tuple() {
        let mut a = Arena::<(u32, u32)>::new(2); // size 8, align 4
        let i = a.alloc((1, 2)).unwrap();
        assert_eq!(a.remove(i), Some((1, 2)));
    }

    #[test]
    fn instantiates_with_order() {
        use types::{Order, Side, OrderType, TimeInForce};
        let mut a = Arena::<Order>::new(2);
        let order = Order {
            order_id: OrderId(1),
            price: 100,
            quantity: 1,
            origin_ts: 0,
            instrument_id: 0,
            side: Side::Ask,
            order_type: OrderType::Limit,
            tif: TimeInForce::IOC,
            _padding: 0,
            intent_hash: IntentHash::zeroed()
        };
        let i = a.alloc(order).unwrap();
        assert!(a.get(i).is_some(), "Arena<Order> alloc/get round-trip failed");
    }

    // ----- Extras: free-list integrity guards -----------------------------------

    #[test]
    fn double_free_does_not_corrupt_list() {
        let mut arena = Arena::<u64>::new(4);
        let idx = arena.alloc(42).unwrap();
        assert_eq!(arena.remove(idx), Some(42));
        assert_eq!(arena.remove(idx), None);
        let a = arena.alloc(1).unwrap();
        let b = arena.alloc(2).unwrap();
        assert_ne!(a.slot, b.slot, "free list corrupted by double-free");
        assert_eq!(arena.get(a), Some(&1));
        assert_eq!(arena.get(b), Some(&2));
    }

    #[test]
    fn stale_remove_does_not_decrement_len() {
        let mut arena = Arena::<u64>::new(2);
        let idx = arena.alloc(7).unwrap();
        assert_eq!(arena.len, 1);
        arena.remove(idx);
        assert_eq!(arena.len, 0);
        arena.remove(idx); // stale; must NOT decrement
        assert_eq!(arena.len, 0, "len underflowed on stale remove");
    }
}