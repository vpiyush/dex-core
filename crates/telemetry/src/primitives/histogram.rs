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

    /// Record `value`, back-filling synthetic samples for an expected arrival
    /// interval — the coordinated-omission correction. If `value` exceeds
    /// `expected_interval`, hdrhistogram adds the samples that *would* have been
    /// recorded had ops kept arriving every `expected_interval` ns during the
    /// stall. Used by the open-loop benchmark runner; for back-to-back recording
    /// use [`Histogram::record`].
    #[inline(always)]
    pub fn record_correct(&mut self, value: Nanos, expected_interval: Nanos) {
        self.inner
            .record_correct(value.as_u64(), expected_interval.as_u64())
            .expect("Nanos cannot exceed histogram bounds");
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

    /// Render the HDR **percentile distribution** in the text format consumed
    /// by the online plotter at <https://hdrhistogram.github.io/HdrHistogram/>.
    /// Paste the file there to get the canonical log-percentile latency curve.
    ///
    /// Columns: `Value  Percentile  TotalCount  1/(1-Percentile)`. The trailing
    /// `1/(1-Percentile)` is what gives the plotter its logarithmic x-axis so
    /// the tail (p99, p99.99, …) is legible rather than crushed against the edge.
    /// `ticks` = points per halving of the tail distance (5 matches the Java
    /// reference's default density).
    pub fn render_hdr_percentiles(&self, ticks: u32) -> String {
        let mut s = String::new();
        s.push_str(&format!("# {}\n", self.label));
        s.push_str("       Value     Percentile TotalCount 1/(1-Percentile)\n\n");
        let mut running_total: u64 = 0;
        for v in self.inner.iter_quantiles(ticks) {
            running_total += v.count_since_last_iteration();
            let q = v.quantile();
            let inv = if q < 1.0 { 1.0 / (1.0 - q) } else { f64::INFINITY };
            s.push_str(&format!(
                "{:12} {:.12} {:10} {:14.2}\n",
                v.value_iterated_to(),
                q,
                running_total,
                inv,
            ));
        }
        s.push_str(&format!(
            "#[Mean    = {:12.2}, Total count  = {:12}]\n",
            self.inner.mean(),
            self.inner.len(),
        ));
        s.push_str(&format!("#[Max     = {:12}]\n", self.inner.max()));
        s
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
    fn record_correct_backfills_coordinated_omission() {
        // One op stalls for 100× the expected interval. record_correct must
        // synthesize the missed in-between samples, so the tail is dragged up
        // and the count grows beyond the literal number of record calls.
        let interval = Nanos(10);
        let mut plain = Histogram::new("plain");
        let mut corrected = Histogram::new("corrected");
        for _ in 0..999 {
            plain.record(interval);
            corrected.record_correct(interval, interval);
        }
        plain.record(Nanos(1_000));
        corrected.record_correct(Nanos(1_000), interval);

        // Plain: exactly 1000 samples, p99.99 sees the lone spike modestly.
        assert_eq!(plain.len(), 1_000);
        // Corrected: the stall back-fills ~99 synthetic samples (1000/10 - 1).
        assert!(corrected.len() > plain.len(), "CO correction adds samples: {} vs {}", corrected.len(), plain.len());
        // The corrected p99 is pulled up by the back-filled latencies.
        assert!(corrected.p99().as_u64() > plain.p99().as_u64(),
            "corrected p99 {} should exceed plain p99 {}", corrected.p99().as_u64(), plain.p99().as_u64());
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
    fn hdr_percentiles_well_formed() {
        let mut h = Histogram::new("hdr");
        for i in 1..=10_000u64 {
            h.record(Nanos(i));
        }
        let out = h.render_hdr_percentiles(5);
        // Header line the online plotter keys off of.
        assert!(out.contains("Value     Percentile TotalCount"));
        // Last data row's running total must equal total sample count.
        let last_data = out
            .lines()
            .rfind(|l| !l.starts_with('#') && !l.trim().is_empty() && !l.contains("Percentile"))
            .unwrap();
        let total: u64 = last_data.split_whitespace().nth(2).unwrap().parse().unwrap();
        assert_eq!(total, 10_000, "running TotalCount must reach the sample count");
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
