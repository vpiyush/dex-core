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

    pub fn merge(&mut self, other: &Self) {
        self.inner.add(&other.inner).expect("failed to add histogram");
    }

    pub fn render_markdown(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            self.label,
            self.len(),
            self.min().as_u64(),
            self.p50().as_u64(),
            self.p99().as_u64(),
            self.p99_99().as_u64(),
            self.max().as_u64(),
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
}
