//! Hand-rolled HDR-histogram benchmarks for the OrderBook crate.
//!
//! Run with: `cargo bench -p orderbook --bench orderbook_bench`
//!
//! Each bench uses explicit warmup + measure phases for clean p50/p99
//! percentiles. Numbers are uncontaminated by criterion warmup but
//! unstable on a non-isolated machine — interpret p50 first, treat
//! max as noise.
//!
//! The headline comparison is `cancel_oldest` vs `cancel_random`:
//! identical workload except cancel order, isolating the cost of the
//! O(level depth) scan in the current VecDeque-based cancel path.

use orderbook::OrderBook;
use telemetry::primitives::{calibrate, rdtscp, rdtscp_then_lfence, Histogram, TscCalibration};
use types::{IntentHash, Order, OrderId, OrderType, Side, TimeInForce};

const WARMUP_ITERS: usize = 100_000;
const MEASURE_ITERS: usize = 1_000_000;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn make_order(price: u64, qty: u64, side: Side, hash_seed: u64) -> Order {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&hash_seed.to_ne_bytes());
    Order {
        order_id: OrderId(hash_seed),
        price,
        quantity: qty,
        origin_ts: 0,
        instrument_id: 1,
        side,
        order_type: OrderType::Limit,
        tif: TimeInForce::GTC,
        _padding: 0,
        intent_hash: IntentHash(hash),
    }
}

/// Fisher-Yates shuffle with an LCG. Deterministic across runs; no rand
/// crate needed. Knuth's LCG constants (Numerical Recipes).
fn deterministic_shuffle<T>(slice: &mut [T], seed: u64) {
    let mut state = seed;
    let n = slice.len();
    for i in (1..n).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        slice.swap(i, j);
    }
}

// ---------------------------------------------------------------------------
// Bench 1 — insert_new_level
// Each insert creates a brand-new price level (distinct prices).
// Measures: arena alloc + BTreeMap node insert + HashMap insert.
// ---------------------------------------------------------------------------

fn bench_insert_new_level(cal: &TscCalibration) {
    let capacity = ((WARMUP_ITERS + MEASURE_ITERS) as u32).next_power_of_two();
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("insert_new_level");

    for i in 0..WARMUP_ITERS {
        let order = make_order(i as u64 + 1, 100, Side::Bid, i as u64);
        std::hint::black_box(book.insert(std::hint::black_box(order))).unwrap();
    }

    for i in 0..MEASURE_ITERS {
        let order = make_order(
            (WARMUP_ITERS + i) as u64 + 1,
            100,
            Side::Bid,
            (WARMUP_ITERS + i) as u64,
        );
        let order = std::hint::black_box(order);

        let t0 = rdtscp_then_lfence();
        let idx = book.insert(order).unwrap();
        let t1 = rdtscp();
        std::hint::black_box(idx);

        hist.record(t1.since(t0).to_nanos(cal));
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------
// Bench 2 — insert_existing_level
// All inserts hit the same price → same level. Measures the steady-state
// "level already exists" path: entry().or_default() + VecDeque push_back.
// ---------------------------------------------------------------------------

fn bench_insert_existing_level(cal: &TscCalibration) {
    let capacity = ((WARMUP_ITERS + MEASURE_ITERS) as u32).next_power_of_two();
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("insert_existing_level");

    const PRICE: u64 = 100;

    for i in 0..WARMUP_ITERS {
        book.insert(make_order(PRICE, 100, Side::Bid, i as u64)).unwrap();
    }

    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let order = std::hint::black_box(make_order(PRICE, 100, Side::Bid, i as u64));
        let t0 = rdtscp_then_lfence();
        let idx = book.insert(order).unwrap();
        let t1 = rdtscp();
        std::hint::black_box(idx);
        hist.record(t1.since(t0).to_nanos(cal));
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------
// Bench 3 — pop_top
// Pre-populate with deep levels, pop the head repeatedly.
// Measures the matcher's full-consumption hot path.
// ---------------------------------------------------------------------------

fn bench_pop_top(cal: &TscCalibration) {
    const LEVEL_DEPTH: usize = 50;
    let num_levels = (WARMUP_ITERS + MEASURE_ITERS) / LEVEL_DEPTH + 1;
    let n = num_levels * LEVEL_DEPTH;
    let capacity = (n as u32).next_power_of_two();
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("pop_top");

    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for ord_idx in 0..LEVEL_DEPTH {
            let seed = (level_idx * LEVEL_DEPTH + ord_idx) as u64;
            book.insert(make_order(price, 100, Side::Bid, seed)).unwrap();
        }
    }

    for _ in 0..WARMUP_ITERS {
        book.pop_top(Side::Bid).unwrap();
    }

    for _ in 0..MEASURE_ITERS {
        let t0 = rdtscp_then_lfence();
        let popped = book.pop_top(Side::Bid);
        let t1 = rdtscp();
        std::hint::black_box(popped);
        hist.record(t1.since(t0).to_nanos(cal));
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------
// Bench 4 — cancel_oldest (best-case cancel)
// FIFO cancel order: each cancel hits position 0 in its level's VecDeque,
// so iter().position() returns immediately. Best case for the current
// scan-based cancel implementation.
// ---------------------------------------------------------------------------

fn bench_cancel_oldest(cal: &TscCalibration) {
    const LEVEL_DEPTH: usize = 50;
    let num_levels = (WARMUP_ITERS + MEASURE_ITERS) / LEVEL_DEPTH + 1;
    let n = num_levels * LEVEL_DEPTH;
    let capacity = (n as u32).next_power_of_two();
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("cancel_oldest");

    let mut hashes = Vec::with_capacity(n);
    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for ord_idx in 0..LEVEL_DEPTH {
            let seed = (level_idx * LEVEL_DEPTH + ord_idx) as u64;
            let order = make_order(price, 100, Side::Bid, seed);
            hashes.push(order.intent_hash);
            book.insert(order).unwrap();
        }
    }
    // No shuffle — cancels in insertion order = FIFO order within each level.

    for i in 0..WARMUP_ITERS {
        book.cancel(&hashes[i]).unwrap();
    }

    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let hash = std::hint::black_box(hashes[i]);
        let t0 = rdtscp_then_lfence();
        let cancelled = book.cancel(&hash);
        let t1 = rdtscp();
        std::hint::black_box(cancelled);
        hist.record(t1.since(t0).to_nanos(cal));
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------
// Bench 5 — cancel_random (the decision-relevant bench)
// Same setup as cancel_oldest but cancels happen in shuffled order,
// hitting random positions in the level. The iter().position() scan
// averages O(LEVEL_DEPTH / 2).
//
// The headline ratio: cancel_random.p50 / cancel_oldest.p50
// Small ratio (≤2×) → linked-list optimization not justified yet.
// Large ratio (3-5×+) → scan dominates, optimization is justified.
// ---------------------------------------------------------------------------

fn bench_cancel_random(cal: &TscCalibration) {
    const LEVEL_DEPTH: usize = 50;
    let num_levels = (WARMUP_ITERS + MEASURE_ITERS) / LEVEL_DEPTH + 1;
    let n = num_levels * LEVEL_DEPTH;
    let capacity = (n as u32).next_power_of_two();
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("cancel_random");

    let mut hashes = Vec::with_capacity(n);
    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for ord_idx in 0..LEVEL_DEPTH {
            let seed = (level_idx * LEVEL_DEPTH + ord_idx) as u64;
            let order = make_order(price, 100, Side::Bid, seed);
            hashes.push(order.intent_hash);
            book.insert(order).unwrap();
        }
    }
    deterministic_shuffle(&mut hashes, 0xCAFE_F00D);

    for i in 0..WARMUP_ITERS {
        book.cancel(&hashes[i]).unwrap();
    }

    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let hash = std::hint::black_box(hashes[i]);
        let t0 = rdtscp_then_lfence();
        let cancelled = book.cancel(&hash);
        let t1 = rdtscp();
        std::hint::black_box(cancelled);
        hist.record(t1.since(t0).to_nanos(cal));
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------
// Bench 6 — realistic_workload
// Mixed 60% insert / 40% cancel across 100 price levels.
// The single-histogram output mixes both op types — for separate
// insert/cancel percentiles, split into two histograms.
// ---------------------------------------------------------------------------

fn bench_realistic_workload(cal: &TscCalibration) {
    const NUM_LEVELS: u64 = 100;
    let capacity = 65_536u32;
    let mut book = OrderBook::new(1, capacity);
    let mut hist = Histogram::new("realistic_workload");

    let mut active_hashes: Vec<IntentHash> = Vec::with_capacity(50_000);
    let mut next_seed: u64 = 0;

    // Pre-fill to ~30K resting orders.
    for _ in 0..30_000 {
        let price = 100 + (next_seed % NUM_LEVELS);
        let order = make_order(price, 100, Side::Bid, next_seed);
        active_hashes.push(order.intent_hash);
        book.insert(order).unwrap();
        next_seed += 1;
    }

    let mut rng = next_seed.wrapping_mul(0x9E37_79B9);
    let mut roll = || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        rng
    };

    for i in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        let measure = i >= WARMUP_ITERS;
        let do_insert = (roll() % 100) < 60 && active_hashes.len() < 50_000;

        if do_insert || active_hashes.is_empty() {
            let price = 100 + (roll() % NUM_LEVELS);
            let order = make_order(price, 100, Side::Bid, next_seed);
            next_seed += 1;

            if measure {
                let t0 = rdtscp_then_lfence();
                book.insert(order).unwrap();
                let t1 = rdtscp();
                hist.record(t1.since(t0).to_nanos(cal));
            } else {
                book.insert(order).unwrap();
            }
            active_hashes.push(order.intent_hash);
        } else {
            let idx = (roll() as usize) % active_hashes.len();
            let hash = active_hashes.swap_remove(idx);
            if measure {
                let t0 = rdtscp_then_lfence();
                let r = book.cancel(&hash);
                let t1 = rdtscp();
                std::hint::black_box(r);
                hist.record(t1.since(t0).to_nanos(cal));
            } else {
                book.cancel(&hash);
            }
        }
    }

    println!("\n{}", hist.render_markdown());
}

// ---------------------------------------------------------------------------

fn main() {
    let cal = calibrate();

    bench_insert_new_level(&cal);
    bench_insert_existing_level(&cal);
    bench_pop_top(&cal);
    bench_cancel_oldest(&cal);
    bench_cancel_random(&cal);
    bench_realistic_workload(&cal);
}
