//! Heap-allocation proof for the matcher hot path, via `benchkit` + `dhat`.
//!
//! Run with: `cargo run --release --example alloc_proof -p matcher`
//! (or `cargo bench-all --crate matcher --intent alloc`).
//!
//! `dhat` installs a global allocator shim that records every allocation, so
//! this is a SEPARATE binary from the latency bench — the shim perturbs timing
//! and must never run during `rdtscp` measurement.
//!
//! What this proves: `Engine::process` does not allocate on the steady-state
//! matching path *as long as it neither creates nor destroys a price level and
//! the event buffer does not grow*. We construct exactly that workload (cancel +
//! re-insert at a single deep, pre-warmed level) and assert zero allocations.
//!
//! Known non-zero-alloc paths (out of scope, documented in the orderbook LLD):
//! emptying a level frees its BTreeMap node + VecDeque; re-creating it allocates
//! again. So a level-churning workload WILL allocate — measured below (Scenario
//! B), reported not asserted, for honesty.
//!
//! Mechanics (warmup, dhat-tracked delta, the zero-alloc assertion) come from
//! `benchkit`; this file owns only the domain scenarios and the dhat allocator
//! the binary must install.

use benchkit::{assert_zero_alloc, measure_alloc};
use matcher::Engine;
use types::{IntentHash, OrderEvent, OrderRequest, OrderType, RequestType, Side, TimeInForce};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const INSTR: u32 = 1;

fn hash_from_seed(seed: u64) -> IntentHash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&seed.to_ne_bytes());
    IntentHash(h)
}

fn new_req(side: Side, price: u64, qty: u64, seed: u64) -> OrderRequest {
    OrderRequest {
        price,
        quantity: qty,
        origin_ts: 0,
        instrument_id: INSTR,
        side,
        order_type: OrderType::Limit,
        tif: TimeInForce::GTC,
        request_type: RequestType::New,
        intent_hash: hash_from_seed(seed),
    }
}

fn cancel_req(seed: u64) -> OrderRequest {
    let mut r = new_req(Side::Bid, 0, 0, seed);
    r.request_type = RequestType::Cancel;
    r
}

fn engine() -> Engine {
    let mut e = Engine::new();
    e.add_instrument(INSTR, 65_536).expect("add_instrument");
    e
}

/// Shared scenario state threaded through warmup + measured phases.
struct St {
    engine: Engine,
    buf: Vec<OrderEvent>,
    seed: u64,
}

const WARMUP: usize = 1_000;
const MEASURE: usize = 500_000;
const PRICE: u64 = 1_000;
const DEPTH: u64 = 256; // pre-warm the level's VecDeque capacity

fn main() {
    // Profiler writes its json on drop (view at dh_view.html).
    let out = benchkit::out_dir();
    std::fs::create_dir_all(&out).expect("create out dir");
    let dhat_json = out.join("matcher_alloc_proof_dhat-heap.json");
    let _profiler = dhat::Profiler::builder().file_name(&dhat_json).build();

    // ----- Scenario A: zero-alloc steady state (the hot path) --------------
    // One bid level pre-filled to DEPTH so its VecDeque/HashMap are warm; then
    // cancel-oldest + re-insert at the SAME price. The level never empties and
    // capacities never grow, so process() must allocate nothing.
    let mut a = St { engine: engine(), buf: Vec::with_capacity(64), seed: 0 };
    for _ in 0..DEPTH {
        a.buf.clear();
        let r = new_req(Side::Bid, PRICE, 100, a.seed);
        a.engine.process(&r, &mut a.buf);
        a.seed += 1;
    }
    // One cancel+reinsert per step; cancels the oldest still-resting order.
    let step_a = |st: &mut St| {
        st.buf.clear();
        let cancel_seed = st.seed - DEPTH;
        st.engine.process(&cancel_req(cancel_seed), &mut st.buf);
        st.buf.clear();
        st.engine.process(&new_req(Side::Bid, PRICE, 100, st.seed), &mut st.buf);
        st.seed += 1;
    };
    let a_delta = assert_zero_alloc("hot path: cancel+reinsert, stable level", &mut a, WARMUP, MEASURE, step_a);

    // ----- Scenario B: level-churn (expected to allocate) ------------------
    // Cross a single resting maker, EMPTYING its level (frees BTreeMap node +
    // VecDeque), then replenish (re-creates it). The documented churn path.
    let mut b = St { engine: engine(), buf: Vec::with_capacity(64), seed: 0 };
    let step_b = |st: &mut St| {
        st.buf.clear();
        st.engine.process(&new_req(Side::Ask, PRICE, 1, st.seed), &mut st.buf);
        st.seed += 1;
        st.buf.clear();
        let mut taker = new_req(Side::Bid, PRICE, 1, st.seed);
        taker.tif = TimeInForce::IOC;
        st.engine.process(&taker, &mut st.buf);
        st.seed += 1;
    };
    let b_delta = measure_alloc(&mut b, WARMUP, MEASURE, step_b);

    // ----- Report ----------------------------------------------------------
    println!("# Matcher allocation proof (dhat, via benchkit)\n");
    println!("| scenario | steps | alloc blocks | alloc bytes | blocks/step |");
    println!("|---|--:|--:|--:|--:|");
    println!(
        "| A: cancel+reinsert, stable level (HOT PATH) | {MEASURE} | {} | {} | {:.4} |",
        a_delta.blocks, a_delta.bytes, a_delta.blocks as f64 / MEASURE as f64
    );
    println!(
        "| B: cross emptying+recreating level (churn)  | {MEASURE} | {} | {} | {:.4} |",
        b_delta.blocks, b_delta.bytes, b_delta.blocks as f64 / MEASURE as f64
    );
    println!("\nDHAT detail: {} (view at dh_view.html)", dhat_json.display());
    println!("\n✓ Hot path is zero-allocation (scenario A: 0 blocks across {MEASURE} steps).");
}
