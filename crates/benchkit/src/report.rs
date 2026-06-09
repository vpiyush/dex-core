//! Report assembly + multi-format export, and the two-run `Comparison` engine.

use std::fmt::Write as _;
use std::path::PathBuf;

use telemetry::primitives::Histogram;

use crate::env::RunEnv;
use crate::stats::{ArrivalModel, Sample};

/// A derived headline metric between two scenarios within one run.
#[derive(Clone, Copy)]
pub enum Delta {
    /// `lhs.p50 - rhs.p50`.
    Sub,
    /// `(lhs.p50 - rhs.p50) / n` — e.g. per-level marginal cost over 15 levels.
    SubPerN(u64),
}

/// Paths written by [`Report::write_run`].
pub struct RunPaths {
    pub markdown: PathBuf,
    pub csv: PathBuf,
    pub hdr_dir: PathBuf,
    /// A gnuplot script that renders a log-percentile latency overlay of every
    /// scenario from the `.hdr` files. Run it with `gnuplot <this>` to produce
    /// the sibling `.svg` (the xtask `plots` intent does this). It is a recipe,
    /// not an image — benchkit emits no chart itself (no Rust plotting dep).
    pub gnuplot: PathBuf,
    /// The SVG the gnuplot script writes to when run (does not exist until then).
    pub svg: PathBuf,
}

struct Section<'a> {
    title: String,
    samples: Vec<&'a Sample>,
}

struct DeltaRow {
    name: String,
    value: i64,
    note: String,
}

/// Composes a detected [`RunEnv`] + caller-grouped sections + caller-defined
/// deltas into markdown / CSV / HDR artifacts. Borrows `Sample`s; the caller
/// keeps the owning bindings so delta derivations read naturally.
pub struct Report<'a> {
    env: &'a RunEnv,
    title: String,
    scope: String,
    sections: Vec<Section<'a>>,
    deltas: Vec<DeltaRow>,
}

impl<'a> Report<'a> {
    pub fn new(env: &'a RunEnv, title: &str, scope: &str) -> Self {
        Report {
            env,
            title: title.to_string(),
            scope: scope.to_string(),
            sections: Vec::new(),
            deltas: Vec::new(),
        }
    }

    /// Add a group of scenarios under a heading. Each section is rendered with a
    /// "Measured under" stamp derived from its samples' arrival model (§3.8).
    pub fn section(&mut self, title: &str, samples: &[&'a Sample]) -> &mut Self {
        self.sections.push(Section {
            title: title.to_string(),
            samples: samples.to_vec(),
        });
        self
    }

    /// Derived headline metric `name = op(lhs.p50, rhs.p50)` with a one-line
    /// note. Debug-asserts both samples share an arrival model — you cannot
    /// subtract a back-to-back p50 from an open-loop p50 (transparency rule 4).
    pub fn delta(
        &mut self,
        name: &str,
        lhs: &'a Sample,
        rhs: &'a Sample,
        op: Delta,
        note: &str,
    ) -> &mut Self {
        debug_assert_eq!(
            lhs.arrival_model(),
            rhs.arrival_model(),
            "delta across different arrival models is meaningless"
        );
        let l = lhs.hist.p50().as_u64() as i64;
        let r = rhs.hist.p50().as_u64() as i64;
        let value = match op {
            Delta::Sub => l - r,
            Delta::SubPerN(n) => {
                if n == 0 {
                    0
                } else {
                    (l - r) / n as i64
                }
            }
        };
        self.deltas.push(DeltaRow {
            name: name.to_string(),
            value,
            note: note.to_string(),
        });
        self
    }

    pub fn to_markdown(&self) -> String {
        let mut s = self.env.header(&self.title, &self.scope);
        if !self.sections.is_empty() {
            let _ = writeln!(
                s,
                "- `inv-cs`/`maj-flt`: involuntary context switches / major page faults during that scenario's measure phase. Nonzero marks a tail spike as OS noise."
            );
        }
        for section in &self.sections {
            let _ = writeln!(s, "\n**{}**  _{}_\n", section.title, measured_under(section));
            let _ = writeln!(s, "| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |");
            let _ = writeln!(s, "|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
            for sample in &section.samples {
                let _ = writeln!(s, "{}", table_row(sample));
            }
        }
        if !self.deltas.is_empty() {
            let _ = writeln!(s, "\n**Headline deltas (p50, ns)**\n");
            let _ = writeln!(s, "| metric | ns | derivation |");
            let _ = writeln!(s, "|---|--:|---|");
            for d in &self.deltas {
                let _ = writeln!(s, "| {} | {} | {} |", d.name, d.value, d.note);
            }
        }
        s
    }

    /// One CSV row per sample (across all sections), via `Histogram::render_csv`,
    /// with a leading `regime` column so an external consumer can't misattribute.
    /// The rusage noise counters are APPENDED after the histogram fields so the
    /// p50/p99 column indices stay stable for existing parsers (`Comparison`,
    /// xtask's compare mode) and for old CSVs compared against new ones.
    pub fn to_csv(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "regime,{},ctx_voluntary,ctx_involuntary,minor_faults,major_faults",
            Histogram::csv_header()
        );
        for section in &self.sections {
            for sample in &section.samples {
                let st = &sample.stats;
                let _ = writeln!(
                    s,
                    "{},{},{},{},{},{}",
                    regime_tag(sample.arrival_model()),
                    sample.hist.render_csv(),
                    st.ctx_voluntary,
                    st.ctx_involuntary,
                    st.minor_faults,
                    st.major_faults,
                );
            }
        }
        s
    }

    /// Write `<dir>/<name>_<id>.{md,csv}` and `<dir>/<name>_<id>_hdr/<scenario>.hdr`,
    /// where `<id>` is [`RunEnv::run_id`] — the git short sha (`-dirty` suffixed on
    /// a modified tree), falling back to the UTC timestamp off-git.
    ///
    /// Directory resolution, in priority order: (1) `BENCH_OUT_DIR` env var, if
    /// set (used verbatim); (2) else `<workspace-root>/<dir>`, where the
    /// workspace root is the nearest ancestor whose `Cargo.toml` declares
    /// `[workspace]`.
    /// This makes every crate's `cargo bench -p <crate>` land in ONE shared
    /// `bench-runs/` at the workspace root, regardless of the per-crate cwd that
    /// cargo sets. Falls back to `dir` relative to cwd if no workspace is found.
    pub fn write_run(&self, dir: &str, name: &str) -> std::io::Result<RunPaths> {
        let dir = match std::env::var("BENCH_OUT_DIR") {
            Ok(d) => d,
            Err(_) => match workspace_root() {
                Some(root) => root.join(dir).to_string_lossy().into_owned(),
                None => dir.to_string(),
            },
        };
        std::fs::create_dir_all(&dir)?;
        // Artifacts are keyed by the code that produced them (git sha, or the
        // timestamp off-git), so a rerun at the same clean commit overwrites in
        // place — one commit, one canonical set of numbers.
        let id = self.env.run_id();
        let md = PathBuf::from(format!("{dir}/{name}_{id}.md"));
        let csv = PathBuf::from(format!("{dir}/{name}_{id}.csv"));
        let hdr_dir = PathBuf::from(format!("{dir}/{name}_{id}_hdr"));

        std::fs::write(&md, self.to_markdown())?;
        std::fs::write(&csv, self.to_csv())?;
        // Replace the hdr dir wholesale: a stale .hdr from a renamed scenario in
        // an earlier run at this id must not survive into the new artifact set.
        let _ = std::fs::remove_dir_all(&hdr_dir);
        std::fs::create_dir_all(&hdr_dir)?;
        let mut safe_labels = Vec::new();
        for section in &self.sections {
            for sample in &section.samples {
                let safe = sanitize(sample.hist.label());
                std::fs::write(hdr_dir.join(format!("{safe}.hdr")), sample.hist.render_hdr_percentiles(5))?;
                safe_labels.push(safe);
            }
        }

        // Emit a gnuplot script (a recipe, not an image): a log-percentile
        // latency overlay of every scenario, reading the .hdr files written
        // above. Render with `gnuplot <script>` → the sibling .svg.
        let gnuplot = PathBuf::from(format!("{dir}/{name}_{id}.gnuplot"));
        let svg = PathBuf::from(format!("{dir}/{name}_{id}.svg"));
        let hdr_dirname = format!("{name}_{id}_hdr");
        let svg_name = format!("{name}_{id}.svg");
        std::fs::write(&gnuplot, gnuplot_script(&self.title, &hdr_dirname, &svg_name, &safe_labels))?;

        Ok(RunPaths { markdown: md, csv, hdr_dir, gnuplot, svg })
    }
}

fn sanitize(label: &str) -> String {
    label.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// A self-contained gnuplot script rendering a log-percentile latency curve
/// (one line per scenario) to SVG. Paths are RELATIVE to the script's own
/// directory, so it must be run from `bench-runs/` (the xtask `plots` intent
/// sets cwd accordingly). X = `1/(1-percentile)` on a log scale, so p99 / p99.99
/// are legible rather than crushed at the right edge — the standard HdrHistogram
/// plot shape. Column 4 of each `.hdr` is exactly that x value; column 1 is the
/// latency in ns.
fn gnuplot_script(title: &str, hdr_dirname: &str, svg_name: &str, labels: &[String]) -> String {
    let mut s = String::new();
    s.push_str("# benchkit latency curve — render with: gnuplot <this file> (run from bench-runs/)\n");
    s.push_str("set terminal svg size 1100,680 font 'sans,11'\n");
    s.push_str(&format!("set output '{svg_name}'\n"));
    s.push_str(&format!("set title \"{} — latency by percentile\"\n", gp_escape(title)));
    s.push_str("set xlabel 'Percentile'\n");
    s.push_str("set ylabel 'Latency (ns)'\n");
    s.push_str("set logscale x\n");
    s.push_str("set grid\n");
    s.push_str("set key outside right top\n");
    // Label the log x-axis with familiar percentiles instead of raw 1/(1-p).
    s.push_str("set xtics ('p50' 2, 'p90' 10, 'p99' 100, 'p99.9' 1000, 'p99.99' 10000, 'p99.999' 100000)\n");
    // Each .hdr: col1=Value(ns), col4=1/(1-percentile). Skip '#' comment lines.
    s.push_str("set datafile commentschars '#'\n");
    s.push_str("plot \\\n");
    let n = labels.len();
    for (i, label) in labels.iter().enumerate() {
        let sep = if i + 1 < n { ", \\" } else { "" };
        s.push_str(&format!(
            "  '{hdr_dirname}/{label}.hdr' using 4:1 with lines lw 2 title '{}'{sep}\n",
            gp_escape(label)
        ));
    }
    if labels.is_empty() {
        s.push_str("  NaN notitle\n");
    }
    s
}

/// Escape characters that would break a gnuplot double-quoted string / title.
fn gp_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\'', "")
}

/// Walk up from the current directory to the workspace root — the nearest
/// ancestor whose `Cargo.toml` contains a `[workspace]` table. Returns `None`
/// off a cargo tree (e.g. an odd cwd), letting the caller fall back.
fn workspace_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if std::fs::read_to_string(&manifest).is_ok_and(|c| c.contains("[workspace]")) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn regime_tag(m: ArrivalModel) -> &'static str {
    match m {
        ArrivalModel::BackToBack => "back_to_back",
        ArrivalModel::OpenLoop { .. } => "open_loop",
    }
}

/// The single-regime "Measured under" stamp for a section. If a section somehow
/// mixes models, say so loudly rather than picking one silently.
fn measured_under(section: &Section) -> String {
    let mut models: Vec<ArrivalModel> = section.samples.iter().map(|s| s.arrival_model()).collect();
    models.dedup();
    let overloaded = section.samples.iter().any(|s| s.overloaded);
    let base = match section.samples.first().map(|s| s.arrival_model()) {
        Some(ArrivalModel::BackToBack) => "measured under: back-to-back (service time)".to_string(),
        Some(ArrivalModel::OpenLoop { interval_ns }) => {
            format!("measured under: open-loop @ {interval_ns}ns interval (CO-corrected latency under load)")
        }
        None => "no samples".to_string(),
    };
    let mixed = section.samples.iter().any(|s| s.arrival_model() != section.samples[0].arrival_model());
    let mut stamp = base;
    if mixed {
        stamp.push_str(" ⚠ MIXED arrival models in one section");
    }
    if overloaded {
        stamp.push_str(" ⚠ OVERLOADED (target rate unachievable)");
    }
    stamp
}

fn humanize(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn table_row(sample: &Sample) -> String {
    let h = &sample.hist;
    let p50 = h.p50().as_u64();
    let tail = if p50 > 0 { h.p99_99().as_u64() as f64 / p50 as f64 } else { 0.0 };
    format!(
        "| {} | {} | {} | {} | {} | {} | {:.0}× | {:.2}M | {} | {} |",
        h.label(),
        humanize(h.len()),
        h.min().as_u64(),
        p50,
        h.p99().as_u64(),
        h.p99_99().as_u64(),
        tail,
        h.throughput_per_sec() / 1e6,
        sample.stats.ctx_involuntary,
        sample.stats.major_faults,
    )
}

// ---- Comparison: A/B + regression, one engine -----------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Improved,
    Regressed,
    WithinNoise,
    /// Scenario present in one CSV but not the other, or regimes differ.
    Incomparable,
}

struct CompRow {
    label: String,
    regime: String,
    base: Option<Row>,
    cur: Option<Row>,
    verdict: Verdict,
    /// The percentile that decided a non-noise verdict (worst regression, or
    /// best improvement when nothing regressed).
    driver: Option<&'static str>,
}

/// Compares two benchmark CSVs (each a `Report::to_csv` output). Serves both CI
/// regression (baseline = previous commit) and A/B testing (baseline = the other
/// implementation). Matches rows by (label, regime); a within-`noise_pct` delta
/// is `WithinNoise`, never a win/loss — the anti-self-deception guard.
///
/// The verdict is the WORST metric across p50, p99, and p99.99: a flat p50
/// cannot mask a blown-out tail, and a p50 improvement does not excuse one.
/// Anything else would contradict the "honest tails" thesis the reports make.
pub struct Comparison {
    rows: Vec<CompRow>,
    noise_pct: f64,
}

impl Comparison {
    pub fn load(baseline_csv: &str, current_csv: &str, noise_pct: f64) -> std::io::Result<Self> {
        let base = std::fs::read_to_string(baseline_csv)?;
        let cur = std::fs::read_to_string(current_csv)?;
        Ok(Self::from_strings(&base, &cur, noise_pct))
    }

    /// Parse two CSV bodies directly (testable without files).
    pub fn from_strings(baseline_csv: &str, current_csv: &str, noise_pct: f64) -> Self {
        let base = parse_csv(baseline_csv);
        let cur = parse_csv(current_csv);

        let mut rows = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (key, b) in &base {
            seen.insert(key.clone());
            let (label, regime) = key.clone();
            match cur.get(key) {
                Some(c) => {
                    let (verdict, driver) = classify_pair(b, c, noise_pct);
                    rows.push(CompRow { label, regime, base: Some(*b), cur: Some(*c), verdict, driver });
                }
                None => rows.push(CompRow {
                    label,
                    regime,
                    base: Some(*b),
                    cur: None,
                    verdict: Verdict::Incomparable,
                    driver: None,
                }),
            }
        }
        // Scenarios new in current (no baseline).
        for (key, c) in &cur {
            if !seen.contains(key) {
                let (label, regime) = key.clone();
                rows.push(CompRow { label, regime, base: None, cur: Some(*c), verdict: Verdict::Incomparable, driver: None });
            }
        }
        rows.sort_by(|a, b| a.label.cmp(&b.label));
        Comparison { rows, noise_pct }
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Benchmark comparison (noise band ±{:.1}%)\n", self.noise_pct);
        let _ = writeln!(s, "Each cell is `base→current (Δ%)` in ns. The verdict is the worst metric of the three.\n");
        let _ = writeln!(s, "| scenario | regime | p50 | p99 | p99.99 | verdict |");
        let _ = writeln!(s, "|---|---|--:|--:|--:|---|");
        for r in &self.rows {
            let verdict = match (r.verdict, r.driver) {
                (Verdict::Improved, Some(m)) => format!("✓ improved ({m})"),
                (Verdict::Improved, None) => "✓ improved".into(),
                (Verdict::Regressed, Some(m)) => format!("✗ REGRESSED ({m})"),
                (Verdict::Regressed, None) => "✗ REGRESSED".into(),
                (Verdict::WithinNoise, _) => "· within noise".into(),
                (Verdict::Incomparable, _) => "? incomparable".into(),
            };
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} |",
                r.label,
                r.regime,
                delta_cell(r.base.map(|b| b.p50), r.cur.map(|c| c.p50)),
                delta_cell(r.base.map(|b| b.p99), r.cur.map(|c| c.p99)),
                delta_cell(r.base.map(|b| b.p99_99), r.cur.map(|c| c.p99_99)),
                verdict,
            );
        }
        s
    }

    /// True if any scenario regressed beyond the noise band — the CI fail signal.
    pub fn any_regressed(&self) -> bool {
        self.rows.iter().any(|r| r.verdict == Verdict::Regressed)
    }
}

#[derive(Clone, Copy)]
struct Row {
    p50: u64,
    p99: u64,
    p99_99: u64,
}

/// Worst-of-three classification. Per metric: within the noise band is neutral,
/// above it is a regression, below it an improvement. Any regression makes the
/// row `Regressed` (driver = the largest one); otherwise any improvement makes
/// it `Improved` (driver = the largest one); otherwise `WithinNoise`. A zero
/// baseline metric makes the row `Incomparable` (no meaningful percentage).
fn classify_pair(base: &Row, cur: &Row, noise_pct: f64) -> (Verdict, Option<&'static str>) {
    let pairs = [
        ("p50", base.p50, cur.p50),
        ("p99", base.p99, cur.p99),
        ("p99.99", base.p99_99, cur.p99_99),
    ];
    if pairs.iter().any(|&(_, b, _)| b == 0) {
        return (Verdict::Incomparable, None);
    }
    let mut worst_regression: Option<(&'static str, f64)> = None;
    let mut best_improvement: Option<(&'static str, f64)> = None;
    for (name, b, c) in pairs {
        let change = (c as f64 - b as f64) / b as f64 * 100.0;
        if change.abs() <= noise_pct {
            continue;
        }
        if change > 0.0 {
            if worst_regression.is_none_or(|(_, w)| change > w) {
                worst_regression = Some((name, change));
            }
        } else if best_improvement.is_none_or(|(_, w)| change < w) {
            best_improvement = Some((name, change));
        }
    }
    match (worst_regression, best_improvement) {
        (Some((m, _)), _) => (Verdict::Regressed, Some(m)),
        (None, Some((m, _))) => (Verdict::Improved, Some(m)),
        (None, None) => (Verdict::WithinNoise, None),
    }
}

/// One `base→current (Δ%)` table cell; `—` stands in for a missing side.
fn delta_cell(base: Option<u64>, cur: Option<u64>) -> String {
    match (base, cur) {
        (Some(b), Some(c)) => {
            if b > 0 {
                format!("{b}→{c} ({:+.1}%)", (c as f64 - b as f64) / b as f64 * 100.0)
            } else {
                format!("{b}→{c}")
            }
        }
        (Some(b), None) => format!("{b}→—"),
        (None, Some(c)) => format!("—→{c}"),
        (None, None) => "—".into(),
    }
}

/// Parse a `Report::to_csv` body into a map keyed by (label, regime).
/// Header: `regime,label,count,min_ns,p50_ns,p99_ns,p99_99_ns,max_ns,msgs_per_sec`,
/// optionally followed by the appended rusage columns (ignored here). All three
/// tracked percentiles must parse for a row to participate.
fn parse_csv(body: &str) -> std::collections::HashMap<(String, String), Row> {
    let mut map = std::collections::HashMap::new();
    for line in body.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        // regime, label, count, min, p50, p99, p99_99, max, msgs/s, [rusage…]
        if f.len() < 9 {
            continue;
        }
        let regime = f[0].to_string();
        let label = f[1].to_string();
        if let (Ok(p50), Ok(p99), Ok(p99_99)) =
            (f[4].parse::<u64>(), f[5].parse::<u64>(), f[6].parse::<u64>())
        {
            map.insert((label, regime), Row { p50, p99, p99_99 });
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(rows: &[(&str, &str, u64, u64, u64)]) -> String {
        let mut s = format!("regime,{}\n", Histogram::csv_header());
        for (regime, label, p50, p99, p99_99) in rows {
            // regime,label,count,min,p50,p99,p99_99,max,msgs/s
            s.push_str(&format!("{regime},{label},1000,1,{p50},{p99},{p99_99},{p99_99},0\n"));
        }
        s
    }

    #[test]
    fn noise_counters_render_in_markdown_and_csv() {
        use crate::stats::{ArrivalModel, RunStats, Sample};
        use telemetry::primitives::{calibrate, Histogram as TelHistogram, Nanos};

        let env = crate::env::RunEnv::detect(&calibrate());
        let mut hist = TelHistogram::new("noisy");
        hist.record(Nanos(100));
        let sample = Sample {
            hist,
            arrival_model: ArrivalModel::BackToBack,
            stats: RunStats { ctx_involuntary: 7, major_faults: 3, ..Default::default() },
            overloaded: false,
        };
        let mut report = Report::new(&env, "t", "scope");
        report.section("S", &[&sample]);

        let md = report.to_markdown();
        assert!(md.contains("| inv-cs | maj-flt |"), "table gains the noise columns");
        assert!(md.contains("| 7 | 3 |"), "the scenario's own counters render");

        let csv = report.to_csv();
        let row = csv.lines().nth(1).unwrap();
        assert_eq!(row.split(',').count(), 13, "regime + 8 hist fields + 4 rusage");
        assert!(row.ends_with(",0,7,0,3"), "rusage appended in vol,invol,minor,major order");
    }

    #[test]
    fn within_noise_is_not_a_win() {
        let base = csv(&[("back_to_back", "cross", 100, 100, 100)]);
        let cur = csv(&[("back_to_back", "cross", 101, 101, 101)]); // +1%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed());
        assert!(c.to_markdown().contains("within noise"));
    }

    #[test]
    fn regression_beyond_band_is_flagged() {
        let base = csv(&[("back_to_back", "cross", 100, 100, 100)]);
        let cur = csv(&[("back_to_back", "cross", 130, 130, 130)]); // +30%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(c.any_regressed());
    }

    #[test]
    fn improvement_detected() {
        let base = csv(&[("back_to_back", "cross", 100, 100, 100)]);
        let cur = csv(&[("back_to_back", "cross", 70, 70, 70)]); // -30%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed());
        assert!(c.to_markdown().contains("improved"));
    }

    #[test]
    fn same_label_different_regime_does_not_match() {
        // A back-to-back baseline and an open-loop current for the "same" label
        // are different keys → each shows as incomparable, never subtracted.
        let base = csv(&[("back_to_back", "cross", 100, 100, 100)]);
        let cur = csv(&[("open_loop", "cross", 500, 500, 500)]);
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed(), "cross-regime must not be read as a regression");
        let md = c.to_markdown();
        assert_eq!(md.matches("incomparable").count(), 2);
    }

    #[test]
    fn tail_regression_caught_when_p50_flat() {
        // p50 identical, p99 inside the band, p99.99 doubled: the old p50-only
        // comparison called this clean. It must fail, and name the driver.
        let base = csv(&[("back_to_back", "cross", 100, 200, 400)]);
        let cur = csv(&[("back_to_back", "cross", 100, 205, 800)]);
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(c.any_regressed(), "a doubled p99.99 must fail even with a flat p50");
        assert!(c.to_markdown().contains("REGRESSED (p99.99)"));
    }

    #[test]
    fn p50_improvement_does_not_mask_tail_regression() {
        // Halved p50, doubled p99.99: worst metric wins, so this is a regression.
        let base = csv(&[("back_to_back", "cross", 100, 200, 400)]);
        let cur = csv(&[("back_to_back", "cross", 50, 200, 800)]);
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(c.any_regressed(), "an improved p50 must not excuse a blown tail");
        assert!(c.to_markdown().contains("REGRESSED (p99.99)"));
    }
}
