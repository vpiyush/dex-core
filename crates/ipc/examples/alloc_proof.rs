//! Heap-allocation proof for the ipc hot path, via `benchkit` + `dhat`.
//!
//! Run with: `cargo run --release --example alloc_proof -p ipc`
//! (or `cargo bench-all --crate ipc --intent alloc`).
//!
//! `dhat` installs a global allocator shim that records every allocation. It is
//! a separate binary from the latency bench because the shim perturbs timing and
//! must never run during `rdtscp` measurement.
//!
//! What this proves: `publish` and `poll` do not allocate. The ring is allocated
//! once in `Queue::new`. After that, publish writes into a pre-existing slot and
//! poll reads from one. Neither touches the heap.

use benchkit::assert_zero_alloc;
use ipc::{Consumer, LapPolicy, Producer, Queue};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

struct St {
    prod: Producer<u64>,
    cons: Consumer<u64>,
    n: u64,
}

const WARMUP: usize = 1_000;
const MEASURE: usize = 500_000;

fn main() {
    std::fs::create_dir_all("bench-runs").ok();
    let _profiler = dhat::Profiler::builder()
        .file_name("bench-runs/ipc_alloc_proof_dhat-heap.json")
        .build();

    let (q, prod) = Queue::<u64>::new(1024);
    let cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    let mut st = St { prod, cons, n: 0 };

    // One publish + one poll per step. Producer and consumer stay in lockstep,
    // so the consumer is never lapped and the ring never reallocates.
    let step = |st: &mut St| {
        st.prod.publish(st.n);
        let _ = st.cons.poll();
        st.n = st.n.wrapping_add(1);
    };

    let delta = assert_zero_alloc("ipc hot path: publish + poll", &mut st, WARMUP, MEASURE, step);

    println!("# ipc allocation proof (dhat, via benchkit)\n");
    println!("| scenario | steps | alloc blocks | alloc bytes |");
    println!("|---|--:|--:|--:|");
    println!("| publish + poll (HOT PATH) | {MEASURE} | {} | {} |", delta.blocks, delta.bytes);
    println!("\nDHAT detail: bench-runs/ipc_alloc_proof_dhat-heap.json (view at dh_view.html)");
    println!("\n✓ Hot path is zero-allocation ({} blocks across {MEASURE} steps).", delta.blocks);
}
