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
        for section in &self.sections {
            let _ = writeln!(s, "\n**{}**  _{}_\n", section.title, measured_under(section));
            let _ = writeln!(s, "| scenario | n | min | p50 | p99 | p99.99 | tail | M/s |");
            let _ = writeln!(s, "|---|--:|--:|--:|--:|--:|--:|--:|");
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
    pub fn to_csv(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "regime,{}", Histogram::csv_header());
        for section in &self.sections {
            for sample in &section.samples {
                let _ = writeln!(s, "{},{}", regime_tag(sample.arrival_model()), sample.hist.render_csv());
            }
        }
        s
    }

    /// Write `<dir>/<name>_<stamp>.{md,csv}` and `<dir>/<name>_<stamp>_hdr/<scenario>.hdr`.
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
        let stamp = &self.env.stamp;
        let md = PathBuf::from(format!("{dir}/{name}_{stamp}.md"));
        let csv = PathBuf::from(format!("{dir}/{name}_{stamp}.csv"));
        let hdr_dir = PathBuf::from(format!("{dir}/{name}_{stamp}_hdr"));

        std::fs::write(&md, self.to_markdown())?;
        std::fs::write(&csv, self.to_csv())?;
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
        let gnuplot = PathBuf::from(format!("{dir}/{name}_{stamp}.gnuplot"));
        let svg = PathBuf::from(format!("{dir}/{name}_{stamp}.svg"));
        let hdr_dirname = format!("{name}_{stamp}_hdr");
        let svg_name = format!("{name}_{stamp}.svg");
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
        "| {} | {} | {} | {} | {} | {} | {:.0}× | {:.2}M |",
        h.label(),
        humanize(h.len()),
        h.min().as_u64(),
        p50,
        h.p99().as_u64(),
        h.p99_99().as_u64(),
        tail,
        h.throughput_per_sec() / 1e6,
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
    base_p50: Option<u64>,
    cur_p50: Option<u64>,
    verdict: Verdict,
}

/// Compares two benchmark CSVs (each a `Report::to_csv` output). Serves both CI
/// regression (baseline = previous commit) and A/B testing (baseline = the other
/// implementation). Matches rows by (label, regime); a within-`noise_pct` delta
/// is `WithinNoise`, never a win/loss — the anti-self-deception guard.
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
                    let verdict = classify(b.p50, c.p50, noise_pct);
                    rows.push(CompRow { label, regime, base_p50: Some(b.p50), cur_p50: Some(c.p50), verdict });
                }
                None => rows.push(CompRow {
                    label,
                    regime,
                    base_p50: Some(b.p50),
                    cur_p50: None,
                    verdict: Verdict::Incomparable,
                }),
            }
        }
        // Scenarios new in current (no baseline).
        for (key, c) in &cur {
            if !seen.contains(key) {
                let (label, regime) = key.clone();
                rows.push(CompRow { label, regime, base_p50: None, cur_p50: Some(c.p50), verdict: Verdict::Incomparable });
            }
        }
        rows.sort_by(|a, b| a.label.cmp(&b.label));
        Comparison { rows, noise_pct }
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Benchmark comparison (noise band ±{:.1}%)\n", self.noise_pct);
        let _ = writeln!(s, "| scenario | regime | base p50 | cur p50 | Δ% | verdict |");
        let _ = writeln!(s, "|---|---|--:|--:|--:|---|");
        for r in &self.rows {
            let delta_pct = match (r.base_p50, r.cur_p50) {
                (Some(b), Some(c)) if b > 0 => format!("{:+.1}%", (c as f64 - b as f64) / b as f64 * 100.0),
                _ => "—".into(),
            };
            let verdict = match r.verdict {
                Verdict::Improved => "✓ improved",
                Verdict::Regressed => "✗ REGRESSED",
                Verdict::WithinNoise => "· within noise",
                Verdict::Incomparable => "? incomparable",
            };
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} |",
                r.label,
                r.regime,
                r.base_p50.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                r.cur_p50.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                delta_pct,
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

struct Row {
    p50: u64,
}

fn classify(base: u64, cur: u64, noise_pct: f64) -> Verdict {
    if base == 0 {
        return Verdict::Incomparable;
    }
    let change = (cur as f64 - base as f64) / base as f64 * 100.0;
    if change.abs() <= noise_pct {
        Verdict::WithinNoise
    } else if change < 0.0 {
        Verdict::Improved
    } else {
        Verdict::Regressed
    }
}

/// Parse a `Report::to_csv` body into a map keyed by (label, regime).
/// Header: `regime,label,count,min_ns,p50_ns,p99_ns,p99_99_ns,max_ns,msgs_per_sec`.
fn parse_csv(body: &str) -> std::collections::HashMap<(String, String), Row> {
    let mut map = std::collections::HashMap::new();
    for line in body.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        // regime, label, count, min, p50, p99, p99_99, max, msgs/s
        if f.len() < 9 {
            continue;
        }
        let regime = f[0].to_string();
        let label = f[1].to_string();
        if let Ok(p50) = f[4].parse::<u64>() {
            map.insert((label, regime), Row { p50 });
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(rows: &[(&str, &str, u64)]) -> String {
        let mut s = format!("regime,{}\n", Histogram::csv_header());
        for (regime, label, p50) in rows {
            // regime,label,count,min,p50,p99,p99_99,max,msgs/s
            s.push_str(&format!("{regime},{label},1000,1,{p50},{p50},{p50},{p50},0\n"));
        }
        s
    }

    #[test]
    fn within_noise_is_not_a_win() {
        let base = csv(&[("back_to_back", "cross", 100)]);
        let cur = csv(&[("back_to_back", "cross", 101)]); // +1%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed());
        assert!(c.to_markdown().contains("within noise"));
    }

    #[test]
    fn regression_beyond_band_is_flagged() {
        let base = csv(&[("back_to_back", "cross", 100)]);
        let cur = csv(&[("back_to_back", "cross", 130)]); // +30%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(c.any_regressed());
    }

    #[test]
    fn improvement_detected() {
        let base = csv(&[("back_to_back", "cross", 100)]);
        let cur = csv(&[("back_to_back", "cross", 70)]); // -30%
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed());
        assert!(c.to_markdown().contains("improved"));
    }

    #[test]
    fn same_label_different_regime_does_not_match() {
        // A back-to-back baseline and an open-loop current for the "same" label
        // are different keys → each shows as incomparable, never subtracted.
        let base = csv(&[("back_to_back", "cross", 100)]);
        let cur = csv(&[("open_loop", "cross", 500)]);
        let c = Comparison::from_strings(&base, &cur, 5.0);
        assert!(!c.any_regressed(), "cross-regime must not be read as a regression");
        let md = c.to_markdown();
        assert_eq!(md.matches("incomparable").count(), 2);
    }
}
