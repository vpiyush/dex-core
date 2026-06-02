//! The back-to-back measurement runner: the single canonical warmup/measure
//! `rdtscp`-fenced loop, written once.

use std::hint::black_box;

use telemetry::primitives::{calibrate, rdtscp, rdtscp_then_lfence, Histogram, Nanos, TscCalibration};

use crate::iters::Iters;
use crate::stats::{read_rusage_thread, ArrivalModel, RunStats, Sample};

/// Owns the TSC calibration and drives scenario measurement. One per benchmark
/// binary; cheap to hold. Single-threaded by intent (matches the matcher/book
/// concurrency model).
pub struct Runner {
    cal: TscCalibration,
}

impl Runner {
    /// Calibrate the TSC once (wraps [`telemetry::primitives::calibrate`]).
    pub fn new() -> Self {
        Self { cal: calibrate() }
    }

    pub fn calibration(&self) -> &TscCalibration {
        &self.cal
    }

    /// Run one scenario back-to-back and return its [`Sample`].
    ///
    /// `prepare(&mut S, i)` does all *untimed* per-iteration work — replenish
    /// liquidity, clear an event buffer, pick the next pre-generated input — and
    /// returns the owned `In` that `op` will time. `op(&mut S, In)` must contain
    /// *only* the operation under measurement. The split is the methodology rule
    /// made structural: setup runs in a different closure, outside the fence, so
    /// it cannot leak into the measured region (the same principle as
    /// iai-callgrind's `setup`). `In` is owned (no borrow of `S`) so the
    /// `prepare` borrow ends before `op`'s `&mut S` begins.
    ///
    /// `i` is the absolute index `0..warmup+measure`, so pre-generated-input
    /// scenarios can index a precomputed table.
    ///
    /// Both the input handed to `op` and its returned `Out` are `black_box`'d:
    /// without that the optimizer may hoist the op out of the fenced region and
    /// the measurement would be meaningless.
    pub fn bench<S, In, Out>(
        &self,
        label: impl Into<String>,
        iters: Iters,
        state: &mut S,
        mut prepare: impl FnMut(&mut S, usize) -> In,
        mut op: impl FnMut(&mut S, In) -> Out,
    ) -> Sample {
        let mut hist = Histogram::new(label);

        // rusage brackets only the MEASURE phase: reset at i == warmup so warmup
        // noise isn't attributed to the recorded samples. Captured outside the
        // fence, so it never perturbs the timing it annotates.
        let mut rusage_before = RunStats::default();

        for i in 0..iters.total() {
            let input = prepare(state, i); // UNTIMED

            if i == iters.warmup {
                rusage_before = read_rusage_thread();
            }

            let t0 = rdtscp_then_lfence();
            let out = op(state, black_box(input)); // TIMED — fence to fence
            let t1 = rdtscp();
            black_box(&out);

            if i >= iters.warmup {
                hist.record(t1.since(t0).to_nanos(&self.cal));
            }
        }

        let stats = read_rusage_thread().delta_from(&rusage_before);
        Sample { hist, arrival_model: ArrivalModel::BackToBack, stats, overloaded: false }
    }

    /// Convenience for static-state scenarios with no per-iteration input: the
    /// timed closure just borrows `&mut S`. Equivalent to [`Runner::bench`] with
    /// a `prepare` that returns `()`.
    pub fn bench_static<S, Out>(
        &self,
        label: impl Into<String>,
        iters: Iters,
        state: &mut S,
        mut op: impl FnMut(&mut S) -> Out,
    ) -> Sample {
        self.bench(label, iters, state, |_, _| (), move |s, ()| op(s))
    }

    /// Open-loop, coordinated-omission-correct measurement: drive ops on a fixed
    /// schedule of one every `interval_ns`, and back-fill the latency of any ops
    /// that were made late by an overrun (the wrk2 / Gil-Tene model). Use this
    /// for any "latency under load" claim; use [`Runner::bench`] (back-to-back)
    /// for isolated service-time microbenchmarks. The two are NOT comparable —
    /// the returned [`Sample`] is tagged [`ArrivalModel::OpenLoop`] so a report
    /// can never conflate them.
    ///
    /// Each measured op is scheduled at `start + i*interval`; the runner spins
    /// until that deadline before timing the op, so a slow op does not delay the
    /// *next* op's clock (that decoupling is what makes the schedule open-loop).
    /// `record_correct` then synthesizes the samples for slots a stall blew past.
    ///
    /// If sustained service time exceeds `interval_ns` the schedule cannot be
    /// kept; the run is *overloaded* and [`Sample::overloaded`] is set so the
    /// report flags it rather than presenting saturated numbers as latency.
    pub fn bench_open_loop<S, In, Out>(
        &self,
        label: impl Into<String>,
        iters: Iters,
        interval_ns: u64,
        state: &mut S,
        mut prepare: impl FnMut(&mut S, usize) -> In,
        mut op: impl FnMut(&mut S, In) -> Out,
    ) -> Sample {
        let mut hist = Histogram::new(label);
        let interval = Nanos(interval_ns);
        let ticks_per_ns = self.cal.ticks_per_nanosecond;
        let interval_ticks = (interval_ns as f64 * ticks_per_ns) as u64;

        // Warm up untimed (no schedule), then start the open-loop clock so the
        // schedule's t0 isn't skewed by cold-start.
        for i in 0..iters.warmup {
            let input = prepare(state, i);
            black_box(op(state, black_box(input)));
        }

        let rusage_before = read_rusage_thread();
        let mut late_ops: u64 = 0;
        let schedule_start = rdtscp_then_lfence().0;

        for k in 0..iters.measure {
            let input = prepare(state, iters.warmup + k);

            // Deadline for this op, in TSC ticks since schedule_start.
            let deadline = schedule_start + (k as u64).saturating_mul(interval_ticks);
            // Open-loop: spin until the scheduled instant. If we're already past
            // it, the previous op overran — count it and proceed immediately.
            let mut now = rdtscp();
            if now.0 < deadline {
                while rdtscp().0 < deadline {
                    std::hint::spin_loop();
                }
            } else {
                late_ops += 1;
            }

            let t0 = rdtscp_then_lfence();
            let out = op(state, black_box(input));
            let t1 = rdtscp();
            black_box(&out);

            hist.record_correct(t1.since(t0).to_nanos(&self.cal), interval);
            now = t1;
            let _ = now;
        }

        let stats = read_rusage_thread().delta_from(&rusage_before);
        // Overloaded if a meaningful fraction of ops could not be scheduled on
        // time (service time >= interval for many ops).
        let overloaded = late_ops * 100 >= iters.measure as u64 * OVERLOAD_PCT;
        Sample {
            hist,
            arrival_model: ArrivalModel::OpenLoop { interval_ns },
            stats,
            overloaded,
        }
    }
}

/// If this percentage or more of scheduled ops missed their deadline, the
/// open-loop run is considered overloaded (target rate unachievable).
const OVERLOAD_PCT: u64 = 5;

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A scenario with a known, controllable cost: spin for `n` iterations so the
    // recorded latency is monotonically related to the work done. We assert
    // structure (counts, ordering, no panics), not absolute ns — those are
    // machine-dependent.

    #[test]
    fn records_exactly_measure_samples() {
        let runner = Runner::new();
        let iters = Iters::new(100, 1_000);
        let sample = runner.bench_static("noop", iters, &mut (), |_| black_box(1u64));
        assert_eq!(sample.hist.len(), 1_000, "only the measure phase is recorded");
    }

    #[test]
    fn arrival_model_is_back_to_back() {
        let runner = Runner::new();
        let sample = runner.bench_static("noop", Iters::new(10, 100), &mut (), |_| 0u64);
        assert_eq!(sample.arrival_model, ArrivalModel::BackToBack);
    }

    #[test]
    fn prepare_runs_total_times_op_runs_total_times() {
        let runner = Runner::new();
        let iters = Iters::new(50, 200);
        // State counts prepare and op invocations separately.
        let mut state = (0usize, 0usize);
        let sample = runner.bench(
            "count",
            iters,
            &mut state,
            |s, _i| {
                s.0 += 1;
                7u64
            },
            |s, input| {
                s.1 += 1;
                black_box(input)
            },
        );
        assert_eq!(state.0, iters.total(), "prepare runs every iteration");
        assert_eq!(state.1, iters.total(), "op runs every iteration");
        assert_eq!(sample.hist.len(), iters.measure as u64);
    }

    #[test]
    fn prepare_index_advances() {
        let runner = Runner::new();
        let iters = Iters::new(5, 10);
        let mut seen_last = 0usize;
        runner.bench(
            "idx",
            iters,
            &mut seen_last,
            |last, i| {
                *last = i;
                i
            },
            |_, i| black_box(i),
        );
        assert_eq!(seen_last, iters.total() - 1, "i runs 0..total-1");
    }

    #[test]
    fn open_loop_tags_arrival_model_and_interval() {
        let runner = Runner::new();
        let s = runner.bench_open_loop(
            "ol",
            Iters::new(100, 1_000),
            500,
            &mut 0u64,
            |_, _| (),
            |st, ()| {
                *st = st.wrapping_add(1);
                black_box(*st)
            },
        );
        assert_eq!(s.arrival_model, ArrivalModel::OpenLoop { interval_ns: 500 });
        // record_correct with a fast op under a generous interval ≈ measure count
        // (no back-fill when nothing overruns), so len is at least measure.
        assert!(s.hist.len() >= 1_000);
    }

    #[test]
    fn open_loop_overload_is_flagged() {
        // Schedule an op every 1ns but make it take far longer — impossible to
        // keep, so the run must flag itself overloaded rather than lie.
        let runner = Runner::new();
        let s = runner.bench_open_loop(
            "saturated",
            Iters::new(100, 5_000),
            1, // 1ns interval — unachievable
            &mut 0u64,
            |_, _| (),
            |st, ()| {
                for _ in 0..50 {
                    *st = st.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                black_box(*st)
            },
        );
        assert!(s.overloaded, "a 1ns schedule with a multi-ns op must flag overload");
    }

    #[test]
    fn larger_op_records_larger_latency() {
        // Sanity that the fence actually measures the op: a heavier op should
        // have a higher p50 than a near-empty one. Generous margin for noise.
        let runner = Runner::new();
        let light = runner.bench_static("light", Iters::new(1_000, 50_000), &mut 0u64, |s| {
            *s = s.wrapping_add(1);
            black_box(*s)
        });
        let heavy = runner.bench_static("heavy", Iters::new(1_000, 50_000), &mut 0u64, |s| {
            for _ in 0..200 {
                *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            }
            black_box(*s)
        });
        assert!(
            heavy.hist.p50().as_u64() > light.hist.p50().as_u64(),
            "heavy p50 {} should exceed light p50 {}",
            heavy.hist.p50().as_u64(),
            light.hist.p50().as_u64()
        );
    }
}
