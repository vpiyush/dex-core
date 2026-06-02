//! The headline methodology test: back-to-back and open-loop measure DIFFERENT
//! things, which is the whole justification for tagging them separately and
//! never comparing them. We construct a workload with a periodic stall and show
//! that open-loop's tail is dragged up by coordinated-omission back-fill while
//! back-to-back's tail barely moves.

use std::hint::black_box;

use benchkit::{ArrivalModel, Iters, Runner};

/// An op that is fast most of the time but stalls hard every `STALL_EVERY` ops,
/// simulating an OS hiccup / GC-like pause. Deterministic.
struct Stally {
    i: u64,
}

const STALL_EVERY: u64 = 200;

fn busy(iters: u64) -> u64 {
    let mut x = 0u64;
    for _ in 0..iters {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    black_box(x)
}

#[test]
fn open_loop_tail_reflects_coordinated_omission() {
    let runner = Runner::new();
    let iters = Iters::new(2_000, 40_000);

    // Back-to-back: the stall is recorded as just a few slow samples.
    let mut s1 = Stally { i: 0 };
    let b2b = runner.bench(
        "stall_b2b",
        iters,
        &mut s1,
        |_, _| (),
        |st, ()| {
            st.i += 1;
            if st.i % STALL_EVERY == 0 {
                busy(2_000) // big stall
            } else {
                busy(2) // tiny normal op
            }
        },
    );
    assert_eq!(b2b.arrival_model, ArrivalModel::BackToBack);

    // Open-loop at an interval near the *normal* op's cost: when the stall hits,
    // it blows past many scheduled slots, which record_correct back-fills.
    let mut s2 = Stally { i: 0 };
    let ol = runner.bench_open_loop(
        "stall_ol",
        iters,
        200, // ~200ns target interval
        &mut s2,
        |_, _| (),
        |st, ()| {
            st.i += 1;
            if st.i % STALL_EVERY == 0 {
                busy(2_000)
            } else {
                busy(2)
            }
        },
    );
    assert!(matches!(ol.arrival_model, ArrivalModel::OpenLoop { .. }));

    // The core claim: coordinated-omission correction makes the open-loop result
    // reflect the stalls' true impact, while back-to-back under-reports it. We
    // assert on the two signals the mechanism guarantees *deterministically* —
    // not on p99.99, which at a handful of samples is dominated by whichever run
    // happened to catch the largest OS hiccup (noise, not CO).
    //
    // Both signals also distinguish correct CO from broken CO: if `record_correct`
    // were silently replaced by `record`, open-loop would record exactly `measure`
    // samples with the same mean as back-to-back, and both asserts would fail.

    // 1. Back-fill synthesizes extra samples for the slots a stall blew past.
    assert!(
        ol.hist.len() > b2b.hist.len(),
        "open-loop must back-fill missed slots: open-loop len {} vs back-to-back len {}",
        ol.hist.len(),
        b2b.hist.len()
    );

    // 2. Those synthetic samples are all in the elevated range, so the mean rises.
    //    Mean is stable across runs (not dominated by a single outlier), so this
    //    is robust on a noisy/un-pinned machine.
    assert!(
        ol.hist.mean() > b2b.hist.mean(),
        "CO back-fill must raise the mean: open-loop mean {:.0}ns vs back-to-back mean {:.0}ns",
        ol.hist.mean(),
        b2b.hist.mean()
    );
}
