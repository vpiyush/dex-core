//! Hand-rolled HDR-histogram benchmarks for the `matcher` Engine — pass 1:
//! controlled latency scenarios.
//!
//! Run with: `cargo bench -p matcher --bench matcher_bench`
//!
//! Methodology mirrors `orderbook_bench.rs`: explicit warmup + measure phases,
//! `rdtscp` fences around *only* `Engine::process`, telemetry `Histogram` for
//! percentiles. The `msg/s` column is the derived `1/mean` single-core upper
//! bound (NOT sustained throughput). Interpret p50 first; treat max as noise on
//! a non-isolated machine.
//!
//! Benches run in release (`cargo bench`), so the debug-only `assert_invariants`
//! in `process` is compiled out and does not contaminate timings.
//!
//! Headline numbers this file is designed to surface:
//!   1. Engine-wrapper overhead: `rest_new_level` here vs orderbook's raw
//!      `insert_new_level` = validation + hashmap + mint + dispatch + event emit.
//!   2. Per-level marginal cross cost: slope of `cross_n` over n in {1, 4, 16}.
//!   3. FOK pre-check cost: `fok_success_n` − `cross_n` (happy-path overhead);
//!      `fok_fail_deep` − `fok_fail_fast` (cost of the O(L) liquidity walk).
//!
//! Single-thread, single-core, isolated-op latency only. No contention, no
//! multi-shard, no NIC path — those belong to the gateway/IPC benches.

use matcher::Engine;
use std::fmt::Write as _;
use telemetry::primitives::{calibrate, rdtscp, rdtscp_then_lfence, Histogram, TscCalibration};
use types::{IntentHash, OrderRequest, OrderType, RequestType, Side, TimeInForce};

const INSTR: u32 = 1;
const WARMUP_ITERS: usize = 100_000;
const MEASURE_ITERS: usize = 1_000_000;

// Crossing benches do N untimed setup inserts per measured op; use fewer
// measured samples to keep wall-clock sane (still plenty for stable tails).
const CROSS_WARMUP: usize = 50_000;
const CROSS_MEASURE: usize = 500_000;

// Worst-case events from one taker is 2 * max_levels_crossed + 1; padded so the
// Vec never reallocs inside a timed region (a realloc there is a fake p99 spike).
const EVENT_BUF_CAP: usize = 128;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A unique intent_hash from `seed` (injective over u64 — keeps the book's
/// dedup index happy across millions of resting orders).
fn hash_from_seed(seed: u64) -> IntentHash {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&seed.to_ne_bytes());
    IntentHash(hash)
}

/// Build a New OrderRequest with a unique intent_hash derived from `seed`.
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

/// Build a Cancel request for `hash`. Cancel dispatches on request_type before
/// any field validation, so price/qty/side/tif are irrelevant here.
fn cancel_req(hash: IntentHash) -> OrderRequest {
    OrderRequest {
        price: 0,
        quantity: 0,
        origin_ts: 0,
        instrument_id: INSTR,
        side: Side::Bid,
        order_type: OrderType::Limit,
        tif: TimeInForce::GTC,
        request_type: RequestType::Cancel,
        intent_hash: hash,
    }
}

fn new_engine(capacity: u32) -> Engine {
    let mut e = Engine::new();
    e.add_instrument(INSTR, capacity).expect("add_instrument");
    e
}

/// Fisher-Yates shuffle with an LCG (Knuth/Numerical Recipes constants).
/// Deterministic across runs; matches `orderbook_bench.rs` so `cancel_hit`'s
/// access pattern lines up with orderbook's `cancel_random` for subtraction.
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
// Bench 1 — rest_new_level
// All bids at distinct ascending prices, no asks → never crosses, always rests
// at a brand-new level. Compare against orderbook's insert_new_level to isolate
// the Engine wrapper's per-op overhead.
// ---------------------------------------------------------------------------

fn bench_rest_new_level(cal: &TscCalibration) -> Histogram {
    let capacity = ((WARMUP_ITERS + MEASURE_ITERS) as u32).next_power_of_two();
    let mut engine = new_engine(capacity);
    let mut hist = Histogram::new("rest_new_level");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);

    for i in 0..WARMUP_ITERS {
        let o = req(Side::Bid, i as u64 + 1, 100, TimeInForce::GTC, RequestType::New, i as u64);
        buf.clear();
        engine.process(&o, &mut buf);
    }
    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let o = req(Side::Bid, i as u64 + 1, 100, TimeInForce::GTC, RequestType::New, i as u64);
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&o), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        hist.record(t1.since(t0).to_nanos(cal));
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 2 — rest_existing_level
// All bids at ONE price, no asks → rests at the same (deepening) level.
// ---------------------------------------------------------------------------

fn bench_rest_existing_level(cal: &TscCalibration) -> Histogram {
    let capacity = ((WARMUP_ITERS + MEASURE_ITERS) as u32).next_power_of_two();
    let mut engine = new_engine(capacity);
    let mut hist = Histogram::new("rest_existing_level");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    const PRICE: u64 = 1_000_000; // constant; no asks, so never crosses

    for i in 0..WARMUP_ITERS {
        let o = req(Side::Bid, PRICE, 100, TimeInForce::GTC, RequestType::New, i as u64);
        buf.clear();
        engine.process(&o, &mut buf);
    }
    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let o = req(Side::Bid, PRICE, 100, TimeInForce::GTC, RequestType::New, i as u64);
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&o), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        hist.record(t1.since(t0).to_nanos(cal));
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 3 — cross_n (steady-state replenish)
// Each iter rests `n` asks (untimed), then times one taker that crosses all n.
// IOC taker fully fills (qty == n) → no pre-check, no residual: pure cross.
// FOK variant adds the has_sufficient_liquidity pre-check before the same cross,
// so fok_success_n − cross_n is the happy-path pre-check overhead.
// ---------------------------------------------------------------------------

fn bench_cross(cal: &TscCalibration, label: &str, n: u64, taker_tif: TimeInForce) -> Histogram {
    let mut engine = new_engine(65_536);
    let mut hist = Histogram::new(label);
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    let total = CROSS_WARMUP + CROSS_MEASURE;
    for it in 0..total {
        // setup (untimed): n asks at ascending prices 100..100+n, qty 1 each.
        for lvl in 0..n {
            let ask = req(Side::Ask, 100 + lvl, 1, TimeInForce::GTC, RequestType::New, seed);
            seed += 1;
            buf.clear();
            engine.process(&ask, &mut buf);
        }
        // taker bids above the top ask so it crosses every level; qty n fully fills.
        let taker = req(Side::Bid, 100 + n, n, taker_tif, RequestType::New, seed);
        seed += 1;
        buf.clear();

        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&taker), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);

        if it >= CROSS_WARMUP {
            hist.record(t1.since(t0).to_nanos(cal));
        }
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 4 — fok_fail_fast (static book)
// Asks priced far above the taker's bid → FOK pre-check bails on level 1.
// Book is never mutated (FOK fail), so every measured op is identical.
// ---------------------------------------------------------------------------

fn bench_fok_fail_fast(cal: &TscCalibration) -> Histogram {
    let mut engine = new_engine(4096);
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    // 16 ask levels priced 1000.., depth 1. Taker bids 100 → never crosses.
    for lvl in 0..16u64 {
        let ask = req(Side::Ask, 1000 + lvl, 1, TimeInForce::GTC, RequestType::New, seed);
        seed += 1;
        buf.clear();
        engine.process(&ask, &mut buf);
    }

    let mut hist = Histogram::new("fok_fail_fast");
    for it in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        let taker = req(Side::Bid, 100, 10, TimeInForce::FOK, RequestType::New, seed);
        seed += 1;
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&taker), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        if it >= WARMUP_ITERS {
            hist.record(t1.since(t0).to_nanos(cal));
        }
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 5 — fok_fail_deep (static book)
// L crossing levels totaling Q units; taker FOK needs Q+1 → pre-check walks
// ALL L levels, sums short, rejects. Book untouched. Isolates the O(L) walk.
// ---------------------------------------------------------------------------

fn bench_fok_fail_deep(cal: &TscCalibration) -> Histogram {
    const L: u64 = 16;
    let mut engine = new_engine(4096);
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    // L ask levels at 100..100+L, qty 1 each → total Q = L.
    for lvl in 0..L {
        let ask = req(Side::Ask, 100 + lvl, 1, TimeInForce::GTC, RequestType::New, seed);
        seed += 1;
        buf.clear();
        engine.process(&ask, &mut buf);
    }

    let mut hist = Histogram::new("fok_fail_deep");
    for it in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        // crosses every level (bid above top) but needs L+1 > Q=L → fails.
        let taker = req(Side::Bid, 100 + L, L + 1, TimeInForce::FOK, RequestType::New, seed);
        seed += 1;
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&taker), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        if it >= WARMUP_ITERS {
            hist.record(t1.since(t0).to_nanos(cal));
        }
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 6 — ioc_no_liquidity (static empty book)
// IOC taker, no opposing liquidity → single Reject, zero fills, no rest.
// The reject floor: validate + dispatch + one failed peek + reject emit.
// ---------------------------------------------------------------------------

fn bench_ioc_no_liquidity(cal: &TscCalibration) -> Histogram {
    let mut engine = new_engine(64);
    let mut hist = Histogram::new("ioc_no_liquidity");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    for it in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        let taker = req(Side::Bid, 100, 10, TimeInForce::IOC, RequestType::New, seed);
        seed += 1;
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&taker), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        if it >= WARMUP_ITERS {
            hist.record(t1.since(t0).to_nanos(cal));
        }
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 7 — cancel_hit
// Pre-build a deep book, store hashes, cancel in shuffled order so each cancel
// hits a random position in its level's VecDeque (the realistic case, and the
// O(level-depth) scan). Setup mirrors orderbook's cancel_random (LEVEL_DEPTH,
// shuffle seed) so cancel_hit − orderbook::cancel_random = Engine cancel overhead.
// ---------------------------------------------------------------------------

fn bench_cancel_hit(cal: &TscCalibration) -> Histogram {
    const LEVEL_DEPTH: usize = 50;
    let num_levels = (WARMUP_ITERS + MEASURE_ITERS) / LEVEL_DEPTH + 1;
    let n = num_levels * LEVEL_DEPTH;
    let capacity = (n as u32).next_power_of_two();
    let mut engine = new_engine(capacity);
    let mut hist = Histogram::new("cancel_hit");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);

    // Pre-insert n bids across levels (no asks → all rest); record their hashes.
    let mut hashes = Vec::with_capacity(n);
    let mut seed: u64 = 0;
    for level_idx in 0..num_levels {
        let price = (level_idx + 1) as u64;
        for _ in 0..LEVEL_DEPTH {
            let o = req(Side::Bid, price, 100, TimeInForce::GTC, RequestType::New, seed);
            hashes.push(o.intent_hash);
            buf.clear();
            engine.process(&o, &mut buf);
            seed += 1;
        }
    }
    deterministic_shuffle(&mut hashes, 0xCAFE_F00D);

    for i in 0..WARMUP_ITERS {
        let c = cancel_req(hashes[i]);
        buf.clear();
        engine.process(&c, &mut buf);
    }
    for i in WARMUP_ITERS..(WARMUP_ITERS + MEASURE_ITERS) {
        let c = cancel_req(hashes[i]);
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&c), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        hist.record(t1.since(t0).to_nanos(cal));
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 8 — cancel_miss (static book)
// Populate ~30K resting orders so the index is realistically sized, then cancel
// hashes from a disjoint seed range (never inserted) → always UnknownOrder.
// Misses don't mutate the book, so every measured op is identical.
// ---------------------------------------------------------------------------

fn bench_cancel_miss(cal: &TscCalibration) -> Histogram {
    let mut engine = new_engine(65_536);
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    for _ in 0..30_000 {
        let o = req(Side::Bid, 100 + (seed % 100), 100, TimeInForce::GTC, RequestType::New, seed);
        buf.clear();
        engine.process(&o, &mut buf);
        seed += 1;
    }

    let mut hist = Histogram::new("cancel_miss");
    let mut miss_seed: u64 = 1_000_000_000; // disjoint from inserted seeds
    for it in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        let c = cancel_req(hash_from_seed(miss_seed));
        miss_seed += 1;
        buf.clear();
        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&c), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        if it >= WARMUP_ITERS {
            hist.record(t1.since(t0).to_nanos(cal));
        }
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 9 — partial_then_rest (steady-state)
// Times a GTC taker that crosses one resting maker AND rests its remainder —
// the combined match+insert path. Setup rests 1 ask (untimed); cleanup cancels
// the rested remainder (untimed) so the book returns to empty each iter, which
// also keeps the next iter's ask from crossing leftover bids.
// ---------------------------------------------------------------------------

fn bench_partial_then_rest(cal: &TscCalibration) -> Histogram {
    let mut engine = new_engine(65_536);
    let mut hist = Histogram::new("partial_then_rest");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);
    let mut seed: u64 = 0;

    let total = CROSS_WARMUP + CROSS_MEASURE;
    for it in 0..total {
        // setup (untimed): one ask @100 qty 1. Bid side is empty → it rests.
        let ask = req(Side::Ask, 100, 1, TimeInForce::GTC, RequestType::New, seed);
        seed += 1;
        buf.clear();
        engine.process(&ask, &mut buf);

        // taker bid @100 qty 2: crosses the 1 ask (fills 1), rests remainder 1.
        let taker_seed = seed;
        let taker = req(Side::Bid, 100, 2, TimeInForce::GTC, RequestType::New, taker_seed);
        seed += 1;
        buf.clear();

        let t0 = rdtscp_then_lfence();
        engine.process(std::hint::black_box(&taker), &mut buf);
        let t1 = rdtscp();
        std::hint::black_box(&buf);
        if it >= CROSS_WARMUP {
            hist.record(t1.since(t0).to_nanos(cal));
        }

        // cleanup (untimed): cancel the rested remainder (carries taker's hash).
        let c = cancel_req(hash_from_seed(taker_seed));
        buf.clear();
        engine.process(&c, &mut buf);
    }
    hist
}

// ---------------------------------------------------------------------------
// Bench 10 — realistic_workload (the sustained-throughput headline)
// Mixed stream around a mid price: 60% passive GTC limit (rests), 30% cancel of
// a random active order, 10% aggressive IOC that sweeps ~3 levels. Book hovers
// at steady depth. Single mixed histogram → its 1/mean is the representative
// single-core sustained throughput. Split into 3 histograms for per-op p99s.
// ---------------------------------------------------------------------------

fn bench_realistic_workload(cal: &TscCalibration) -> Histogram {
    const MID: u64 = 1_000;
    const MAX_ACTIVE: usize = 80_000; // bounds book depth; capacity must exceed it
    let mut engine = new_engine(131_072);
    let mut hist = Histogram::new("realistic_workload");
    let mut buf = Vec::with_capacity(EVENT_BUF_CAP);

    let mut active: Vec<IntentHash> = Vec::with_capacity(MAX_ACTIVE);
    let mut seed: u64 = 0;

    // LCG roll, same constants as orderbook_bench.
    let mut rng: u64 = 0x9E37_79B9;
    let mut roll = || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        rng
    };

    // Pre-fill ~30K passive orders straddling the mid.
    for _ in 0..30_000 {
        let r = roll();
        let (side, price) = if r & 1 == 0 {
            (Side::Bid, MID - 1 - (r >> 1) % 50) // bids in [MID-50, MID-1]
        } else {
            (Side::Ask, MID + 1 + (r >> 1) % 50) // asks in [MID+1, MID+50]
        };
        let o = req(side, price, 1, TimeInForce::GTC, RequestType::New, seed);
        active.push(o.intent_hash);
        buf.clear();
        engine.process(&o, &mut buf);
        seed += 1;
    }

    for i in 0..(WARMUP_ITERS + MEASURE_ITERS) {
        let measure = i >= WARMUP_ITERS;
        let pick = roll() % 100;

        // Build the request for this op (request construction is untimed).
        let request = if pick < 60 && active.len() < MAX_ACTIVE {
            // 60%: passive GTC limit that rests (does not cross the mid).
            let r = roll();
            let (side, price) = if r & 1 == 0 {
                (Side::Bid, MID - 1 - (r >> 1) % 50)
            } else {
                (Side::Ask, MID + 1 + (r >> 1) % 50)
            };
            let o = req(side, price, 1, TimeInForce::GTC, RequestType::New, seed);
            seed += 1;
            active.push(o.intent_hash);
            o
        } else if pick < 90 && !active.is_empty() {
            // 30%: cancel a random active order (may miss if already swept — realistic).
            let idx = (roll() as usize) % active.len();
            cancel_req(active.swap_remove(idx))
        } else {
            // 10%: aggressive IOC sweeping ~3 units through the opposing side.
            let buy = roll() & 1 == 0;
            let (side, price) = if buy { (Side::Bid, MID + 1000) } else { (Side::Ask, 1) };
            let o = req(side, price, 3, TimeInForce::IOC, RequestType::New, seed);
            seed += 1;
            o
        };

        buf.clear();
        if measure {
            let t0 = rdtscp_then_lfence();
            engine.process(std::hint::black_box(&request), &mut buf);
            let t1 = rdtscp();
            std::hint::black_box(&buf);
            hist.record(t1.since(t0).to_nanos(cal));
        } else {
            engine.process(&request, &mut buf);
        }
    }
    hist
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Reference — raw orderbook insert (no Engine), same setup as rest_new_level so
// `rest_new_level.p50 − raw_book_insert.p50` = the Engine wrapper's overhead.
// ---------------------------------------------------------------------------

fn bench_raw_orderbook_insert(cal: &TscCalibration) -> Histogram {
    use orderbook::OrderBook;
    use types::{Order, OrderId};

    let capacity = ((WARMUP_ITERS + MEASURE_ITERS) as u32).next_power_of_two();
    let mut book = OrderBook::new(INSTR, capacity);
    let mut hist = Histogram::new("raw_book_insert");

    let mk = |seed: u64| -> Order {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&seed.to_ne_bytes());
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
            intent_hash: IntentHash(h),
        }
    };

    for i in 0..WARMUP_ITERS as u64 {
        book.insert(mk(i)).unwrap();
    }
    for i in WARMUP_ITERS as u64..(WARMUP_ITERS + MEASURE_ITERS) as u64 {
        let o = mk(i);
        let t0 = rdtscp_then_lfence();
        let r = book.insert(std::hint::black_box(o));
        let t1 = rdtscp();
        std::hint::black_box(&r);
        r.unwrap();
        hist.record(t1.since(t0).to_nanos(cal));
    }
    hist
}

// ---------------------------------------------------------------------------
// Report rendering — everything is built into Strings and written to files;
// nothing is printed to stdout (only the output paths, to stderr).
// ---------------------------------------------------------------------------

fn read_sys(path: &str) -> String {
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// (year, month, day) from days-since-epoch — Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "YYYY-MM-DD_HH-MM-SS" UTC, filename-safe.
fn utc_stamp(secs: u64) -> String {
    let (h, mi, s) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

fn methodology(cal: &TscCalibration, stamp: &str, secs: u64) -> String {
    let governor = read_sys("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor");
    let turbo = match read_sys("/sys/devices/system/cpu/intel_pstate/no_turbo").as_str() {
        "1" => "off",
        "0" => "on",
        _ => "?",
    };
    let isolated = {
        let s = read_sys("/sys/devices/system/cpu/isolated");
        if s.is_empty() { "none".into() } else { s }
    };
    let affinity = read_sys("/proc/self/status")
        .lines()
        .find(|l| l.starts_with("Cpus_allowed_list:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "?".into());
    let pinned = !(affinity.contains('-') || affinity.contains(','));

    let mut s = String::new();
    let _ = writeln!(s, "# Matcher engine benchmark — {stamp} UTC (unix {secs})\n");
    let _ = writeln!(s, "**Hardware:** {}", cpu_model());
    let _ = writeln!(
        s,
        "- governor `{governor}` · turbo `{turbo}` · isolated cores `{isolated}` · affinity `{affinity}` {}",
        if pinned { "(pinned ✓)" } else { "(NOT pinned ⚠)" }
    );
    let _ = writeln!(
        s,
        "- TSC rate ~{:.2} GHz · samples {WARMUP_ITERS} warmup / {MEASURE_ITERS} measure (cross/fok {CROSS_WARMUP}/{CROSS_MEASURE})",
        cal.ticks_per_nanosecond
    );
    let _ = writeln!(
        s,
        "- scope: engine-internal `process()` (matcher + orderbook + arena); includes event emit into the Vec, excludes wire decode & transport."
    );
    let _ = writeln!(
        s,
        "- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first; tails need an isolated core."
    );
    if !pinned {
        let _ = writeln!(
            s,
            "- ⚠ process not pinned to one core (affinity `{affinity}`): tails include OS migration jitter. Re-run under `taskset -c <core>`."
        );
    }
    s
}

fn humanize(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn table_row(h: &Histogram) -> String {
    let p50 = h.p50().as_u64();
    let tail = if p50 > 0 { h.p99_99().as_u64() as f64 / p50 as f64 } else { 0.0 };
    format!(
        "| {} | {} | {} | {} | {} | {} | {:.0}× | {:.2}M |",
        h.label(),
        humanize(h.len()),
        h.min().as_u64(),
        p50,
        h.p99().as_u64(),
        h.p99_99().as_u64(),
        tail,
        h.throughput_per_sec() / 1e6,
    )
}

fn section(title: &str, hists: &[&Histogram]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\n**{title}**\n");
    let _ = writeln!(s, "| scenario | n | min | p50 | p99 | p99.99 | tail | M/s |");
    let _ = writeln!(s, "|---|--:|--:|--:|--:|--:|--:|--:|");
    for h in hists {
        let _ = writeln!(s, "{}", table_row(h));
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn headline_deltas(
    rest_new: &Histogram,
    raw: &Histogram,
    cross1: &Histogram,
    cross16: &Histogram,
    fok16: &Histogram,
    fast: &Histogram,
    deep: &Histogram,
) -> String {
    let p50 = |h: &Histogram| h.p50().as_u64() as i64;
    let mut s = String::new();
    let _ = writeln!(s, "\n**Headline deltas (p50, ns)**\n");
    let _ = writeln!(s, "| metric | ns | derivation |");
    let _ = writeln!(s, "|---|--:|---|");
    let _ = writeln!(s, "| Engine wrapper overhead | {} | rest_new_level − raw_book_insert |", p50(rest_new) - p50(raw));
    let _ = writeln!(s, "| Marginal cost / level swept | {} | (cross_n=16 − cross_n=1) / 15 |", (p50(cross16) - p50(cross1)) / 15);
    let _ = writeln!(s, "| FOK pre-check overhead (16 lvl) | {} | fok_success_n=16 − cross_n=16 |", p50(fok16) - p50(cross16));
    let _ = writeln!(s, "| FOK liquidity walk (16 lvl) | {} | fok_fail_deep − fok_fail_fast |", p50(deep) - p50(fast));
    s
}

fn main() {
    let cal = calibrate();
    let secs = unix_secs();
    let stamp = utc_stamp(secs);

    // Run every scenario (no stdout).
    let rest_new = bench_rest_new_level(&cal);
    let rest_exist = bench_rest_existing_level(&cal);
    let raw = bench_raw_orderbook_insert(&cal);
    let cross1 = bench_cross(&cal, "cross_n=1", 1, TimeInForce::IOC);
    let cross4 = bench_cross(&cal, "cross_n=4", 4, TimeInForce::IOC);
    let cross16 = bench_cross(&cal, "cross_n=16", 16, TimeInForce::IOC);
    let fok1 = bench_cross(&cal, "fok_success_n=1", 1, TimeInForce::FOK);
    let fok4 = bench_cross(&cal, "fok_success_n=4", 4, TimeInForce::FOK);
    let fok16 = bench_cross(&cal, "fok_success_n=16", 16, TimeInForce::FOK);
    let fok_fast = bench_fok_fail_fast(&cal);
    let fok_deep = bench_fok_fail_deep(&cal);
    let ioc_dry = bench_ioc_no_liquidity(&cal);
    let cancel_hit = bench_cancel_hit(&cal);
    let cancel_miss = bench_cancel_miss(&cal);
    let partial = bench_partial_then_rest(&cal);
    let realistic = bench_realistic_workload(&cal);

    // Build the markdown report.
    let mut report = methodology(&cal, &stamp, secs);
    report.push_str(&section("Resting (no cross)", &[&rest_new, &rest_exist]));
    report.push_str(&section("Reference — raw orderbook (no Engine)", &[&raw]));
    report.push_str(&section("Crossing", &[&cross1, &cross4, &cross16]));
    report.push_str(&section("FOK gate", &[&fok1, &fok4, &fok16, &fok_fast, &fok_deep]));
    report.push_str(&section("Reject floor", &[&ioc_dry]));
    report.push_str(&section("Cancel", &[&cancel_hit, &cancel_miss]));
    report.push_str(&section("Combined / composite", &[&partial, &realistic]));
    report.push_str(&headline_deltas(&rest_new, &raw, &cross1, &cross16, &fok16, &fok_fast, &fok_deep));

    // Build the CSV (machine-readable, for cross-run comparison).
    let all = [
        &rest_new, &rest_exist, &raw, &cross1, &cross4, &cross16, &fok1, &fok4, &fok16,
        &fok_fast, &fok_deep, &ioc_dry, &cancel_hit, &cancel_miss, &partial, &realistic,
    ];
    let mut csv = String::new();
    csv.push_str(Histogram::csv_header());
    csv.push('\n');
    for h in all {
        csv.push_str(&h.render_csv());
        csv.push('\n');
    }

    // Write to a gitignored, timestamped folder so runs can be compared later.
    let dir = std::env::var("BENCH_OUT_DIR")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench-runs").to_string());
    std::fs::create_dir_all(&dir).expect("create bench-runs dir");
    let md_path = format!("{dir}/matcher_{stamp}.md");
    let csv_path = format!("{dir}/matcher_{stamp}.csv");
    std::fs::write(&md_path, report).expect("write report");
    std::fs::write(&csv_path, csv).expect("write csv");
    eprintln!("benchmark run written:\n  {md_path}\n  {csv_path}");
}
