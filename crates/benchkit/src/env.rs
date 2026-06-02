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
    pub stamp: String, // "YYYY-MM-DD_HH-MM-SS" UTC, filename-safe
}

impl RunEnv {
    /// Detect the current environment. `cal` supplies the measured TSC rate.
    pub fn detect(cal: &TscCalibration) -> Self {
        let unix_secs = unix_secs();
        let affinity = read_affinity();
        let pinned = is_single_core(&affinity);
        RunEnv {
            cpu_model: cpu_model(),
            governor: read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
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
        w
    }

    /// Markdown methodology block. `title` and `scope` are caller-supplied
    /// (crate-specific); everything else is detected. Emits the ⚠ warnings.
    pub fn header(&self, title: &str, scope: &str) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# {title} — {} UTC (unix {})\n", self.stamp, self.unix_secs);
        let _ = writeln!(s, "**Hardware:** {}", self.cpu_model);
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
    }
}
