//! Per-run result types: the latency distribution plus the OS-noise counters
//! captured during the *same* execution.

use telemetry::primitives::Histogram;

/// How a scenario's ops were driven. A correctness field, not decoration:
/// back-to-back p99.99 (service time) and open-loop p99.99 (latency under load)
/// measure different things and must never be compared or conflated. Stamped on
/// the [`Sample`] so a number cannot be read without its arrival model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArrivalModel {
    /// Next op starts the instant the previous finishes. Measures the *service
    /// time* of one isolated op. Suffers coordinated omission (a stall hides the
    /// lateness it inflicts on the ops that would have arrived during it), so it
    /// is honest only as "isolated op latency, no arrival schedule".
    BackToBack,
    /// Ops scheduled every `interval_ns`; overruns back-fill the missed slots so
    /// a stall's true latency impact is counted (the wrk2 / Gil-Tene-correct
    /// model). Produced by the open-loop runner — a later phase; the variant is
    /// defined now so `Sample` carries the tag from day one.
    OpenLoop { interval_ns: u64 },
}

/// OS-noise counters captured around the *measure phase* of one scenario, via
/// `getrusage(RUSAGE_THREAD)` deltas — cheap, no root, no perturbation (read
/// outside the `rdtscp` fence). These explain a tail: on a non-pinned core, a
/// non-zero `ctx_involuntary` or `major_faults` is the proof a p99 spike was the
/// OS, not the code.
///
/// `migrations` needs `perf_event_open` and stays `None` under the default
/// rusage tier (see benchkit LLD §2.8).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RunStats {
    /// `ru_nvcsw` — voluntary context switches (the thread gave up the CPU,
    /// e.g. blocked). Usually benign.
    pub ctx_voluntary: u64,
    /// `ru_nivcsw` — involuntary context switches (preempted). The usual p99
    /// culprit on a shared/un-isolated core.
    pub ctx_involuntary: u64,
    /// `ru_minflt` — minor page faults (page mapped without disk I/O).
    pub minor_faults: u64,
    /// `ru_majflt` — major page faults (fault that hit disk). Should be 0 in
    /// steady state; non-zero invalidates a latency run.
    pub major_faults: u64,
    /// Cross-core migrations. `Some` only under the perf-event tier.
    pub migrations: Option<u64>,
}

impl RunStats {
    /// `self - earlier`, field by field (saturating, so a counter that somehow
    /// went backwards yields 0 rather than wrapping). `migrations` follows the
    /// same rule when both sides are `Some`, else `None`.
    pub fn delta_from(&self, earlier: &RunStats) -> RunStats {
        RunStats {
            ctx_voluntary: self.ctx_voluntary.saturating_sub(earlier.ctx_voluntary),
            ctx_involuntary: self.ctx_involuntary.saturating_sub(earlier.ctx_involuntary),
            minor_faults: self.minor_faults.saturating_sub(earlier.minor_faults),
            major_faults: self.major_faults.saturating_sub(earlier.major_faults),
            migrations: match (self.migrations, earlier.migrations) {
                (Some(a), Some(b)) => Some(a.saturating_sub(b)),
                _ => None,
            },
        }
    }
}

/// One scenario's result: its latency distribution, the arrival model that
/// produced it, and the OS-noise counters from the *same* run.
///
/// Bundling the histogram with its `RunStats` enforces the co-execution rule —
/// a noise count only explains a latency tail if both came from one execution,
/// so the type makes them inseparable. `Report` consumes `&Sample`.
pub struct Sample {
    pub hist: Histogram,
    pub arrival_model: ArrivalModel,
    pub stats: RunStats,
    /// Set only by the open-loop runner when the target rate could not be
    /// sustained (service time exceeded the arrival interval for too many ops).
    /// A `true` here means the latency numbers reflect a *saturated* system and
    /// must be reported as overloaded, not as latency-under-load.
    pub overloaded: bool,
}

impl Sample {
    pub fn arrival_model(&self) -> ArrivalModel {
        self.arrival_model
    }
}

/// Read the calling thread's resource-usage counters into [`RunStats`].
///
/// `migrations` is always `None` here (rusage has no migration counter); the
/// perf-event tier fills it later.
pub(crate) fn read_rusage_thread() -> RunStats {
    // SAFETY: getrusage writes a fully-initialized `rusage` into the out-param
    // when it returns 0. We pass a zeroed, correctly-typed, stack-owned struct
    // and a valid `RUSAGE_THREAD` selector; the call cannot retain the pointer
    // past return. On the (effectively impossible for RUSAGE_THREAD) error path
    // we leave the zeroed value, which `delta_from` treats as "no change".
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) };
    debug_assert_eq!(rc, 0, "getrusage(RUSAGE_THREAD) failed");
    RunStats {
        ctx_voluntary: usage.ru_nvcsw as u64,
        ctx_involuntary: usage.ru_nivcsw as u64,
        minor_faults: usage.ru_minflt as u64,
        major_faults: usage.ru_majflt as u64,
        migrations: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_subtracts_fieldwise() {
        let a = RunStats { ctx_voluntary: 10, ctx_involuntary: 5, minor_faults: 100, major_faults: 1, migrations: Some(3) };
        let b = RunStats { ctx_voluntary: 4, ctx_involuntary: 5, minor_faults: 40, major_faults: 1, migrations: Some(1) };
        let d = a.delta_from(&b);
        assert_eq!(d.ctx_voluntary, 6);
        assert_eq!(d.ctx_involuntary, 0);
        assert_eq!(d.minor_faults, 60);
        assert_eq!(d.major_faults, 0);
        assert_eq!(d.migrations, Some(2));
    }

    #[test]
    fn delta_saturates_on_decrease() {
        // A counter that appears to go backwards yields 0, never wraps.
        let a = RunStats { ctx_voluntary: 1, ..Default::default() };
        let b = RunStats { ctx_voluntary: 9, ..Default::default() };
        assert_eq!(a.delta_from(&b).ctx_voluntary, 0);
    }

    #[test]
    fn migrations_none_if_either_side_none() {
        let a = RunStats { migrations: Some(5), ..Default::default() };
        let b = RunStats { migrations: None, ..Default::default() };
        assert_eq!(a.delta_from(&b).migrations, None);
    }

    #[test]
    fn rusage_read_is_monotonic_nondecreasing() {
        // Counters only ever go up within a thread's life, so a later read minus
        // an earlier read is well-defined (non-negative). We can't assert exact
        // values (OS-dependent), only the ordering invariant delta relies on.
        let first = read_rusage_thread();
        // Touch some pages to (likely) bump minor_faults; not strictly required.
        let _v: Vec<u8> = (0..4096).map(|i| i as u8).collect();
        let second = read_rusage_thread();
        assert!(second.ctx_voluntary >= first.ctx_voluntary);
        assert!(second.minor_faults >= first.minor_faults);
    }
}
