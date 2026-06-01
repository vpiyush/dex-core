//! Deterministic instruction-count benchmark via iai-callgrind.
//!
//! Gated behind the `iai` feature (see docs/benchmarking.md §6). The default
//! `cargo bench -p matcher` ignores this file; run it explicitly with:
//!   cargo bench -p matcher --features iai --bench matcher_iai
//! Requires valgrind + a matching `iai-callgrind-runner` (cargo install).
//!
//! callgrind counts instructions / cache accesses exactly and reproducibly —
//! immune to governor/turbo/core migration. Use it as a CI regression gate
//! (assert on instruction deltas) and a complementary "N instructions per
//! cross" figure. It is a model (estimated cycles, not real out-of-order
//! execution), so it complements — never replaces — the rdtscp wall-clock run.
//!
//! TWO methodology rules make the numbers measure `process()` and nothing else:
//!   1. `setup =` runs OUTSIDE the measured region. All engine construction and
//!      pre-resting liquidity happens there, so the measured body is a single
//!      `process()` call — not a 65k-slot arena init.
//!   2. The benched fn RETURNS the engine + buffer it was handed. iai-callgrind
//!      drops the return value outside the measured region, so the (expensive)
//!      Engine/arena teardown is not counted. Values merely dropped at end of
//!      the fn body WOULD be counted — hence we hand them back.
//!
//! NB: iai-callgrind's #[library_benchmark] rejects `///` doc comments on the
//! benchmarked fn (they parse as #[doc] attributes) — use plain `//`.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use matcher::Engine;
use std::hint::black_box;
use types::{IntentHash, OrderEvent, OrderRequest, OrderType, RequestType, Side, TimeInForce};

const INSTR: u32 = 1;
const CAP: u32 = 1024;

fn hash_from_seed(seed: u64) -> IntentHash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&seed.to_ne_bytes());
    IntentHash(h)
}

fn new_req(side: Side, price: u64, qty: u64, tif: TimeInForce, seed: u64) -> OrderRequest {
    OrderRequest {
        price,
        quantity: qty,
        origin_ts: 0,
        instrument_id: INSTR,
        side,
        order_type: OrderType::Limit,
        tif,
        request_type: RequestType::New,
        intent_hash: hash_from_seed(seed),
    }
}

type Fixture = (Engine, OrderRequest, Vec<OrderEvent>);

// ---- setup fns (run OUTSIDE measurement) -----------------------------------

// Empty book; the taker is a GTC bid that will rest (no opposing liquidity).
fn setup_rest() -> Fixture {
    let mut e = Engine::new();
    e.add_instrument(INSTR, CAP).unwrap();
    let taker = new_req(Side::Bid, 100, 10, TimeInForce::GTC, 1);
    (e, taker, Vec::with_capacity(64))
}

// Book pre-loaded with `asks` resting ask levels (price 100.., qty 1 each).
// Taker is an IOC bid priced above the top ask, qty = asks → sweeps all of them.
fn setup_cross(asks: u64) -> Fixture {
    let mut e = Engine::new();
    e.add_instrument(INSTR, CAP).unwrap();
    let mut buf = Vec::with_capacity(64);
    let mut seed = 0;
    for lvl in 0..asks {
        buf.clear();
        e.process(&new_req(Side::Ask, 100 + lvl, 1, TimeInForce::GTC, seed), &mut buf);
        seed += 1;
    }
    buf.clear();
    let taker = new_req(Side::Bid, 100 + asks, asks, TimeInForce::IOC, seed);
    (e, taker, buf)
}

// Book with 16 ask units; taker is a FOK needing 17 → fails the pre-check,
// rejects with zero fills (book untouched).
fn setup_fok_fail() -> Fixture {
    let (e, _, buf) = setup_cross(16);
    let taker = new_req(Side::Bid, 200, 17, TimeInForce::FOK, 9_999);
    (e, taker, buf)
}

// ---- the single measured op: one process() call ----------------------------
// Returns the fixture so iai drops it (Engine/arena teardown) outside the
// measured region. `black_box` on inputs and output prevents the optimizer from
// eliding the call.

fn run_one(mut fx: Fixture) -> Fixture {
    let (ref mut e, ref taker, ref mut buf) = fx;
    buf.clear();
    e.process(black_box(taker), black_box(buf));
    black_box(fx)
}

// A GTC limit that rests (empty opposing side) — the insert path.
#[library_benchmark]
#[bench::rest(setup = setup_rest)]
fn rest_one(fx: Fixture) -> Fixture {
    run_one(fx)
}

// IOC takers crossing 1 vs 16 levels — slope = per-level instruction cost.
#[library_benchmark]
#[bench::n1(args = (1,), setup = setup_cross)]
#[bench::n16(args = (16,), setup = setup_cross)]
fn cross(fx: Fixture) -> Fixture {
    run_one(fx)
}

// FOK that fails its pre-check (needs 17, only 16 available) — reject, no fills.
#[library_benchmark]
#[bench::fail(setup = setup_fok_fail)]
fn fok_fail(fx: Fixture) -> Fixture {
    run_one(fx)
}

library_benchmark_group!(
    name = matcher_ops;
    benchmarks = rest_one, cross, fok_fail
);

main!(library_benchmark_groups = matcher_ops);
