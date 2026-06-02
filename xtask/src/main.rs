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
//! Intents (comma-separated): latency (default), cache, flamegraph.
//! (alloc/open-loop are in-process regimes a bench opts into in code; this
//! runner orchestrates the out-of-process regimes that need extra tooling.)

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, exit};

/// Crates that currently ship a benchkit `*_bench` target.
const BENCH_CRATES: &[&str] = &["arena", "orderbook", "matcher"];
/// Crates that ship an iai (callgrind) bench target + the `iai` feature.
const IAI_CRATES: &[&str] = &["matcher"];
/// Crates that ship a dhat `alloc_proof` example.
const ALLOC_CRATES: &[&str] = &["matcher"];

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
        BENCH_CRATES.iter().map(|s| s.to_string()).collect()
    } else if let Some(c) = &opts.krate {
        vec![c.clone()]
    } else {
        // Default: every benched crate, latency only.
        BENCH_CRATES.iter().map(|s| s.to_string()).collect()
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
    let bench = format!("{krate}_bench");
    eprintln!("→ [{krate}] latency: cargo bench -p {krate} --bench {bench}");
    cargo_bench(krate, &bench, &[])
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

/// CPU-internals (`perf stat`: IPC, cache-miss%, branch-mispredict%) via
/// scripts/perf_bench.sh. The script handles the build, taskset pinning, and
/// sudo-if-paranoid.
fn run_perfstat(krate: &str) -> Result<(), String> {
    eprintln!("→ [{krate}] perfstat: scripts/perf_bench.sh {krate}");
    let status = Command::new("scripts/perf_bench.sh")
        .arg(krate)
        .status()
        .map_err(|e| format!("failed to spawn scripts/perf_bench.sh: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] perf_bench.sh failed (exit {:?})", status.code()));
    }
    Ok(())
}

/// perf-record a latency run and emit a flamegraph SVG. Requires perf and the
/// flamegraph scripts (stackcollapse-perf.pl / flamegraph.pl) on PATH.
fn run_flamegraph(krate: &str) -> Result<(), String> {
    let bench = format!("{krate}_bench");
    eprintln!("→ [{krate}] flamegraph: perf record …");

    // Build the bench binary first so perf profiles the bench, not the build.
    let status = Command::new("cargo")
        .args(["bench", "-p", krate, "--bench", &bench, "--no-run"])
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
    // Delegate the actual comparison to a tiny crate-local reimplementation that
    // matches benchkit's CSV schema — keeps xtask dep-free of benchkit itself.
    let report = compare_csvs(base, cur, noise)?;
    println!("{report}");
    if report.contains("REGRESSED") {
        return Err("one or more scenarios regressed beyond the noise band".into());
    }
    Ok(())
}

/// Minimal CSV comparison mirroring `benchkit::Comparison`. Schema:
/// `regime,label,count,min_ns,p50_ns,p99_ns,p99_99_ns,max_ns,msgs_per_sec`.
fn compare_csvs(base_path: &str, cur_path: &str, noise_pct: f64) -> Result<String, String> {
    let base = parse_p50s(base_path)?;
    let cur = parse_p50s(cur_path)?;

    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "# Benchmark comparison (noise band ±{noise_pct:.1}%)\n");
    let _ = writeln!(out, "| scenario | regime | base p50 | cur p50 | Δ% | verdict |");
    let _ = writeln!(out, "|---|---|--:|--:|--:|---|");

    let keys: BTreeSet<&(String, String)> = base.keys().chain(cur.keys()).collect();
    for key in keys {
        let (label, regime) = key;
        match (base.get(key), cur.get(key)) {
            (Some(&b), Some(&c)) => {
                let delta = (c as f64 - b as f64) / b as f64 * 100.0;
                let verdict = if delta.abs() <= noise_pct {
                    "· within noise"
                } else if delta < 0.0 {
                    "✓ improved"
                } else {
                    "✗ REGRESSED"
                };
                let _ = writeln!(out, "| {label} | {regime} | {b} | {c} | {delta:+.1}% | {verdict} |");
            }
            (b, c) => {
                let _ = writeln!(
                    out,
                    "| {label} | {regime} | {} | {} | — | ? incomparable |",
                    b.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                    c.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                );
            }
        }
    }
    Ok(out)
}

fn parse_p50s(path: &str) -> Result<std::collections::HashMap<(String, String), u64>, String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let mut map = std::collections::HashMap::new();
    for line in body.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 9 {
            continue;
        }
        if let Ok(p50) = f[4].parse::<u64>() {
            map.insert((f[1].to_string(), f[0].to_string()), p50);
        }
    }
    Ok(map)
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
                if !BENCH_CRATES.contains(&c.as_str()) {
                    return Err(format!("unknown crate `{c}`; known: {}", BENCH_CRATES.join(", ")));
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
                        "latency" | "cache" | "alloc" | "perfstat" => {
                            intents.insert(p.to_string());
                        }
                        "flamegraph" => flamegraph = true,
                        "" => {}
                        other => {
                            return Err(format!(
                                "unknown intent `{other}` (latency|cache|alloc|perfstat|flamegraph)"
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
         flamegraph  build the bench for perf profiling (needs perf + FlameGraph scripts)\n\n\
         CRATES: {}\n\n\
         EXAMPLES:\n  \
         cargo bench-all\n  \
         cargo bench-all --crate matcher --intent latency,cache\n  \
         cargo bench-all --compare bench-runs/matcher_OLD.csv bench-runs/matcher_NEW.csv",
        BENCH_CRATES.join(", ")
    );
}
