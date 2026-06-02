//! HDR-histogram benchmarks for the `matcher` Engine, on the `benchkit` harness.
//!
//! Run with: `cargo bench -p matcher --bench matcher_bench`
//!
//! benchkit owns the mechanics (warmup/measure `rdtscp` fence, OS-noise capture,
//! `RunEnv` detection, markdown/CSV/HDR rendering + file output). This file owns
//! only what is matcher-specific: the scenario bodies (expressed as benchkit
//! prepare/op pairs) and the report composition (section grouping + the headline
//! deltas). The fence placement was proven identical to the prior hand-rolled
//! loop by `matcher_equiv` (since deleted).
//!
//! Headline numbers this file surfaces:
//!   1. Engine-wrapper overhead: `rest_new_level` vs `raw_book_insert` (same
//!      setup, no Engine) = validation + hashmap + mint + dispatch + event emit.
//!   2. Per-level marginal cross cost: slope of `cross_n` over n in {1, 4, 16}.
//!   3. FOK pre-check cost: `fok_success_n` − `cross_n` (happy-path overhead);
//!      `fok_fail_deep` − `fok_fail_fast` (cost of the O(L) liquidity walk).
//!
//! Single-thread, single-core, isolated-op latency only — no contention, no
//! multi-shard, no NIC path. Benches run in release, so the debug-only
//! `assert_invariants` is compiled out and does not contaminate timings.

use benchkit::{Delta, Iters, Lcg, Report, RunEnv, Runner, Sample};
use matcher::Engine;
use types::{IntentHash, OrderEvent, OrderRequest, OrderType, RequestType, Side, TimeInForce};

const INSTR: u32 = 1;
const STD: Iters = Iters::STANDARD; // 100k / 1M — cheap ops
const CROSS: Iters = Iters::LIGHT; // 50k / 500k — ops with per-iter setup
const EVENT_BUF_CAP: usize = 128;

// --- domain fixtures --------------------------------------------------------

fn hash_from_seed(seed: u64) -> IntentHash {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&seed.to_ne_bytes());
    IntentHash(hash)
}

fn req(side: Side, price: u64, qty: u64, tif: TimeInForce, rtype: RequestType, seed: u64) -> OrderRequest {
    OrderRequest {
        price,
        quantity: qty,
        origin_ts: 0,
        instrument_id: INSTR,
        side,
        order_type: OrderType::Limit,
        tif,
        request_type: rtype,
        intent_hash: hash_from_seed(seed),
    }
}

fn cancel_req(hash: IntentHash) -> OrderRequest {
    let mut r = req(Side::Bid, 0, 0, TimeInForce::GTC, RequestType::Cancel, 0);
    r.intent_hash = hash;
    r
}

fn new_engine(capacity: u32) -> Engine {
    let mut e = Engine::new();
    e.add_instrument(INSTR, capacity).expect("add_instrument");
    e
}

/// Shared scenario state: an engine and a reused, pre-sized event buffer.
struct Book {
    engine: Engine,
    buf: Vec<OrderEvent>,
}

impl Book {
    fn new(capacity: u32) -> Self {
        Self { engine: new_engine(capacity), buf: Vec::with_capacity(EVENT_BUF_CAP) }
    }
    /// Untimed processing (setup/replenish/cleanup).
    fn process(&mut self, r: &OrderRequest) {
        self.buf.clear();
        self.engine.process(r, &mut self.buf);
    }
}

// --- scenarios (matcher-specific; mechanics supplied by benchkit) -----------

// rest_new_level: all bids at distinct ascending prices, no asks → never
// crosses, always rests at a brand-new level.
fn rest_new_level(r: &Runner) -> Sample {
    let cap = (STD.total() as u32).next_power_of_two();
    let mut book = Book::new(cap);
    r.bench(
        "rest_new_level",
        STD,
        &mut book,
        |_b, i| req(Side::Bid, i as u64 + 1, 100, TimeInForce::GTC, RequestType::New, i as u64),
        |b, o| {
            b.buf.clear();
            b.engine.process(&o, &mut b.buf);
        },
    )
}

// rest_existing_level: all bids at ONE price, no asks → deepening single level.
fn rest_existing_level(r: &Runner) -> Sample {
    const PRICE: u64 = 1_000_000;
    let cap = (STD.total() as u32).next_power_of_two();
    let mut book = Book::new(cap);
    r.bench(
        "rest_existing_level",
        STD,
        &mut book,
        |_b, i| req(Side::Bid, PRICE, 100, TimeInForce::GTC, RequestType::New, i as u64),
        |b, o| {
            b.buf.clear();
            b.engine.process(&o, &mut b.buf);
        },
    )
}

// cross_n: each iter rests `n` asks (untimed prepare), then times one taker that
// crosses all n. IOC fully fills (pure cross); FOK adds the liquidity pre-check,
// so fok_success_n − cross_n is the happy-path pre-check overhead.
fn cross(r: &Runner, label: &str, n: u64, tif: TimeInForce) -> Sample {
    struct St { book: Book, seed: u64 }
    let mut st = St { book: Book::new(65_536), seed: 0 };
    r.bench(
        label,
        CROSS,
        &mut st,
        |st, _i| {
            for lvl in 0..n {
                let ask = req(Side::Ask, 100 + lvl, 1, TimeInForce::GTC, RequestType::New, st.seed);
                st.seed += 1;
                st.book.process(&ask);
            }
            let taker = req(Side::Bid, 100 + n, n, tif, RequestType::New, st.seed);
            st.seed += 1;
            taker
        },
        |st, taker| {
            st.book.buf.clear();
            st.book.engine.process(&taker, &mut st.book.buf);
        },
    )
}

// fok_fail_fast: asks priced far above the taker → FOK pre-check bails on level 1.
// Static book (FOK fail never mutates), so every measured op is identical.
fn fok_fail_fast(r: &Runner) -> Sample {
    let mut book = Book::new(4096);
    for lvl in 0..16u64 {
        book.process(&req(Side::Ask, 1000 + lvl, 1, TimeInForce::GTC, RequestType::New, lvl));
    }
    r.bench_static("fok_fail_fast", STD, &mut book, |b| {
        let taker = req(Side::Bid, 100, 10, TimeInForce::FOK, RequestType::New, 1_000);
        b.buf.clear();
        b.engine.process(&taker, &mut b.buf);
    })
}

// fok_fail_deep: L crossing levels totaling Q units; FOK needs Q+1 → pre-check
// walks ALL L levels, sums short, rejects. Isolates the O(L) walk. Static book.
fn fok_fail_deep(r: &Runner) -> Sample {
    const L: u64 = 16;
    let mut book = Book::new(4096);
    for lvl in 0..L {
        book.process(&req(Side::Ask, 100 + lvl, 1, TimeInForce::GTC, RequestType::New, lvl));
    }
    r.bench_static("fok_fail_deep", STD, &mut book, |b| {
        let taker = req(Side::Bid, 100 + L, L + 1, TimeInForce::FOK, RequestType::New, 1_000);
        b.buf.clear();
        b.engine.process(&taker, &mut b.buf);
    })
}

// ioc_no_liquidity: IOC taker, empty opposing side → single Reject, no mutation.
// The reject floor: validate + dispatch + one failed peek + reject emit.
fn ioc_no_liquidity(r: &Runner) -> Sample {
    let mut book = Book::new(64);
    r.bench_static("ioc_no_liquidity", STD, &mut book, |b| {
        let taker = req(Side::Bid, 100, 10, TimeInForce::IOC, RequestType::New, 1);
        b.buf.clear();
        b.engine.process(&taker, &mut b.buf);
    })
}

// cancel_hit: deep book, cancel in shuffled order so each cancel hits a random
// position in its level's VecDeque (the O(level-depth) scan). Pre-generated
// inputs: the i-th measured op cancels hashes[i].
fn cancel_hit(r: &Runner) -> Sample {
    const LEVEL_DEPTH: usize = 50;
    let num_levels = STD.total() / LEVEL_DEPTH + 1;
    let n = num_levels * LEVEL_DEPTH;
    let cap = (n as u32).next_power_of_two();
    let mut book = Book::new(cap);

    let mut hashes = Vec::with_capacity(n);
    let mut seed: u64 = 0;
    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for _ in 0..LEVEL_DEPTH {
            let o = req(Side::Bid, price, 100, TimeInForce::GTC, RequestType::New, seed);
            hashes.push(o.intent_hash);
            book.process(&o);
            seed += 1;
        }
    }
    Lcg::new(0xCAFE_F00D).shuffle(&mut hashes);

    r.bench(
        "cancel_hit",
        STD,
        &mut book,
        |_b, i| cancel_req(hashes[i]),
        |b, c| {
            b.buf.clear();
            b.engine.process(&c, &mut b.buf);
        },
    )
}

// cancel_miss: ~30K resting orders for a realistic index, then cancel hashes from
// a disjoint seed range → always UnknownOrder. Misses don't mutate; static op.
fn cancel_miss(r: &Runner) -> Sample {
    let mut book = Book::new(65_536);
    for seed in 0..30_000u64 {
        book.process(&req(Side::Bid, 100 + (seed % 100), 100, TimeInForce::GTC, RequestType::New, seed));
    }
    let mut miss_seed: u64 = 1_000_000_000; // disjoint from inserted seeds
    r.bench_static("cancel_miss", STD, &mut book, move |b| {
        let c = cancel_req(hash_from_seed(miss_seed));
        miss_seed += 1;
        b.buf.clear();
        b.engine.process(&c, &mut b.buf);
    })
}

// partial_then_rest: GTC taker crosses one resting maker AND rests the remainder
// (combined match+insert). prepare rests one ask; op times the taker; the rested
// remainder is cancelled in the NEXT prepare so the book returns to empty.
fn partial_then_rest(r: &Runner) -> Sample {
    struct St { book: Book, seed: u64, last_taker: Option<u64> }
    let mut st = St { book: Book::new(65_536), seed: 0, last_taker: None };
    r.bench(
        "partial_then_rest",
        CROSS,
        &mut st,
        |st, _i| {
            // cleanup previous iter's rested remainder (untimed).
            if let Some(prev) = st.last_taker.take() {
                st.book.process(&cancel_req(hash_from_seed(prev)));
            }
            // setup: one ask @100 qty1 rests (bid side empty).
            let ask = req(Side::Ask, 100, 1, TimeInForce::GTC, RequestType::New, st.seed);
            st.seed += 1;
            st.book.process(&ask);
            // taker bid @100 qty2: fills 1, rests 1.
            let taker_seed = st.seed;
            st.seed += 1;
            st.last_taker = Some(taker_seed);
            req(Side::Bid, 100, 2, TimeInForce::GTC, RequestType::New, taker_seed)
        },
        |st, taker| {
            st.book.buf.clear();
            st.book.engine.process(&taker, &mut st.book.buf);
        },
    )
}

// realistic_workload: mixed stream around a mid — 60% passive GTC rest, 30%
// cancel of a random active order, 10% aggressive IOC sweeping ~3 levels. Book
// hovers at steady depth; the mixed 1/mean is the representative sustained M/s.
fn realistic_workload(r: &Runner) -> Sample {
    const MID: u64 = 1_000;
    const MAX_ACTIVE: usize = 80_000;
    struct St { book: Book, active: Vec<IntentHash>, seed: u64, rng: Lcg }
    let mut st = St {
        book: Book::new(131_072),
        active: Vec::with_capacity(MAX_ACTIVE),
        seed: 0,
        rng: Lcg::new(0x9E37_79B9),
    };

    // Pre-fill ~30K passive orders straddling the mid.
    for _ in 0..30_000 {
        let rv = st.rng.next_u64();
        let (side, price) = if rv & 1 == 0 {
            (Side::Bid, MID - 1 - (rv >> 1) % 50)
        } else {
            (Side::Ask, MID + 1 + (rv >> 1) % 50)
        };
        let o = req(side, price, 1, TimeInForce::GTC, RequestType::New, st.seed);
        st.active.push(o.intent_hash);
        st.book.process(&o);
        st.seed += 1;
    }

    r.bench(
        "realistic_workload",
        STD,
        &mut st,
        |st, _i| {
            let pick = st.rng.next_u64() % 100;
            if pick < 60 && st.active.len() < MAX_ACTIVE {
                let rv = st.rng.next_u64();
                let (side, price) = if rv & 1 == 0 {
                    (Side::Bid, MID - 1 - (rv >> 1) % 50)
                } else {
                    (Side::Ask, MID + 1 + (rv >> 1) % 50)
                };
                let o = req(side, price, 1, TimeInForce::GTC, RequestType::New, st.seed);
                st.seed += 1;
                st.active.push(o.intent_hash);
                o
            } else if pick < 90 && !st.active.is_empty() {
                let idx = (st.rng.next_u64() as usize) % st.active.len();
                cancel_req(st.active.swap_remove(idx))
            } else {
                let buy = st.rng.next_u64() & 1 == 0;
                let (side, price) = if buy { (Side::Bid, MID + 1000) } else { (Side::Ask, 1) };
                let o = req(side, price, 3, TimeInForce::IOC, RequestType::New, st.seed);
                st.seed += 1;
                o
            }
        },
        |st, request| {
            st.book.buf.clear();
            st.book.engine.process(&request, &mut st.book.buf);
        },
    )
}

// raw_book_insert: same setup as rest_new_level but against OrderBook directly
// (no Engine). rest_new_level.p50 − raw_book_insert.p50 = the Engine overhead.
fn raw_book_insert(r: &Runner) -> Sample {
    use orderbook::OrderBook;
    use types::{Order, OrderId};

    let cap = (STD.total() as u32).next_power_of_two();
    let mut book = OrderBook::new(INSTR, cap);
    let mk = |seed: u64| -> Order {
        Order {
            order_id: OrderId(seed),
            price: seed + 1,
            quantity: 100,
            origin_ts: 0,
            instrument_id: INSTR,
            side: Side::Bid,
            order_type: OrderType::Limit,
            tif: TimeInForce::GTC,
            _padding: 0,
            intent_hash: hash_from_seed(seed),
        }
    };
    r.bench(
        "raw_book_insert",
        STD,
        &mut book,
        |_b, i| mk(i as u64),
        |b, o| {
            b.insert(o).expect("arena capacity");
        },
    )
}

fn main() -> std::io::Result<()> {
    let runner = Runner::new();
    let env = RunEnv::detect(runner.calibration());

    // Run every scenario.
    let rest_new = rest_new_level(&runner);
    let rest_exist = rest_existing_level(&runner);
    let raw = raw_book_insert(&runner);
    let cross1 = cross(&runner, "cross_n=1", 1, TimeInForce::IOC);
    let cross4 = cross(&runner, "cross_n=4", 4, TimeInForce::IOC);
    let cross16 = cross(&runner, "cross_n=16", 16, TimeInForce::IOC);
    let fok1 = cross(&runner, "fok_success_n=1", 1, TimeInForce::FOK);
    let fok4 = cross(&runner, "fok_success_n=4", 4, TimeInForce::FOK);
    let fok16 = cross(&runner, "fok_success_n=16", 16, TimeInForce::FOK);
    let fok_fast = fok_fail_fast(&runner);
    let fok_deep = fok_fail_deep(&runner);
    let ioc_dry = ioc_no_liquidity(&runner);
    let c_hit = cancel_hit(&runner);
    let c_miss = cancel_miss(&runner);
    let partial = partial_then_rest(&runner);
    let realistic = realistic_workload(&runner);

    // Compose the report.
    let scope = "engine-internal `process()` (matcher + orderbook + arena); \
                 includes event emit into the Vec, excludes wire decode & transport.";
    let mut report = Report::new(&env, "Matcher engine benchmark", scope);
    report
        .section("Resting (no cross)", &[&rest_new, &rest_exist])
        .section("Reference — raw orderbook (no Engine)", &[&raw])
        .section("Crossing", &[&cross1, &cross4, &cross16])
        .section("FOK gate", &[&fok1, &fok4, &fok16, &fok_fast, &fok_deep])
        .section("Reject floor", &[&ioc_dry])
        .section("Cancel", &[&c_hit, &c_miss])
        .section("Combined / composite", &[&partial, &realistic])
        .delta("Engine wrapper overhead", &rest_new, &raw, Delta::Sub, "rest_new_level − raw_book_insert")
        .delta("Marginal cost / level swept", &cross16, &cross1, Delta::SubPerN(15), "(cross_n=16 − cross_n=1) / 15")
        .delta("FOK pre-check overhead (16 lvl)", &fok16, &cross16, Delta::Sub, "fok_success_n=16 − cross_n=16")
        .delta("FOK liquidity walk (16 lvl)", &fok_deep, &fok_fast, Delta::Sub, "fok_fail_deep − fok_fail_fast");

    let paths = report.write_run("bench-runs", "matcher")?;
    eprintln!(
        "benchmark run written:\n  {}\n  {}\n  {}/*.hdr",
        paths.markdown.display(),
        paths.csv.display(),
        paths.hdr_dir.display()
    );
    Ok(())
}
