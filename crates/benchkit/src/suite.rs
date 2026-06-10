//! The intent-driven facade — the ergonomic top layer over `Runner`/`Report`.
//!
//! For crates that want "declare scenarios, declare what to measure, get a
//! correct labeled report" without wiring the core by hand. The matcher keeps
//! using the core directly for its bespoke report; arena/ipc/orderbook use this.

use crate::env::RunEnv;
use crate::iters::Iters;
use crate::report::Report;
use crate::runner::Runner;
use crate::stats::Sample;

/// What the developer wants measured. Each enabled measurement runs as its OWN
/// labeled pass — intent never merges two regimes' numbers into one row, because
/// the instrumentation is mutually exclusive (rdtscp latency vs dhat alloc vs
/// callgrind cache). See benchkit LLD §3.6. The latency pass is always implied.
#[derive(Clone, Copy, Debug)]
pub struct MeasurementIntent {
    /// Wall-clock latency via rdtscp (back-to-back). The default; always on.
    pub latency: bool,
    /// Open-loop latency-under-load (coordinated-omission correct). Needs a rate
    /// set via [`BenchmarkSuite::at_rate`].
    pub latency_under_load: bool,
    /// Heap allocations via dhat. Separate pass (feature `alloc`); flagged here
    /// so the suite can note it must be run as its own binary/pass.
    pub allocations: bool,
    /// Cache/instruction counts via callgrind. Cannot run in-process with the
    /// rdtscp passes — the xtask runner spawns the iai bench; the suite only
    /// records that it was requested.
    pub cache_and_instructions: bool,
}

impl Default for MeasurementIntent {
    fn default() -> Self {
        Self::LATENCY
    }
}

impl MeasurementIntent {
    pub const LATENCY: Self = Self {
        latency: true,
        latency_under_load: false,
        allocations: false,
        cache_and_instructions: false,
    };

    pub fn latency() -> Self {
        Self::LATENCY
    }

    pub fn with_load(mut self) -> Self {
        self.latency_under_load = true;
        self
    }
    pub fn with_allocations(mut self) -> Self {
        self.allocations = true;
        self
    }
    pub fn with_cache(mut self) -> Self {
        self.cache_and_instructions = true;
        self
    }
}

/// Declarative benchmark suite. Scenarios are measured eagerly as registered
/// (so a scenario closure borrowing shared `&mut` state is fine — only one is
/// live at a time), then [`BenchmarkSuite::finish_and_report`] composes them.
pub struct BenchmarkSuite {
    name: String,
    intent: MeasurementIntent,
    iters: Iters,
    interval_ns: u64,
    runner: Runner,
    env: RunEnv,
    samples: Vec<Sample>,        // back-to-back latency results
    load_samples: Vec<Sample>,   // open-loop results (if latency_under_load)
}

impl BenchmarkSuite {
    pub fn new(name: &str) -> Self {
        let runner = Runner::new();
        let env = RunEnv::detect(runner.calibration());
        Self {
            name: name.to_string(),
            intent: MeasurementIntent::default(),
            iters: Iters::STANDARD,
            interval_ns: 1_000, // 1µs default schedule for open-loop
            runner,
            env,
            samples: Vec::new(),
            load_samples: Vec::new(),
        }
    }

    pub fn set_intent(&mut self, intent: MeasurementIntent) -> &mut Self {
        self.intent = intent;
        self
    }

    pub fn iters(&mut self, iters: Iters) -> &mut Self {
        self.iters = iters;
        self
    }

    /// Target arrival rate for the open-loop (latency-under-load) pass.
    pub fn at_rate(&mut self, ops_per_sec: u64) -> &mut Self {
        if ops_per_sec > 0 {
            self.interval_ns = 1_000_000_000 / ops_per_sec;
        }
        self
    }

    /// Register and immediately measure a static-state scenario: `op` is the
    /// timed operation (warmup, fences, recording supplied by the suite). Runs
    /// once per enabled in-process regime (latency, and latency_under_load if a
    /// rate is set).
    pub fn run_scenario<Out>(&mut self, label: &str, mut op: impl FnMut() -> Out) {
        if self.intent.latency {
            let s = self.runner.bench_static(label, self.iters, &mut (), |_| op());
            self.samples.push(s);
        }
        if self.intent.latency_under_load {
            let s = self.runner.bench_open_loop(
                format!("{label}@load"),
                self.iters,
                self.interval_ns,
                &mut (),
                |_, _| (),
                |_, ()| op(),
            );
            self.load_samples.push(s);
        }
    }

    /// Register and measure a scenario with per-iteration setup (prepare/op
    /// split, §3.2). `state` is threaded through both closures.
    pub fn run_scenario_with<S, In, Out>(
        &mut self,
        label: &str,
        state: &mut S,
        mut prepare: impl FnMut(&mut S, usize) -> In,
        mut op: impl FnMut(&mut S, In) -> Out,
    ) {
        if self.intent.latency {
            let s = self.runner.bench(label, self.iters, state, &mut prepare, &mut op);
            self.samples.push(s);
        }
        if self.intent.latency_under_load {
            let s = self.runner.bench_open_loop(
                format!("{label}@load"),
                self.iters,
                self.interval_ns,
                state,
                &mut prepare,
                &mut op,
            );
            self.load_samples.push(s);
        }
    }

    /// Build the report (one section per regime, each stamped with how it was
    /// measured) and write md/csv/hdr artifacts to [`crate::out_dir`]
    /// (`bench-runs/` by default, `BENCH_OUT_DIR` to route). Prints RunEnv
    /// warnings and any requested-but-out-of-process regimes to stderr.
    pub fn finish_and_report(self) -> std::io::Result<crate::report::RunPaths> {
        let scope = format!(
            "engine-internal `op()` latency; {} scenario(s). \
             rdtscp-fenced, warmup {} / measure {}.",
            self.samples.len(),
            self.iters.warmup,
            self.iters.measure
        );
        let mut report = Report::new(&self.env, &self.name, &scope);
        if !self.samples.is_empty() {
            let refs: Vec<&Sample> = self.samples.iter().collect();
            report.section("Latency (back-to-back)", &refs);
        }
        if !self.load_samples.is_empty() {
            let refs: Vec<&Sample> = self.load_samples.iter().collect();
            report.section("Latency under load (open-loop, CO-corrected)", &refs);
        }

        for w in self.env.warnings() {
            eprintln!("⚠ {w}");
        }
        if self.intent.allocations {
            eprintln!(
                "note: allocation regime requested — run the dhat alloc-proof binary separately (feature `alloc`); it is a distinct pass from latency."
            );
        }
        if self.intent.cache_and_instructions {
            eprintln!(
                "note: cache/instruction regime requested — run `cargo bench --features iai --bench <crate>_iai`; callgrind cannot share this process."
            );
        }

        let paths = report.write_run(&self.name)?;
        eprintln!(
            "benchmark run written:\n  {}\n  {}\n  {}/*.hdr",
            paths.markdown.display(),
            paths.csv.display(),
            paths.hdr_dir.display()
        );
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    #[test]
    fn suite_runs_latency_scenarios() {
        let mut suite = BenchmarkSuite::new("test_suite");
        suite.iters(Iters::new(100, 1_000));
        let mut counter = 0u64;
        suite.run_scenario("inc", || {
            counter = counter.wrapping_add(1);
            black_box(counter)
        });
        assert_eq!(suite.samples.len(), 1);
        assert_eq!(suite.samples[0].hist.len(), 1_000);
    }

    #[test]
    fn intent_builder_composes() {
        let i = MeasurementIntent::latency().with_load().with_allocations();
        assert!(i.latency && i.latency_under_load && i.allocations);
        assert!(!i.cache_and_instructions);
    }

    #[test]
    fn at_rate_sets_interval() {
        let mut suite = BenchmarkSuite::new("t");
        suite.at_rate(1_000_000); // 1M ops/s → 1000ns interval
        assert_eq!(suite.interval_ns, 1_000);
    }
}
