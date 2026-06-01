//! Heap-allocation proof for the matcher hot path, via `dhat`.
//!
//! Run with: `cargo run --release --example alloc_proof -p matcher`
//!
//! `dhat` installs a global allocator shim that records every allocation, so
//! this is a SEPARATE binary from the latency bench — the shim perturbs timing
//! and must never run during `rdtscp` measurement.
//!
//! What this proves: `Engine::process` does not allocate on the steady-state
//! matching path *as long as it neither creates nor destroys a price level and
//! the event buffer does not grow*. We construct exactly that workload —
//! cancel + re-insert at a single deep, pre-warmed level — and assert the
//! allocation delta across the measured window is zero.
//!
//! Known non-zero-alloc paths (out of scope for this proof, documented in the
//! orderbook LLD): emptying a level frees its BTreeMap node + VecDeque, and
//! re-creating it allocates again (there is a `todo: cache the removed level`).
//! Growing a level's VecDeque past its capacity also allocates. So a workload
//! that churns levels WILL allocate; that is expected and measured separately
//! below for honesty.

use dhat::HeapStats;
use matcher::Engine;
use types::{IntentHash, OrderRequest, OrderType, RequestType, Side, TimeInForce};

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

/// Allocation delta (blocks, bytes) of running `body`, measured via dhat.
fn alloc_delta(body: impl FnOnce()) -> (u64, u64) {
    let before = HeapStats::get();
    body();
    let after = HeapStats::get();
    (
        after.total_blocks - before.total_blocks,
        after.total_bytes - before.total_bytes,
    )
}

fn main() {
    // Normal (non-testing) profiler: writes dhat-heap.json on drop for the
    // DHAT viewer at https://nnethercote.github.io/dh_view/dh_view.html
    let _profiler = dhat::Profiler::builder()
        .file_name("bench-runs/alloc_proof_dhat-heap.json")
        .build();

    const PRICE: u64 = 1_000;
    const DEPTH: u64 = 256; // level depth to pre-warm the VecDeque capacity
    const ITERS: u64 = 500_000;

    // ----- Scenario A: zero-alloc steady state -----------------------------
    // One bid level at PRICE, pre-filled to DEPTH so its VecDeque/HashMap are
    // warmed. Then cancel the oldest + re-insert a fresh order at the SAME
    // price, ITERS times. The level never empties (depth stays >= DEPTH-1) and
    // capacities never grow, so process() should allocate nothing.
    let (blocks_a, bytes_a) = {
        let mut e = engine();
        let mut buf = Vec::with_capacity(64);
        let mut seed: u64 = 0;

        for _ in 0..DEPTH {
            buf.clear();
            e.process(&new_req(Side::Bid, PRICE, 100, seed), &mut buf);
            seed += 1;
        }
        // warm: one cancel+insert cycle so any first-touch growth happens now.
        for _ in 0..1000 {
            buf.clear();
            let cancel_seed = seed - DEPTH; // oldest still-resting order
            e.process(&cancel_req(cancel_seed), &mut buf);
            buf.clear();
            e.process(&new_req(Side::Bid, PRICE, 100, seed), &mut buf);
            seed += 1;
        }

        alloc_delta(|| {
            for _ in 0..ITERS {
                buf.clear();
                let cancel_seed = seed - DEPTH;
                e.process(&cancel_req(cancel_seed), &mut buf);
                buf.clear();
                e.process(&new_req(Side::Bid, PRICE, 100, seed), &mut buf);
                seed += 1;
            }
        })
    };

    // ----- Scenario B: level-churn (expected to allocate) ------------------
    // Cross a single resting maker, which EMPTIES its level (freeing the
    // BTreeMap node + VecDeque), then replenish (re-creating it). This is the
    // documented alloc-churn path — reported, not asserted, for honesty.
    let (blocks_b, bytes_b) = {
        let mut e = engine();
        let mut buf = Vec::with_capacity(64);
        let mut seed: u64 = 0;
        // warm
        for _ in 0..1000 {
            buf.clear();
            e.process(&new_req(Side::Ask, PRICE, 1, seed), &mut buf);
            seed += 1;
            buf.clear();
            let mut taker = new_req(Side::Bid, PRICE, 1, seed);
            taker.tif = TimeInForce::IOC;
            e.process(&taker, &mut buf);
            seed += 1;
        }
        alloc_delta(|| {
            for _ in 0..ITERS {
                buf.clear();
                e.process(&new_req(Side::Ask, PRICE, 1, seed), &mut buf);
                seed += 1;
                buf.clear();
                let mut taker = new_req(Side::Bid, PRICE, 1, seed);
                taker.tif = TimeInForce::IOC;
                e.process(&taker, &mut buf);
                seed += 1;
            }
        })
    };

    // ----- Report ----------------------------------------------------------
    let ops_a = ITERS * 2; // cancel + insert per iter
    let ops_b = ITERS * 2; // insert(ask) + cross(bid) per iter
    println!("# Matcher allocation proof (dhat)\n");
    println!("| scenario | ops | alloc blocks | alloc bytes | blocks/op |");
    println!("|---|--:|--:|--:|--:|");
    println!(
        "| A: cancel+reinsert, stable level (HOT PATH) | {ops_a} | {blocks_a} | {bytes_a} | {:.4} |",
        blocks_a as f64 / ops_a as f64
    );
    println!(
        "| B: cross emptying+recreating level (churn)  | {ops_b} | {blocks_b} | {bytes_b} | {:.4} |",
        blocks_b as f64 / ops_b as f64
    );
    println!("\nDHAT detail: bench-runs/alloc_proof_dhat-heap.json (view at dh_view.html)");

    assert_eq!(
        blocks_a, 0,
        "HOT PATH REGRESSION: steady-state cancel+reinsert allocated {blocks_a} blocks \
         ({bytes_a} bytes) — process() must be zero-alloc when no level is created/destroyed"
    );
    println!("\n✓ Hot path is zero-allocation (scenario A: 0 blocks across {ops_a} ops).");
}
