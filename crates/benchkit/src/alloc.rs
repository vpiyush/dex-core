//! Heap-allocation measurement via `dhat` (feature `alloc`).
//!
//! The benchmark binary must install dhat as the global allocator for these to
//! see anything:
//! ```ignore
//! #[global_allocator]
//! static ALLOC: dhat::Alloc = dhat::Alloc;
//! ```
//! and hold a `dhat::Profiler` alive for the measured region. dhat's allocator
//! shim perturbs timing, so allocation measurement is a SEPARATE pass from the
//! rdtscp latency runs — never the same execution.

/// Allocation delta over a measured region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocDelta {
    pub blocks: u64,
    pub bytes: u64,
}

/// Allocations performed during `body`, as the delta of dhat's heap stats across
/// the call. The low-level primitive; prefer [`assert_zero_alloc`] /
/// [`measure_alloc`] when you have a per-step closure + warmup.
pub fn alloc_delta(body: impl FnOnce()) -> AllocDelta {
    let before = dhat::HeapStats::get();
    body();
    let after = dhat::HeapStats::get();
    AllocDelta {
        blocks: after.total_blocks - before.total_blocks,
        bytes: after.total_bytes - before.total_bytes,
    }
}

/// Run `step` `warmup` times untracked (to settle first-touch growth — initial
/// `Vec`/`HashMap`/arena capacity), then `measure` times under dhat tracking;
/// return the allocations performed during the tracked run.
///
/// `step` takes `&mut S`, so warmup and measured phases share one piece of state
/// without the double-`&mut`-borrow problem a two-closure (warm, measured)
/// signature would hit — the same threading the latency [`crate::Runner`] uses.
pub fn measure_alloc<S>(
    state: &mut S,
    warmup: usize,
    measure: usize,
    mut step: impl FnMut(&mut S),
) -> AllocDelta {
    for _ in 0..warmup {
        step(state);
    }
    alloc_delta(|| {
        for _ in 0..measure {
            step(state);
        }
    })
}

/// As [`measure_alloc`], but assert the tracked run allocated **zero** blocks —
/// the hot-path zero-allocation gate. Panics with the observed delta if not
/// (this is a CI/test gate, so panic == fail). Returns the delta on success so
/// the caller can report "0 blocks across N steps".
pub fn assert_zero_alloc<S>(
    label: &str,
    state: &mut S,
    warmup: usize,
    measure: usize,
    step: impl FnMut(&mut S),
) -> AllocDelta {
    let delta = measure_alloc(state, warmup, measure, step);
    assert_eq!(
        delta.blocks, 0,
        "{label}: hot path allocated {} blocks ({} bytes) over {measure} steps; expected zero",
        delta.blocks, delta.bytes
    );
    delta
}
