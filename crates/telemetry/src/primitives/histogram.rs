use crate::primitives::Nanos;

#[cfg(feature = "histogram")]
pub struct Histogram {
    inner: hdrhistogram::Histogram<u64>,
    label: String
}

#[cfg(feature = "histogram")]
impl Histogram {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            inner: hdrhistogram::Histogram::<u64>::new_with_max(u64::MAX, 3).expect("failed to create histogram"),
            label: label.into(),
        }
    }

    #[inline(always)]
    pub fn record(&mut self, value: Nanos) {
        self.inner.record(value.as_u64()).expect("Nanos cannot exceed histogram bounds");
    }

    pub fn len(&self) -> u64 {
        self.inner.len()
    }

    /// The label this histogram was constructed with.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn min(&self) -> Nanos {
        Nanos(self.inner.min())
    }

    pub fn max(&self) -> Nanos {
        Nanos(self.inner.max())
    }

    pub fn p50(&self) -> Nanos {
        Nanos(self.inner.value_at_percentile(50.0))
    }

    pub fn p99(&self) -> Nanos {
        Nanos(self.inner.value_at_percentile(99.0))
    }

    pub fn p99_99(&self) -> Nanos {
        Nanos(self.inner.value_at_percentile(99.99))
    }

    pub fn percentile(&self, p: f64) -> Nanos {
        Nanos(self.inner.value_at_percentile(p))
    }

    /// Arithmetic mean of the recorded latencies, in nanoseconds.
    pub fn mean(&self) -> f64 {
        self.inner.mean()
    }

    /// Single-core throughput *implied by the mean latency* (`1 / mean`), in
    /// messages per second.
    ///
    /// This is an inverse-of-latency figure — ops/sec if operations ran
    /// back-to-back at the average service time on one core. It is an **upper
    /// bound**, not measured sustained throughput: the per-op samples are
    /// individually fenced, so they exclude loop/dispatch/IPC overhead and the
    /// gaps between timed regions. For honest sustained throughput, measure
    /// wall-clock around the whole batch and divide the op count by it (the
    /// aggregator does this; see `docs/lld/telemetry.md` §4.4).
    ///
    /// Returns `0.0` for an empty histogram or a non-positive mean.
    pub fn throughput_per_sec(&self) -> f64 {
        let mean_ns = self.mean();
        if self.len() == 0 || mean_ns <= 0.0 {
            0.0
        } else {
            1_000_000_000.0 / mean_ns
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.inner.add(&other.inner).expect("failed to add histogram");
    }

    /// CSV header matching [`Self::render_csv`]'s column order.
    pub fn csv_header() -> &'static str {
        "label,count,min_ns,p50_ns,p99_ns,p99_99_ns,max_ns,msgs_per_sec"
    }

    /// Render one CSV data row (no trailing newline) for machine consumption —
    /// re-plotting or CI regression tracking. Same numbers as
    /// [`Self::render_markdown`].
    pub fn render_csv(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{:.0}",
            self.label,
            self.len(),
            self.min().as_u64(),
            self.p50().as_u64(),
            self.p99().as_u64(),
            self.p99_99().as_u64(),
            self.max().as_u64(),
            self.throughput_per_sec(),
        )
    }

    /// Render a single markdown table row. Column order:
    /// `| label | count | min | p50 | p99 | p99.99 | max | msg/s |`
    /// — latency columns are nanoseconds; `msg/s` is the derived single-core
    /// upper bound (`1 / mean`, see [`Self::throughput_per_sec`]), so a
    /// truthful header reads e.g. `msg/s (1/mean, 1-core)`.
    pub fn render_markdown(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {:.0} |",
            self.label,
            self.len(),
            self.min().as_u64(),
            self.p50().as_u64(),
            self.p99().as_u64(),
            self.p99_99().as_u64(),
            self.max().as_u64(),
            self.throughput_per_sec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_distribution_percentiles_are_correct() {
        let mut h = Histogram::new("test");
        for i in 1..=10_000u64 { h.record(Nanos(i)); }
        // 3-sigfig precision → ~0.1% tolerance
        assert!((h.p50().as_u64() as i64 - 5000).abs() < 10);
        assert!((h.p99().as_u64() as i64 - 9900).abs() < 20);
    }

    #[test]
    fn merge_equals_combined_record() {
        let mut a = Histogram::new("a");
        let mut b = Histogram::new("b");
        let mut c = Histogram::new("c");
        for i in 1..=5_000u64   { a.record(Nanos(i)); c.record(Nanos(i)); }
        for i in 5_001..=10_000 { b.record(Nanos(i)); c.record(Nanos(i)); }
        a.merge(&b);
        assert_eq!(a.len(), c.len());
        assert_eq!(a.p50().as_u64(), c.p50().as_u64());
        assert_eq!(a.p99().as_u64(), c.p99().as_u64());
    }

    #[test]
    fn record_zero_does_not_panic() {
        let mut h = Histogram::new("test");
        h.record(Nanos(0));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn percentile_takes_percent_not_quantile() {
        // Regression: ensure we divide by 100. If someone "fixes" this,
        // percentile(50.0) starts returning max, and this test fires.
        let mut h = Histogram::new("test");
        for i in 1..=10_000u64 { h.record(Nanos(i)); }
        assert_eq!(h.percentile(50.0).as_u64(), h.p50().as_u64());
        assert_eq!(h.percentile(99.0).as_u64(), h.p99().as_u64());
    }

    #[test]
    fn throughput_is_inverse_of_mean() {
        // constant 500 ns service time → mean ≈ 500 ns → 1e9 / 500 = 2,000,000 msg/s
        let mut h = Histogram::new("test");
        for _ in 0..1_000 { h.record(Nanos(500)); }
        assert!(
            (h.throughput_per_sec() - 2_000_000.0).abs() < 20_000.0,
            "got {}",
            h.throughput_per_sec()
        );
    }

    #[test]
    fn throughput_empty_is_zero() {
        // Guards the divide-by-zero path: an empty histogram has mean 0.
        assert_eq!(Histogram::new("test").throughput_per_sec(), 0.0);
    }

    #[test]
    fn render_csv_fields_match_header() {
        let mut h = Histogram::new("my_label");
        for i in 1..=1000u64 {
            h.record(Nanos(i));
        }
        let row = h.render_csv();
        let fields: Vec<&str> = row.split(',').collect();
        assert_eq!(fields.len(), Histogram::csv_header().split(',').count());
        assert_eq!(fields[0], "my_label");
        assert_eq!(fields[1], "1000"); // count
    }
}
