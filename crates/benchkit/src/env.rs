//! Run-environment detection and the methodology header.
//!
//! Captures the machine + process state at run time so every report stamps the
//! conditions it ran under — the transparency layer. Linux-only reads via /proc
//! and /sys; everything degrades to "unknown"/false off-Linux so the crate still
//! builds and runs elsewhere (dev convenience).

use std::fmt::Write as _;

use telemetry::primitives::TscCalibration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Turbo {
    On,
    Off,
    Unknown,
}

/// Snapshot of machine + process state for one benchmark run.
#[derive(Clone, Debug)]
pub struct RunEnv {
    pub cpu_model: String,
    pub governor: String,
    pub turbo: Turbo,
    pub isolated: String, // "none" when no cores are isolated
    pub affinity: String, // Cpus_allowed_list from /proc/self/status
    pub pinned: bool,     // affinity names exactly one core
    pub tsc_ghz: f64,
    pub unix_secs: u64,
    pub stamp: String,          // "YYYY-MM-DD_HH-MM-SS" UTC, filename-safe
    pub commit: Option<String>, // git short sha at run time; None off-git
    pub dirty: bool,            // tracked files modified since that commit
    pub rustc: String,          // compiler that built this binary (from build.rs)
    pub rustflags: String,      // flags cargo passed it; "" when none
    pub profile: String,        // "release"/"debug", from debug_assertions
    // Bench-core prep state, detected ONLY when pinned (when unpinned the
    // not-pinned warning already condemns the run; cpu0's state would be noise).
    pub bench_core: Option<u32>,       // the core `affinity` names
    pub clock_min_khz: Option<u64>,    // scaling_min_freq of that core
    pub clock_max_khz: Option<u64>,    // scaling_max_freq of that core
    pub smt_siblings_online: Vec<u32>, // ONLINE hyperthread siblings of that core
}

impl RunEnv {
    /// Detect the current environment. `cal` supplies the measured TSC rate.
    pub fn detect(cal: &TscCalibration) -> Self {
        let unix_secs = unix_secs();
        let affinity = read_affinity();
        let pinned = is_single_core(&affinity);
        let bench_core = if pinned { affinity.parse::<u32>().ok() } else { None };
        // Read per-cpu state from the BENCH core when pinned — cpu0's governor
        // or clock limits say nothing about the core the run executes on.
        let probe = bench_core.unwrap_or(0);
        let (clock_min_khz, clock_max_khz) = match bench_core {
            Some(c) => (read_khz(c, "scaling_min_freq"), read_khz(c, "scaling_max_freq")),
            None => (None, None),
        };
        RunEnv {
            cpu_model: cpu_model(),
            governor: read_trim(&format!(
                "/sys/devices/system/cpu/cpu{probe}/cpufreq/scaling_governor"
            ))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into()),
            turbo: read_turbo(),
            isolated: read_trim("/sys/devices/system/cpu/isolated")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "none".into()),
            affinity,
            pinned,
            tsc_ghz: cal.ticks_per_nanosecond,
            unix_secs,
            stamp: utc_stamp(unix_secs),
            commit: git_short_sha(),
            dirty: git_dirty(),
            // Baked in by build.rs at compile time (see its module doc): the
            // resolved compiler and the flags cargo actually passed.
            rustc: env!("BENCHKIT_RUSTC_VERSION").to_string(),
            rustflags: env!("BENCHKIT_RUSTFLAGS").to_string(),
            // debug_assertions tracks the profile family this code was compiled
            // under: off for release/bench, on for dev/test.
            profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_string(),
            bench_core,
            clock_min_khz,
            clock_max_khz,
            smt_siblings_online: bench_core.map(online_siblings).unwrap_or_default(),
        }
    }

    /// Identity for this run's artifacts: the *code* that produced them, not the
    /// time they ran. `<sha>` on a clean tree, `<sha>-dirty` otherwise, and the
    /// timestamp off-git (so filenames are never empty). Reruns at the same
    /// clean commit overwrite their artifacts — that is the point: one commit,
    /// one canonical set of numbers. The header still records the timestamp.
    pub fn run_id(&self) -> String {
        match (&self.commit, self.dirty) {
            (Some(sha), false) => sha.clone(),
            (Some(sha), true) => format!("{sha}-dirty"),
            (None, _) => self.stamp.clone(),
        }
    }

    /// Machine-readable warnings: conditions that make a latency run untrustworthy.
    /// Empty == clean run. Callers / CI can fail on a non-empty result.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if !self.pinned {
            w.push(format!(
                "process not pinned to one core (affinity `{}`): tails include OS migration jitter — re-run under `taskset -c <core>`",
                self.affinity
            ));
        }
        if self.governor != "performance" && self.governor != "unknown" {
            w.push(format!(
                "governor `{}` (not `performance`): clock may scale during the run",
                self.governor
            ));
        }
        let core = self.bench_core.unwrap_or(0);
        if let (Some(min), Some(max)) = (self.clock_min_khz, self.clock_max_khz)
            && min != max
        {
            w.push(format!(
                "clock not pinned on cpu{core} (scaling min={min} max={max} kHz): the rate can move mid-run — pin it: sudo cpupower -c {core} frequency-set -d <khz> -u <khz>"
            ));
        }
        if !self.smt_siblings_online.is_empty() {
            let sibs: Vec<String> =
                self.smt_siblings_online.iter().map(|c| format!("cpu{c}")).collect();
            w.push(format!(
                "SMT sibling {} of bench cpu{core} is online: an OS thread there shares the physical core's pipelines and caches and pollutes tails — offline it: echo 0 | sudo tee /sys/devices/system/cpu/cpu<N>/online",
                sibs.join(", ")
            ));
        }
        w
    }

    /// Markdown methodology block. `title` and `scope` are caller-supplied
    /// (crate-specific); everything else is detected. Emits the ⚠ warnings.
    pub fn header(&self, title: &str, scope: &str) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# {title} — {} UTC (unix {})\n", self.stamp, self.unix_secs);
        let _ = writeln!(s, "**Hardware:** {}", self.cpu_model);
        let code = match (&self.commit, self.dirty) {
            (Some(sha), false) => format!("`{sha}`"),
            (Some(sha), true) => format!("`{sha}` + uncommitted changes (dirty)"),
            (None, _) => "unknown (not a git checkout)".to_string(),
        };
        let _ = writeln!(s, "- code: {code}");
        let flags = if self.rustflags.is_empty() {
            "(none)".to_string()
        } else {
            format!("`{}`", self.rustflags)
        };
        let _ = writeln!(s, "- build: {} · profile `{}` · rustflags {flags}", self.rustc, self.profile);
        let turbo = match self.turbo {
            Turbo::On => "on",
            Turbo::Off => "off",
            Turbo::Unknown => "?",
        };
        let _ = writeln!(
            s,
            "- governor `{}` · turbo `{turbo}` · isolated cores `{}` · affinity `{}` {}",
            self.governor,
            self.isolated,
            self.affinity,
            if self.pinned { "(pinned ✓)" } else { "(NOT pinned ⚠)" }
        );
        if let (Some(core), Some(min), Some(max)) =
            (self.bench_core, self.clock_min_khz, self.clock_max_khz)
        {
            let _ = writeln!(
                s,
                "- clock: cpu{core} scaling min={min} max={max} kHz {}",
                if min == max { "(pinned ✓)" } else { "(NOT pinned ⚠)" }
            );
        }
        let _ = writeln!(s, "- TSC rate ~{:.2} GHz", self.tsc_ghz);
        let _ = writeln!(s, "- scope: {scope}");
        let _ = writeln!(
            s,
            "- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first."
        );
        for warning in self.warnings() {
            let _ = writeln!(s, "- ⚠ {warning}");
        }
        s
    }
}

// ---- detection helpers (Linux; degrade gracefully elsewhere) ---------------

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn cpu_model() -> String {
    read_trim("/proc/cpuinfo")
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn read_turbo() -> Turbo {
    match read_trim("/sys/devices/system/cpu/intel_pstate/no_turbo").as_deref() {
        Some("1") => Turbo::Off,
        Some("0") => Turbo::On,
        _ => Turbo::Unknown,
    }
}

/// A cpufreq value (kHz) for one core, e.g. `scaling_min_freq`.
fn read_khz(core: u32, file: &str) -> Option<u64> {
    read_trim(&format!("/sys/devices/system/cpu/cpu{core}/cpufreq/{file}"))
        .and_then(|s| s.parse().ok())
}

/// Online SMT siblings of `core` — the hyperthreads sharing its physical core.
/// Empty when topology is unreadable (off-Linux) or all siblings are offline.
fn online_siblings(core: u32) -> Vec<u32> {
    let Some(list) = read_trim(&format!(
        "/sys/devices/system/cpu/cpu{core}/topology/thread_siblings_list"
    )) else {
        return Vec::new();
    };
    parse_cpu_list(&list)
        .into_iter()
        .filter(|&sib| sib != core)
        .filter(|&sib| {
            // A missing `online` file means the cpu is not hot-pluggable
            // (cpu0): it cannot be offlined, so it IS online.
            read_trim(&format!("/sys/devices/system/cpu/cpu{sib}/online"))
                .map(|v| v == "1")
                .unwrap_or(true)
        })
        .collect()
}

/// Parse a sysfs cpu list ("2,6", "0-3", "0-1,4") into core ids.
fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(v) = part.parse::<u32>() {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// Short sha of HEAD, captured at run time by shelling out to `git`. `None` when
/// not in a git checkout (or git is absent) — callers fall back to the timestamp.
/// Run-time capture is correct for the normal `cargo bench` flow where build and
/// run happen back to back; a build-time stamp would survive `git checkout` but
/// brings its own staleness traps (see the LLD discussion).
fn git_short_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Whether tracked files differ from HEAD. `--untracked-files=no` is the
/// load-bearing flag: untracked files (bench-runs/, scratch docs) do not change
/// the compiled binary, so they must not poison the run id with `-dirty`. Only
/// a modified tracked file means "this binary may not match the sha".
fn git_dirty() -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn read_affinity() -> String {
    read_trim("/proc/self/status")
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Cpus_allowed_list:"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "?".into())
}

/// A `Cpus_allowed_list` names exactly one core iff it has no range (`-`) and no
/// comma (`,`) — e.g. "3" is pinned, "0-7" or "1,3" is not.
fn is_single_core(affinity: &str) -> bool {
    !affinity.is_empty()
        && affinity != "?"
        && !affinity.contains('-')
        && !affinity.contains(',')
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// (year, month, day) from days-since-epoch — Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "YYYY-MM-DD_HH-MM-SS" UTC, filename-safe.
pub(crate) fn utc_stamp(secs: u64) -> String {
    let (h, mi, s) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_core_detection() {
        assert!(is_single_core("3"));
        assert!(is_single_core("0"));
        assert!(!is_single_core("0-7"));
        assert!(!is_single_core("1,3"));
        assert!(!is_single_core("?"));
        assert!(!is_single_core(""));
    }

    #[test]
    fn utc_stamp_known_epochs() {
        // 0 = 1970-01-01 00:00:00
        assert_eq!(utc_stamp(0), "1970-01-01_00-00-00");
        // 1_000_000_000 = 2001-09-09 01:46:40 UTC
        assert_eq!(utc_stamp(1_000_000_000), "2001-09-09_01-46-40");
        // 1_780_000_000 = 2026-05-28 20:26:40 UTC
        assert_eq!(utc_stamp(1_780_000_000), "2026-05-28_20-26-40");
    }

    #[test]
    fn detect_never_panics_and_fills_fields() {
        let cal = telemetry::primitives::calibrate();
        let env = RunEnv::detect(&cal);
        assert!(!env.cpu_model.is_empty());
        assert!(!env.governor.is_empty());
        assert!(!env.isolated.is_empty());
        assert!(env.tsc_ghz > 0.0);
        // header renders without panicking and contains the title.
        let h = env.header("test_suite", "unit-test scope");
        assert!(h.contains("test_suite"));
        assert!(h.contains("Hardware:"));
        // the code line renders in both the in-git and off-git shapes.
        assert!(h.contains("- code:"));
        // build provenance is baked in at compile time, so it is never empty,
        // and under `cargo test` this very test compiles with debug_assertions.
        assert!(!env.rustc.is_empty());
        assert_eq!(env.profile, "debug");
        assert!(h.contains("- build:"));
    }

    #[test]
    fn cpu_list_parsing() {
        assert_eq!(parse_cpu_list("2,6"), vec![2, 6]);
        assert_eq!(parse_cpu_list("0-3"), vec![0, 1, 2, 3]);
        assert_eq!(parse_cpu_list("0-1,4"), vec![0, 1, 4]);
        assert_eq!(parse_cpu_list("3"), vec![3]);
        assert_eq!(parse_cpu_list(""), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("junk"), Vec::<u32>::new());
    }

    #[test]
    fn warnings_flag_unpinned_clock_and_online_siblings() {
        let cal = telemetry::primitives::calibrate();
        let mut env = RunEnv::detect(&cal);
        // Force a clean prep baseline, then break one condition at a time.
        env.affinity = "2".into();
        env.pinned = true;
        env.governor = "performance".into();
        env.bench_core = Some(2);
        env.clock_min_khz = Some(4_700_000);
        env.clock_max_khz = Some(4_700_000);
        env.smt_siblings_online = Vec::new();
        assert!(env.warnings().is_empty(), "clean prep produces no warnings");

        env.clock_min_khz = Some(1_200_000);
        let w = env.warnings();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("clock not pinned on cpu2"));
        assert!(w[0].contains("min=1200000 max=4700000"));
        env.clock_min_khz = Some(4_700_000);

        env.smt_siblings_online = vec![6];
        let w = env.warnings();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("SMT sibling cpu6"));
        assert!(w[0].contains("bench cpu2"));

        // The header stamps the clock policy and surfaces the warning lines.
        let h = env.header("t", "s");
        assert!(h.contains("- clock: cpu2 scaling min=4700000 max=4700000 kHz (pinned ✓)"));
        assert!(h.contains("⚠ SMT sibling cpu6"));
    }

    #[test]
    fn run_id_prefers_commit_and_marks_dirty() {
        // detect() gives a valid base; override the git fields by hand so the
        // test is deterministic in any checkout state (and off-git entirely).
        let cal = telemetry::primitives::calibrate();
        let mut env = RunEnv::detect(&cal);

        env.commit = Some("abc1234".into());
        env.dirty = false;
        assert_eq!(env.run_id(), "abc1234");

        env.dirty = true;
        assert_eq!(env.run_id(), "abc1234-dirty");

        env.commit = None;
        assert_eq!(env.run_id(), env.stamp, "off-git falls back to the timestamp");
    }
}
