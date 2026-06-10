//! Benchkit orchestration runner.
//!
//! Invoked as `cargo bench-all -- <args>` (alias in .cargo/config.toml). Maps a
//! measurement *intent* to the correct `cargo bench` invocations so a developer
//! never hand-assembles `--features` flags — and, crucially, VERIFIES a regime
//! can actually run before claiming it did. A silently-skipped measurement (you
//! think you ran a cache test but forgot `--features iai`) is as dangerous as a
//! mislabeled one, so this runner errors loudly instead.
//!
//! Usage:
//!   cargo bench-all                         # latency, all benched crates
//!   cargo bench-all --crate matcher         # latency, one crate
//!   cargo bench-all --crate matcher --intent latency,cache
//!   cargo bench-all --all-crates --intent latency
//!   cargo bench-all --crate matcher --flamegraph
//!   cargo bench-all --compare BASE.csv CUR.csv [--noise 5]
//!
//! Intents (comma-separated): latency (default), cache, alloc, perfstat,
//! plots, flamegraph. (open-loop is an in-process regime a bench opts into in
//! code; this runner orchestrates the passes that need extra tooling.)

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, exit};

/// Crates that ship a benchkit latency bench, mapped to that bench target's
/// name — the single source of truth for both "which crates" and "what the
/// bench is called". Most follow the `<crate>_bench` convention; ipc's suite
/// bench is `self_latency` (its `cross_core` bench pins two cores and runs on
/// its own, e.g. via scripts/capture.sh — not part of the orchestrated sweep).
const BENCH_CRATES: &[(&str, &str)] = &[
    ("arena", "arena_bench"),
    ("orderbook", "orderbook_bench"),
    ("matcher", "matcher_bench"),
    ("ipc", "self_latency"),
];

/// Latency bench target for a crate in [`BENCH_CRATES`].
fn latency_bench(krate: &str) -> Option<&'static str> {
    BENCH_CRATES.iter().find(|(k, _)| *k == krate).map(|(_, b)| *b)
}

/// The known crate names, for error messages and help text.
fn bench_crate_list() -> String {
    BENCH_CRATES.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
}
/// Crates that ship an iai (callgrind) bench target + the `iai` feature.
const IAI_CRATES: &[&str] = &["matcher"];
/// Crates that ship a dhat `alloc_proof` example.
const ALLOC_CRATES: &[&str] = &["matcher", "ipc"];
/// Core the perfstat regime pins to — paired with the latency core (2) on this
/// box; both are isolcpus'd so the counters see only the bench.
const PERF_CORE: &str = "3";

fn main() {
    // The `cargo bench-all -- <args>` alias already strips one `--`, but if the
    // user adds their own (`cargo bench-all -- --crate matcher`) a literal `--`
    // can lead the args. Skip a single leading separator so both forms work.
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(|s| s == "--").unwrap_or(false) {
        raw.remove(0);
    }
    let args = raw;
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("\x1b[31merror:\x1b[0m {e}");
            exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    // --compare short-circuits: it's a different mode (two CSVs in, verdict out).
    if let Some(pos) = args.iter().position(|a| a == "--compare") {
        return run_compare(&args[pos + 1..]);
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }

    let opts = parse(args)?;
    let crates = if opts.all_crates {
        BENCH_CRATES.iter().map(|(k, _)| k.to_string()).collect()
    } else if let Some(c) = &opts.krate {
        vec![c.clone()]
    } else {
        // Default: every benched crate, latency only.
        BENCH_CRATES.iter().map(|(k, _)| k.to_string()).collect()
    };

    // Verify-before-claim: if a regime was requested, its tooling must be present,
    // or we refuse the whole run rather than silently skipping it later.
    if opts.intents.contains("cache") && !tool_present("valgrind") {
        return Err(
            "cache regime requested but `valgrind` is not on PATH. \
             Install valgrind (and `cargo install iai-callgrind-runner`) or drop `cache` from --intent."
                .into(),
        );
    }
    if opts.intents.contains("perfstat") && !tool_present("perf") {
        return Err("perfstat regime requested but `perf` is not on PATH.".into());
    }
    if opts.intents.contains("plots") && !tool_present("gnuplot") {
        return Err("plots regime requested but `gnuplot` is not on PATH.".into());
    }
    if opts.flamegraph && !tool_present("perf") {
        return Err("--flamegraph requested but `perf` is not on PATH.".into());
    }

    eprintln!("== benchkit runner ==");
    eprintln!("crates : {}", crates.join(", "));
    eprintln!("intents: {}", opts.intents.iter().cloned().collect::<Vec<_>>().join(", "));
    eprintln!();

    for krate in &crates {
        if opts.intents.contains("latency") {
            run_latency(krate)?;
        }
        if opts.intents.contains("cache") {
            run_cache(krate)?;
        }
        if opts.intents.contains("alloc") {
            run_alloc(krate)?;
        }
        if opts.intents.contains("perfstat") {
            run_perfstat(krate)?;
        }
        if opts.intents.contains("plots") {
            run_plots(krate)?;
        }
        if opts.flamegraph {
            run_flamegraph(krate)?;
        }
    }

    eprintln!("\n\x1b[32m✓ done.\x1b[0m reports under bench-runs/ at the workspace root.");
    Ok(())
}

// ---- regimes ---------------------------------------------------------------

/// rdtscp latency — the default, zero-extra-deps pass.
fn run_latency(krate: &str) -> Result<(), String> {
    let bench = latency_bench(krate)
        .ok_or_else(|| format!("[{krate}] no latency bench registered in BENCH_CRATES"))?;
    eprintln!("→ [{krate}] latency: cargo bench -p {krate} --bench {bench}");
    cargo_bench(krate, bench, &[])
}

/// callgrind instruction/cache counts. Cannot share a process with rdtscp passes
/// (the binary is launched under valgrind by the iai runner), so it's its own
/// `cargo bench --features iai` invocation — spawned here so there's no "did I
/// remember to run the iai one?" gap.
fn run_cache(krate: &str) -> Result<(), String> {
    if !IAI_CRATES.contains(&krate) {
        eprintln!("→ [{krate}] cache: skipped (no iai bench target for this crate)");
        return Ok(());
    }
    let bench = format!("{krate}_iai");
    eprintln!("→ [{krate}] cache: cargo bench -p {krate} --features iai --bench {bench}");
    cargo_bench(krate, &bench, &["--features", "iai"])
}

/// Heap-allocation proof via the crate's dhat `alloc_proof` example. Separate
/// binary (dhat's global-allocator shim perturbs timing), so never a latency
/// pass — spawned here so `alloc` is a real one-command regime.
fn run_alloc(krate: &str) -> Result<(), String> {
    if !ALLOC_CRATES.contains(&krate) {
        eprintln!("→ [{krate}] alloc: skipped (no alloc_proof example for this crate)");
        return Ok(());
    }
    eprintln!("→ [{krate}] alloc: cargo run --release --example alloc_proof -p {krate}");
    let status = Command::new("cargo")
        .args(["run", "--release", "--example", "alloc_proof", "-p", krate])
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] alloc_proof failed (exit {:?})", status.code()));
    }
    Ok(())
}

/// CPU-internals (`perf stat`: IPC, cache-miss%, branch-mispredict%) of a
/// crate's latency bench, pinned to one core (taskset) so counters aren't
/// smeared across migrations — the same hygiene as a latency run.
fn run_perfstat(krate: &str) -> Result<(), String> {
    let bench = latency_bench(krate)
        .ok_or_else(|| format!("[{krate}] no latency bench registered in BENCH_CRATES"))?;

    // Refuse rather than escalate when the kernel blocks unprivileged counters:
    // a sudo'd bench writes root-owned, commit-keyed artifacts that every later
    // unprivileged run dies trying to overwrite.
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if paranoid > 2 {
        return Err(format!(
            "kernel.perf_event_paranoid={paranoid} (>2) blocks unprivileged perf counters.\n\
             fix now:  sudo sysctl kernel.perf_event_paranoid=1\n\
             persist:  echo 'kernel.perf_event_paranoid = 1' | sudo tee /etc/sysctl.d/99-perf.conf"
        ));
    }

    let bin = build_bench_binary(krate, bench)?;

    // The bench writes its benchkit report as a side effect, and artifacts are
    // commit-keyed — left alone, this perf-instrumented run would overwrite the
    // canonical reports from the clean latency pass. Route them to a scratch
    // dir; the counters are this run's product, not the report.
    let scratch = std::env::temp_dir().join(format!("benchkit_perfstat_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create scratch dir: {e}"))?;

    eprintln!("→ [{krate}] perfstat: perf stat … -- taskset -c {PERF_CORE} {bin}");
    // -d -d = detailed: adds L1/LLC loads+misses on top of the explicit set.
    let status = Command::new("perf")
        .args([
            "stat",
            "-e", "task-clock,cycles,instructions,branches,branch-misses",
            "-e", "cache-references,cache-misses",
            "-d", "-d",
            "--",
            "taskset", "-c", PERF_CORE, &bin,
        ])
        .env("BENCH_OUT_DIR", &scratch)
        .status();
    let _ = std::fs::remove_dir_all(&scratch);
    let status = status.map_err(|e| format!("failed to spawn perf: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] perf stat failed (exit {:?})", status.code()));
    }
    eprintln!("  IPC = instructions/cycles; cache-miss% = cache-misses/cache-references.");
    eprintln!("  note: counters aggregate ALL scenarios (a portfolio-wide average); per-scenario counts come from the iai cache regime.");
    Ok(())
}

/// Build a bench target (`cargo bench --no-run`) and return its executable
/// path, parsed from cargo's JSON messages — exact, instead of globbing
/// `target/release/deps` and hoping the newest hash is the right one. Build
/// progress stays visible on stderr; only the JSON stream is captured.
fn build_bench_binary(krate: &str, bench: &str) -> Result<String, String> {
    let out = Command::new("cargo")
        .args(["bench", "-p", krate, "--bench", bench, "--no-run", "--message-format=json"])
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!("[{krate}] bench build for `{bench}` failed"));
    }
    // One JSON object per line; only runnable artifacts carry a quoted
    // "executable" (libs and build scripts report null). The bench target
    // builds last, so the last match wins.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut exe = None;
    for line in stdout.lines() {
        if let Some(i) = line.find("\"executable\":\"") {
            let rest = &line[i + "\"executable\":\"".len()..];
            if let Some(j) = rest.find('"') {
                exe = Some(rest[..j].to_string());
            }
        }
    }
    exe.ok_or_else(|| format!("[{krate}] no executable in cargo's JSON output for `{bench}`"))
}

/// Render the latency-curve SVG by running gnuplot on the most recent
/// `<crate>_*.gnuplot` script in bench-runs/ (emitted by a latency run). The
/// script's paths are relative to bench-runs/, so we run gnuplot with that cwd.
fn run_plots(krate: &str) -> Result<(), String> {
    let bench_runs = workspace_path("bench-runs");
    let script = latest_file(&bench_runs, krate, ".gnuplot").ok_or_else(|| {
        format!(
            "no {krate}_*.gnuplot found in bench-runs/ — run a latency pass first \
             (`cargo bench-all --crate {krate}` writes the gnuplot script)."
        )
    })?;
    let script_name = script.file_name().unwrap().to_string_lossy().into_owned();
    eprintln!("→ [{krate}] plots: gnuplot {script_name}  (cwd bench-runs/)");
    let status = Command::new("gnuplot")
        .arg(&script_name)
        .current_dir(&bench_runs)
        .status()
        .map_err(|e| format!("failed to spawn gnuplot: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] gnuplot failed (exit {:?})", status.code()));
    }
    let svg = script_name.replace(".gnuplot", ".svg");
    eprintln!("  wrote bench-runs/{svg}");
    Ok(())
}

/// perf-record a latency run and emit a flamegraph SVG. Requires perf and the
/// flamegraph scripts (stackcollapse-perf.pl / flamegraph.pl) on PATH.
fn run_flamegraph(krate: &str) -> Result<(), String> {
    let bench = latency_bench(krate)
        .ok_or_else(|| format!("[{krate}] no latency bench registered in BENCH_CRATES"))?;
    eprintln!("→ [{krate}] flamegraph: perf record …");

    // Build the bench binary first so perf profiles the bench, not the build.
    let status = Command::new("cargo")
        .args(["bench", "-p", krate, "--bench", bench, "--no-run"])
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] bench build for flamegraph failed"));
    }

    if !tool_present("flamegraph.pl") || !tool_present("stackcollapse-perf.pl") {
        return Err(
            "flamegraph scripts not found on PATH (need stackcollapse-perf.pl and flamegraph.pl). \
             Install Brendan Gregg's FlameGraph and add it to PATH."
                .into(),
        );
    }
    eprintln!(
        "  note: run perf manually on the built binary under target/release/deps/{bench}-* \
         then `perf script | stackcollapse-perf.pl | flamegraph.pl > bench-runs/{krate}_flame.svg`."
    );
    // Auto-driving perf record across distros/permission models is brittle
    // (perf_event_paranoid, kptr_restrict); we verified perf exists and built
    // the binary, but leave the privileged record step explicit.
    Ok(())
}

fn cargo_bench(krate: &str, bench: &str, extra: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("cargo");
    cmd.args(["bench", "-p", krate, "--bench", bench]);
    cmd.args(extra);
    let status = cmd.status().map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] `{bench}` failed (exit {:?})", status.code()));
    }
    Ok(())
}

// ---- compare mode ----------------------------------------------------------

fn run_compare(rest: &[String]) -> Result<(), String> {
    let mut positional = Vec::new();
    let mut noise = 5.0f64;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--noise" => {
                let v = rest.get(i + 1).ok_or("--noise needs a value")?;
                noise = v.parse().map_err(|_| format!("bad --noise value: {v}"))?;
                i += 2;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    if positional.len() != 2 {
        return Err("--compare needs exactly two CSV paths: BASELINE CURRENT".into());
    }
    let (base, cur) = (&positional[0], &positional[1]);
    for p in [base, cur] {
        if !Path::new(p).exists() {
            return Err(format!("csv not found: {p}"));
        }
    }
    // benchkit owns the verdict logic (worst metric across p50/p99/p99.99, noise
    // band, incomparable rows); xtask only parses flags and sets the exit code.
    let cmp = benchkit::Comparison::load(base, cur, noise)
        .map_err(|e| format!("compare failed: {e}"))?;
    print!("{}", cmp.to_markdown());
    if cmp.any_regressed() {
        return Err("one or more scenarios regressed beyond the noise band".into());
    }
    Ok(())
}

// ---- arg parsing -----------------------------------------------------------

struct Opts {
    krate: Option<String>,
    all_crates: bool,
    intents: BTreeSet<String>,
    flamegraph: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut krate = None;
    let mut all_crates = false;
    let mut intents = BTreeSet::new();
    let mut flamegraph = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--crate" => {
                let c = args.get(i + 1).ok_or("--crate needs a value")?;
                if latency_bench(c).is_none() {
                    return Err(format!("unknown crate `{c}`; known: {}", bench_crate_list()));
                }
                krate = Some(c.clone());
                i += 2;
            }
            "--all-crates" => {
                all_crates = true;
                i += 1;
            }
            "--intent" => {
                let v = args.get(i + 1).ok_or("--intent needs a value")?;
                for part in v.split(',') {
                    let p = part.trim();
                    match p {
                        "latency" | "cache" | "alloc" | "perfstat" | "plots" => {
                            intents.insert(p.to_string());
                        }
                        "flamegraph" => flamegraph = true,
                        "" => {}
                        other => {
                            return Err(format!(
                                "unknown intent `{other}` (latency|cache|alloc|perfstat|plots|flamegraph)"
                            ));
                        }
                    }
                }
                i += 2;
            }
            "--flamegraph" => {
                flamegraph = true;
                i += 1;
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    // latency is the implicit default if no measurement intent was named.
    if intents.is_empty() && !flamegraph {
        intents.insert("latency".to_string());
    }
    Ok(Opts { krate, all_crates, intents, flamegraph })
}

/// Resolve `<workspace-root>/<rel>` by walking up from cwd to the dir whose
/// Cargo.toml declares `[workspace]`. Falls back to `rel` relative to cwd.
fn workspace_path(rel: &str) -> std::path::PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        if std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|c| c.contains("[workspace]")) {
            return dir.join(rel);
        }
        if !dir.pop() {
            return std::path::PathBuf::from(rel);
        }
    }
}

/// Most recently modified file in `dir` whose name starts with `prefix_` and
/// ends with `suffix`.
fn latest_file(dir: &Path, prefix: &str, suffix: &str) -> Option<std::path::PathBuf> {
    let pre = format!("{prefix}_");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(&pre) && name.ends_with(suffix)) {
            continue;
        }
        if let Ok(mtime) = entry.metadata().and_then(|m| m.modified())
            && best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true)
        {
            best = Some((mtime, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

fn tool_present(tool: &str) -> bool {
    // `which`-style check via PATH; works for binaries and PATH-listed scripts.
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn print_help() {
    eprintln!(
        "benchkit runner — orchestrates `cargo bench` across crates and regimes.\n\n\
         USAGE:\n  \
         cargo bench-all [--crate <name> | --all-crates] [--intent <list>] [--flamegraph]\n  \
         cargo bench-all --compare <baseline.csv> <current.csv> [--noise <pct>]\n\n\
         INTENTS (comma-separated, default `latency`):\n  \
         latency     rdtscp wall-clock latency (zero extra deps)\n  \
         cache       callgrind instruction/cache counts (needs valgrind; iai crates only)\n  \
         alloc       dhat heap-allocation proof / zero-alloc gate (alloc_proof crates only)\n  \
         perfstat    perf stat: IPC, cache-miss%, branch-mispredict% (needs perf)\n  \
         plots       render the latency-curve SVG from the latest run (needs gnuplot)\n  \
         flamegraph  build the bench for perf profiling (needs perf + FlameGraph scripts)\n\n\
         CRATES: {}\n\n\
         EXAMPLES:\n  \
         cargo bench-all\n  \
         cargo bench-all --crate matcher --intent latency,cache\n  \
         cargo bench-all --compare bench-runs/matcher_OLD.csv bench-runs/matcher_NEW.csv",
        bench_crate_list()
    );
}
