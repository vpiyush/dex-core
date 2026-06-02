//! Warmup + measured iteration counts for one scenario.

/// How many iterations to run un-recorded (warmup) before recording begins, and
/// how many to record (measure). Warmup lets caches, branch predictors, and
/// any first-touch allocation settle so the recorded samples reflect steady
/// state rather than cold-start cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Iters {
    pub warmup: usize,
    pub measure: usize,
}

impl Iters {
    /// 100k warmup / 1M measure — the matcher/orderbook default for cheap ops.
    pub const STANDARD: Iters = Iters { warmup: 100_000, measure: 1_000_000 };

    /// 50k warmup / 500k measure — for scenarios with per-iteration setup
    /// (replenish/cross), where each measured op costs more wall-clock.
    pub const LIGHT: Iters = Iters { warmup: 50_000, measure: 500_000 };

    pub fn new(warmup: usize, measure: usize) -> Self {
        Self { warmup, measure }
    }

    /// Total iterations the loop runs (warmup then measure).
    #[inline]
    pub fn total(self) -> usize {
        self.warmup + self.measure
    }
}
