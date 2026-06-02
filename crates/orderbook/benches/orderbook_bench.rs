//! OrderBook benchmarks, on the `benchkit` harness.
//!
//! Run with: `cargo bench -p orderbook --bench orderbook_bench`
//!
//! benchkit supplies the warmup/measure `rdtscp` fence, env detection, and
//! report rendering; this file supplies only the scenarios.
//!
//! The headline comparison is `cancel_oldest` vs `cancel_random`: identical
//! workload except cancel order, isolating the cost of the O(level-depth) scan
//! in the current VecDeque-based cancel path. `cancel_random` uses the same
//! `Lcg` seed (0xCAFE_F00D) as the matcher's `cancel_hit` so the access patterns
//! line up for cross-crate subtraction.

use benchkit::{Iters, Lcg, Report, RunEnv, Runner, Sample};
use std::hint::black_box;

use orderbook::OrderBook;
use types::{IntentHash, Order, OrderId, OrderType, Side, TimeInForce};

const STD: Iters = Iters::STANDARD;
const LEVEL_DEPTH: usize = 50;

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

// insert_new_level: each insert creates a brand-new price level (distinct,
// ascending prices). Measures arena alloc + BTreeMap node insert + HashMap insert.
fn insert_new_level(r: &Runner) -> Sample {
    let cap = (STD.total() as u32).next_power_of_two();
    let mut book = OrderBook::new(1, cap);
    r.bench(
        "insert_new_level",
        STD,
        &mut book,
        |_b, i| make_order(i as u64 + 1, 100, Side::Bid, i as u64),
        |b, o| b.insert(o).expect("arena capacity"),
    )
}

// insert_existing_level: all inserts hit the same price → same level. Measures
// the steady-state "level already exists" path: entry().or_default() + push_back.
fn insert_existing_level(r: &Runner) -> Sample {
    const PRICE: u64 = 100;
    let cap = (STD.total() as u32).next_power_of_two();
    let mut book = OrderBook::new(1, cap);
    r.bench(
        "insert_existing_level",
        STD,
        &mut book,
        |_b, i| make_order(PRICE, 100, Side::Bid, i as u64),
        |b, o| b.insert(o).expect("arena capacity"),
    )
}

// pop_top: pre-populate deep levels; time one pop per iter. Steady-state — each
// prepare replenishes one order so the book never empties across 1M+ pops. (The
// replenish inserts at a far price so it never becomes the popped top.)
fn pop_top(r: &Runner) -> Sample {
    struct St { book: OrderBook, seed: u64 }
    // Pre-fill enough depth at low prices that pops keep finding a best bid.
    let mut st = St { book: OrderBook::new(1, 65_536), seed: 0 };
    // Seed a block of resting bids spread across levels so there is always a top.
    for _ in 0..(LEVEL_DEPTH * 100) {
        let price = 1 + (st.seed % 100);
        st.book.insert(make_order(price, 100, Side::Bid, st.seed)).unwrap();
        st.seed += 1;
    }
    r.bench(
        "pop_top",
        STD,
        &mut st,
        // Replenish one bid (untimed) at a price below the current tops so the
        // book stays at steady depth and the timed pop always succeeds.
        |st, _i| {
            let price = 1 + (st.seed % 100);
            st.book.insert(make_order(price, 100, Side::Bid, st.seed)).unwrap();
            st.seed += 1;
        },
        |st, ()| black_box(st.book.pop_top(Side::Bid)),
    )
}

/// Build a book of `n` resting bids across `n/LEVEL_DEPTH` levels and return the
/// book plus the inserted hashes (in insertion order).
fn build_deep_book(n: usize) -> (OrderBook, Vec<IntentHash>) {
    let cap = (n as u32).next_power_of_two();
    let mut book = OrderBook::new(1, cap);
    let mut hashes = Vec::with_capacity(n);
    let num_levels = n / LEVEL_DEPTH;
    let mut seed = 0u64;
    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for _ in 0..LEVEL_DEPTH {
            let o = make_order(price, 100, Side::Bid, seed);
            hashes.push(o.intent_hash);
            book.insert(o).unwrap();
            seed += 1;
        }
    }
    (book, hashes)
}

// cancel_oldest: cancel in insertion (FIFO) order → each cancel hits position 0
// of its level's VecDeque; iter().position() returns immediately. Best case.
fn cancel_oldest(r: &Runner) -> Sample {
    let n = STD.total() + LEVEL_DEPTH;
    let (book, hashes) = build_deep_book(n);
    let mut book = book; // no shuffle: insertion order == FIFO
    r.bench(
        "cancel_oldest",
        STD,
        &mut book,
        |_b, i| hashes[i],
        |b, h| black_box(b.cancel(&h)),
    )
}

// cancel_random: same setup, shuffled cancel order → hits random positions in the
// level, so iter().position() averages O(LEVEL_DEPTH/2). The decision-relevant
// bench; cancel_random.p50 / cancel_oldest.p50 is the scan-cost ratio.
fn cancel_random(r: &Runner) -> Sample {
    let n = STD.total() + LEVEL_DEPTH;
    let (book, mut hashes) = build_deep_book(n);
    let mut book = book;
    Lcg::new(0xCAFE_F00D).shuffle(&mut hashes);
    r.bench(
        "cancel_random",
        STD,
        &mut book,
        |_b, i| hashes[i],
        |b, h| black_box(b.cancel(&h)),
    )
}

// realistic_workload: mixed 60% insert / 40% cancel across 100 levels, steady
// state around ~30K resting orders. Single mixed histogram.
fn realistic_workload(r: &Runner) -> Sample {
    const NUM_LEVELS: u64 = 100;
    struct St { book: OrderBook, active: Vec<IntentHash>, seed: u64, rng: Lcg }
    let mut st = St {
        book: OrderBook::new(1, 65_536),
        active: Vec::with_capacity(50_000),
        seed: 0,
        rng: Lcg::new(0x9E37_79B9),
    };
    for _ in 0..30_000 {
        let price = 100 + (st.seed % NUM_LEVELS);
        let o = make_order(price, 100, Side::Bid, st.seed);
        st.active.push(o.intent_hash);
        st.book.insert(o).unwrap();
        st.seed += 1;
    }

    // The op is chosen inside prepare and returned as a tagged action so the
    // timed closure does exactly one book call. Action carries what to do.
    enum Act { Insert(Order), Cancel(IntentHash) }
    r.bench(
        "realistic_workload",
        STD,
        &mut st,
        |st, _i| {
            let do_insert = (st.rng.next_u64() % 100) < 60 && st.active.len() < 50_000;
            if do_insert || st.active.is_empty() {
                let price = 100 + (st.rng.next_u64() % NUM_LEVELS);
                let o = make_order(price, 100, Side::Bid, st.seed);
                st.seed += 1;
                st.active.push(o.intent_hash);
                Act::Insert(o)
            } else {
                let idx = (st.rng.next_u64() as usize) % st.active.len();
                Act::Cancel(st.active.swap_remove(idx))
            }
        },
        |st, act| match act {
            Act::Insert(o) => {
                st.book.insert(o).unwrap();
            }
            Act::Cancel(h) => {
                black_box(st.book.cancel(&h));
            }
        },
    )
}

fn main() -> std::io::Result<()> {
    let runner = Runner::new();
    let env = RunEnv::detect(runner.calibration());

    let ins_new = insert_new_level(&runner);
    let ins_exist = insert_existing_level(&runner);
    let pop = pop_top(&runner);
    let c_oldest = cancel_oldest(&runner);
    let c_random = cancel_random(&runner);
    let realistic = realistic_workload(&runner);

    let scope = "single OrderBook op (insert / pop_top / cancel); \
                 BTreeMap levels + VecDeque orders + arena slots, no Engine, no I/O.";
    let mut report = Report::new(&env, "OrderBook benchmark", scope);
    report
        .section("Insert", &[&ins_new, &ins_exist])
        .section("Consume (matcher hot path)", &[&pop])
        .section("Cancel (scan-cost comparison)", &[&c_oldest, &c_random])
        .section("Composite", &[&realistic]);

    // Headline: the scan-cost ratio that decides the linked-list optimization.
    // Reported as a delta (random − oldest) in ns; the ratio is read from p50s.
    report.delta(
        "Cancel scan cost (random − oldest)",
        &c_random,
        &c_oldest,
        benchkit::Delta::Sub,
        "extra p50 ns from O(depth/2) VecDeque scan vs FIFO hit",
    );

    let paths = report.write_run("bench-runs", "orderbook")?;
    eprintln!(
        "benchmark run written:\n  {}\n  {}\n  {}/*.hdr",
        paths.markdown.display(),
        paths.csv.display(),
        paths.hdr_dir.display()
    );
    Ok(())
}
