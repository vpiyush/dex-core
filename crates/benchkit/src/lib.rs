//! `benchkit` — crate-agnostic benchmarking + reporting harness for dex-core.
//!
//! Extracts the warmup/measure `rdtscp` loop, machine-environment detection, and
//! report rendering out of each crate's `benches/*.rs` so a bench file contains
//! only what is genuinely crate-specific: the scenario bodies and the report
//! composition. See `docs/lld/benchkit.md` for the full design.
//!
//! Dev-tooling only — appears as a `[dev-dependencies]` entry, never in a
//! production binary. `telemetry` owns the measurement *primitives* (Histogram,
//! TSC clock); `benchkit` is a consumer of those primitives.
//!
//! ## Layout
//! - [`Runner`] — the warmup/measure `rdtscp` loop (back-to-back + open-loop).
//! - [`RunEnv`] — machine/process detection + methodology header.
//! - [`Report`] — section/delta composition + markdown/CSV/HDR export.
//! - [`Comparison`] — two-run A/B + regression with a noise-band verdict.
//! - [`BenchmarkSuite`] — the intent-driven facade over the above.
//! - [`Lcg`] — deterministic RNG for reproducible workloads.
//! - `alloc` (feature) — dhat-backed [`assert_zero_alloc`].

mod env;
mod iters;
mod lcg;
mod report;
mod runner;
mod stats;
mod suite;

#[cfg(feature = "alloc")]
mod alloc;

pub use env::{CorePrep, RunEnv, Turbo, run_id};
pub use iters::Iters;
pub use lcg::Lcg;
pub use report::{Comparison, Delta, Report, RunPaths, Verdict, out_dir};
pub use runner::Runner;
pub use stats::{ArrivalModel, RunStats, Sample};
pub use suite::{BenchmarkSuite, MeasurementIntent};

#[cfg(feature = "alloc")]
pub use alloc::{alloc_delta, assert_zero_alloc, measure_alloc, AllocDelta};
