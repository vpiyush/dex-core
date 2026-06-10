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
//!   cargo bench-all capture [--out DIR]     # one-shot canonical capture
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
/// Cores the pinned passes use — latency on 2, perf counters on 3; both are
/// isolcpus'd on this box. Overridable via env (the knobs the old capture
/// script honored): `LAT_CORE=4 PERF_CORE=5 cargo bench-all capture`.
fn lat_core() -> String {
    std::env::var("LAT_CORE").unwrap_or_else(|_| "2".into())
}
fn perf_core() -> String {
    std::env::var("PERF_CORE").unwrap_or_else(|_| "3".into())
}

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
    // Subcommand-style modes short-circuit the intent machinery.
    if args.first().map(String::as_str) == Some("capture") {
        return run_capture(&args[1..]);
    }
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
            run_perfstat(krate, None)?;
        }
        if opts.intents.contains("plots") {
            run_plots(krate)?;
        }
        if opts.flamegraph {
            run_flamegraph(krate)?;
        }
    }

    eprintln!("\n\x1b[32m✓ done.\x1b[0m reports under {}", benchkit::out_dir().display());
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

/// Refuse rather than escalate when the kernel blocks unprivileged counters:
/// a sudo'd bench writes root-owned artifacts at run-id paths that every later
/// unprivileged rerun dies trying to overwrite.
fn check_paranoid() -> Result<(), String> {
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
    Ok(())
}

/// CPU-internals (`perf stat`: IPC, cache-miss%, branch-mispredict%) of a
/// crate's latency bench, pinned to one core (taskset) so counters aren't
/// smeared across migrations — the same hygiene as a latency run. With `tee`,
/// the merged output is also written there (capture mode).
fn run_perfstat(krate: &str, tee: Option<&Path>) -> Result<(), String> {
    let bench = latency_bench(krate)
        .ok_or_else(|| format!("[{krate}] no latency bench registered in BENCH_CRATES"))?;
    check_paranoid()?;
    let bin = build_bench_binary(krate, bench)?;

    // The bench writes its benchkit report as a side effect, into this run id's
    // directory — left alone, this perf-instrumented run would overwrite the
    // canonical reports from the clean latency pass. Route them to a scratch
    // dir; the counters are this run's product, not the report.
    let scratch = std::env::temp_dir().join(format!("benchkit_perfstat_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create scratch dir: {e}"))?;

    let core = perf_core();
    eprintln!("→ [{krate}] perfstat: perf stat … -- taskset -c {core} {bin}");
    // -d -d = detailed: adds L1/LLC loads+misses on top of the explicit set.
    let mut cmd = Command::new("perf");
    cmd.args([
        "stat",
        "-e", "task-clock,cycles,instructions,branches,branch-misses",
        "-e", "cache-references,cache-misses",
        "-d", "-d",
        "--",
        "taskset", "-c", &core, &bin,
    ])
    .env("BENCH_OUT_DIR", &scratch);
    let result = match tee {
        Some(txt) => tee_run(cmd, txt),
        None => status_run(cmd),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    result.map_err(|e| format!("[{krate}] perf stat failed: {e}"))?;
    eprintln!("  IPC = instructions/cycles; cache-miss% = cache-misses/cache-references.");
    eprintln!("  note: counters aggregate ALL scenarios (a portfolio-wide average); per-scenario counts come from the iai cache regime.");
    Ok(())
}

/// Build a bench target (`cargo bench --no-run`) and return its executable path.
fn build_bench_binary(krate: &str, bench: &str) -> Result<String, String> {
    cargo_executable(&["bench", "-p", krate, "--bench", bench, "--no-run"])
}

/// Run `cargo <args> --message-format=json` and return the built executable's
/// path, parsed from the JSON messages — exact, instead of globbing
/// `target/release/deps` and hoping the newest hash is the right one. Build
/// progress stays visible on stderr; only the JSON stream is captured.
fn cargo_executable(args: &[&str]) -> Result<String, String> {
    let out = Command::new("cargo")
        .args(args)
        .arg("--message-format=json")
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!("`cargo {}` failed", args.join(" ")));
    }
    // One JSON object per line; only runnable artifacts carry a quoted
    // "executable" (libs and build scripts report null). The requested target
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
    exe.ok_or_else(|| format!("no executable in cargo's JSON output for `cargo {}`", args.join(" ")))
}

/// Run a command to completion with inherited stdio.
fn status_run(mut cmd: Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("spawn: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("exit {:?}", status.code())) }
}

/// Run a command, then both print its merged stdout+stderr and write it to
/// `txt` — the in-process `cmd 2>&1 | tee txt`. One pipe carries both streams,
/// so interleaving is preserved. The output is written even on failure
/// (diagnostics belong in the capture too). Passes are short (the builds run
/// separately, live), so end-of-run printing beats live streaming here.
fn tee_run(mut cmd: Command, txt: &Path) -> Result<(), String> {
    use std::io::Read as _;
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
    let writer2 = writer.try_clone().map_err(|e| format!("pipe: {e}"))?;
    cmd.stdout(writer).stderr(writer2);
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    // The Command retains its copies of the pipe writers; drop them or the
    // reader never sees EOF.
    drop(cmd);
    let mut out = String::new();
    reader.read_to_string(&mut out).map_err(|e| format!("read: {e}"))?;
    let status = child.wait().map_err(|e| format!("wait: {e}"))?;
    print!("{out}");
    std::fs::write(txt, &out).map_err(|e| format!("write {}: {e}", txt.display()))?;
    if status.success() { Ok(()) } else { Err(format!("exit {:?}", status.code())) }
}

/// Render the latency-curve SVG by running gnuplot on the crate's `.gnuplot`
/// script in the CURRENT run's directory (`target/bench-runs/<run_id>/`, via
/// benchkit::out_dir). The script's paths are relative to that dir, so gnuplot
/// runs with it as cwd.
fn run_plots(krate: &str) -> Result<(), String> {
    let dir = benchkit::out_dir();
    // The script is `<report name>.gnuplot`; report names are the crate name or
    // prefixed by it (ipc's latency report is `ipc_self_latency`).
    let script_name = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| {
            n.ends_with(".gnuplot") && (n == &format!("{krate}.gnuplot") || n.starts_with(&format!("{krate}_")))
        })
        .ok_or_else(|| {
            format!(
                "no gnuplot script for `{krate}` in {} — run a latency pass at this commit first \
                 (`cargo bench-all --crate {krate}`).",
                dir.display()
            )
        })?;
    eprintln!("→ [{krate}] plots: gnuplot {script_name}  (cwd {})", dir.display());
    let status = Command::new("gnuplot")
        .arg(&script_name)
        .current_dir(&dir)
        .status()
        .map_err(|e| format!("failed to spawn gnuplot: {e}"))?;
    if !status.success() {
        return Err(format!("[{krate}] gnuplot failed (exit {:?})", status.code()));
    }
    eprintln!("  wrote {}/{}", dir.display(), script_name.replace(".gnuplot", ".svg"));
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

// ---- capture mode ----------------------------------------------------------

/// One-shot canonical performance capture: everything the README and per-crate
/// docs quote — latency reports + curves, the cross-core hop, callgrind counts,
/// alloc proofs, perf counters, provenance — written DIRECTLY into one
/// directory (default `docs/perf/<run_id>`) by routing every child's
/// `BENCH_OUT_DIR` there. No post-hoc artifact gathering, so two runs can
/// never mix.
///
/// PREP THE BOX FIRST (by hand, needs sudo — the capture itself only warns):
///   sudo cpupower frequency-set -g performance
///   # PEAK mode (published numbers): turbo ON, request the top bin; the
///   # effective clock is MEASURED and stamped in ENVIRONMENT.txt.
///   # CAUTION: never `no_turbo=1` — it caps at the CONFIGURED base,
///   # 1.2 GHz on this cTDP-down X1 Yoga.
///   echo 0 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
///   sudo cpupower -c 2,3 frequency-set -d 4700MHz -u 4700MHz
///   #   (PINNED mode for A/B regression runs instead: -d 2800MHz -u 2800MHz)
///   # Offline the SMT siblings of the bench cores (2,3 pair with 6,7 here):
///   echo 0 | sudo tee /sys/devices/system/cpu/cpu6/online
///   echo 0 | sudo tee /sys/devices/system/cpu/cpu7/online
///   sudo sysctl kernel.perf_event_paranoid=1
///   # Best, needs one reboot — isolate the bench cores in grub:
///   #   GRUB_CMDLINE_LINUX_DEFAULT="... isolcpus=2,3 nohz_full=2,3 rcu_nocbs=2,3"
///   # Then close browsers, editors, Docker, sync daemons. Undo after.
fn run_capture(rest: &[String]) -> Result<(), String> {
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--out" => {
                out = Some(rest.get(i + 1).ok_or("--out needs a value")?.clone());
                i += 2;
            }
            other => return Err(format!("unknown capture argument `{other}` (only --out <dir>)")),
        }
    }
    // Published captures are keyed like every run: by what produced them.
    // `docs/perf/<run_id>/` — a new commit gets a new directory, so a capture
    // can never damage a previously published one.
    let dir = match out {
        Some(d) => std::path::PathBuf::from(d),
        None => workspace_path(&format!("docs/perf/{}", benchkit::run_id())),
    };

    // Verify-before-claim, capture edition: a canonical capture with silently
    // missing passes is worse than no capture. All tools, up front.
    let missing: Vec<&str> = ["perf", "valgrind", "gnuplot", "taskset"]
        .into_iter()
        .filter(|t| !tool_present(t))
        .collect();
    if !missing.is_empty() {
        return Err(format!("capture needs {} on PATH", missing.join(", ")));
    }
    check_paranoid()?;

    // The capture dir is the canonical record of ONE run at one run id. A
    // rerun at the same id must not inherit artifacts from renamed scenarios
    // or passes of an earlier attempt. Start from empty.
    if dir.exists() && std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        eprintln!("-> {} is not empty; clearing the previous capture so runs don't mix", dir.display());
        std::fs::remove_dir_all(&dir).map_err(|e| format!("clear {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let lat = lat_core();
    let perf = perf_core();
    eprintln!("== dex-core capture ==");
    eprintln!("output : {}", dir.display());
    eprintln!("cores  : latency->cpu{lat}  perf->cpu{perf}  cross_core->2,3 (self-pinned)");
    // Surface prep problems up front, not buried in report headers (the same
    // checks each pinned bench re-applies to its own core via RunEnv).
    for core in [&lat, &perf] {
        if let Ok(n) = core.parse::<u32>() {
            for w in benchkit::CorePrep::detect(n).warnings() {
                eprintln!("⚠ {w}");
            }
        }
    }
    let eff = effective_clock_ghz(&lat);
    eprintln!(
        "clock  : effective under load on cpu{lat}: {}",
        eff.map(|g| format!("{g:.2} GHz")).unwrap_or_else(|| "unmeasured".into())
    );
    eprintln!();

    write_environment(&dir, &lat, &perf, eff)?;

    let mut fails: Vec<String> = Vec::new();
    let mut step = |name: &str, r: Result<(), String>| {
        if let Err(e) = r {
            eprintln!("  WARN: {name} failed (continuing): {e}");
            fails.push(name.to_string());
        }
    };

    // 1. Latency curves, pinned to the isolated latency core.
    for (krate, bench) in BENCH_CRATES.iter().copied() {
        step(&format!("latency {krate}"), capture_latency(krate, bench, &lat, &dir));
    }
    // 2. Cross-core hop (the bench pins ITSELF to cores 2 and 3).
    step("cross_core", capture_cross_core(&dir));
    // 3. Deterministic instruction counts (callgrind).
    step("matcher_iai", capture_iai(&dir));
    // 4. Zero-allocation proofs (dhat).
    for krate in ALLOC_CRATES.iter().copied() {
        step(&format!("alloc {krate}"), capture_alloc(krate, &dir));
    }
    // 5. perf counters (IPC, cache-miss%, branch-miss%).
    for krate in ["ipc", "matcher"] {
        let txt = dir.join(format!("{krate}_perfstat.txt"));
        step(&format!("perfstat {krate}"), run_perfstat(krate, Some(&txt)));
    }
    // 6. Render every latency curve this run emitted.
    step("plots", render_plots_in(&dir));

    eprintln!();
    if fails.is_empty() {
        eprintln!("\x1b[32m✓ capture complete.\x1b[0m artifacts in: {}", dir.display());
        eprintln!("  *.md / *.csv / *_hdr/   benchkit reports + raw distributions");
        eprintln!("  *.txt                   pass output (percentiles, counters, proofs)");
        eprintln!("  *.svg                   latency-curve plots");
        eprintln!("  ENVIRONMENT.txt         host, kernel, cores, effective clock");
        Ok(())
    } else {
        Err(format!("capture INCOMPLETE — failed: {}", fails.join(", ")))
    }
}

fn capture_latency(krate: &str, bench: &str, core: &str, dir: &Path) -> Result<(), String> {
    eprintln!("-> latency: {krate}/{bench} (cpu{core})");
    let bin = build_bench_binary(krate, bench)?;
    let mut cmd = Command::new("taskset");
    cmd.args(["-c", core, &bin]).env("BENCH_OUT_DIR", dir);
    tee_run(cmd, &dir.join(format!("{krate}_latency.txt")))
}

fn capture_cross_core(dir: &Path) -> Result<(), String> {
    eprintln!("-> cross-core hop: ipc/cross_core (cores 2,3)");
    let bin = build_bench_binary("ipc", "cross_core")?;
    let mut cmd = Command::new(bin);
    cmd.env("BENCH_OUT_DIR", dir);
    tee_run(cmd, &dir.join("ipc_cross_core.txt"))
}

/// The iai bench must run under cargo (the iai-callgrind runner drives
/// valgrind), so its txt carries a little cargo noise — acceptable.
fn capture_iai(dir: &Path) -> Result<(), String> {
    eprintln!("-> cache: matcher/matcher_iai (callgrind)");
    let mut cmd = Command::new("cargo");
    cmd.args(["bench", "-p", "matcher", "--features", "iai", "--bench", "matcher_iai"]);
    tee_run(cmd, &dir.join("matcher_iai.txt"))
}

fn capture_alloc(krate: &str, dir: &Path) -> Result<(), String> {
    eprintln!("-> alloc: {krate}/alloc_proof (dhat)");
    let bin = cargo_executable(&["build", "--release", "--example", "alloc_proof", "-p", krate])?;
    let mut cmd = Command::new(bin);
    cmd.env("BENCH_OUT_DIR", dir);
    tee_run(cmd, &dir.join(format!("{krate}_alloc.txt")))
}

/// Render every `.gnuplot` script in the capture dir (each latency report
/// emits one), plus the cross_core curve — that bench writes a raw `.hdr`
/// with no script, so the recipe lives here.
fn render_plots_in(dir: &Path) -> Result<(), String> {
    eprintln!("-> plots: rendering latency-curve SVGs");
    let mut failed = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".gnuplot"))
        .collect();
    names.sort();
    for name in &names {
        let ok = Command::new("gnuplot")
            .arg(name)
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            eprintln!("  rendered {}", name.replace(".gnuplot", ".svg"));
        } else {
            failed.push(name.clone());
        }
    }
    if dir.join("ipc_cross_core.hdr").exists() {
        // HdrHistogram percentile columns: value, percentile, count, 1/(1-pct).
        // Plot value (col 1) against the log-percentile axis (col 4).
        let script = "set terminal svg size 1100,680 font 'sans,11' background '#ffffff'; \
             set output 'ipc_cross_core.svg'; \
             set title 'ipc cross-core hop — latency by percentile'; \
             set xlabel 'Percentile'; set ylabel 'Latency (ns)'; \
             set logscale x; set grid; set datafile commentschars '#'; \
             set xtics ('p50' 2, 'p90' 10, 'p99' 100, 'p99.9' 1000, 'p99.99' 10000, 'p99.999' 100000); \
             plot 'ipc_cross_core.hdr' using 4:1 with lines lw 2 title 'hop'";
        let ok = Command::new("gnuplot")
            .args(["-e", script])
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            eprintln!("  rendered ipc_cross_core.svg");
        } else {
            failed.push("ipc_cross_core (direct)".into());
        }
    }
    if !failed.is_empty() {
        return Err(format!("gnuplot failed for: {}", failed.join(", ")));
    }
    if names.is_empty() {
        return Err("no .gnuplot scripts found to render".into());
    }
    Ok(())
}

/// Run-level provenance: what the per-report headers DON'T carry — the kernel,
/// the topology dump, which cores the session used, and the measured effective
/// clock. Per-bench conditions (commit, rustc, governor, clock policy,
/// affinity) are in each report's own header.
fn write_environment(dir: &Path, lat: &str, perf: &str, eff: Option<f64>) -> Result<(), String> {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# dex-core perf capture — run-level provenance");
    let _ = writeln!(s, "date_utc : {}", shell_line("date", &["-u", "+%FT%TZ"]));
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);
    let _ = writeln!(
        s,
        "git_rev  : {}{}",
        shell_line("git", &["rev-parse", "--short", "HEAD"]),
        if dirty { " (dirty)" } else { "" }
    );
    let _ = writeln!(s, "kernel   : {}", shell_line("uname", &["-a"]));
    let _ = writeln!(s, "cores    : latency=cpu{lat} perf=cpu{perf} (cross_core self-pins 2,3)");
    for core in [lat, perf] {
        if let Ok(n) = core.parse::<u32>() {
            let p = benchkit::CorePrep::detect(n);
            let khz = |v: Option<u64>| v.map(|k| k.to_string()).unwrap_or_else(|| "?".into());
            let sibs = if p.smt_siblings_online.is_empty() {
                "none".to_string()
            } else {
                p.smt_siblings_online.iter().map(|c| format!("cpu{c}")).collect::<Vec<_>>().join(",")
            };
            let _ = writeln!(
                s,
                "cpu{n}     : governor={} clock min={} max={} kHz, online SMT siblings: {}",
                p.governor,
                khz(p.clock_min_khz),
                khz(p.clock_max_khz),
                sibs
            );
        }
    }
    let _ = writeln!(
        s,
        "clock_eff: {} (1s busy-loop probe on cpu{lat}; the rate the benches actually ran at)",
        eff.map(|g| format!("{g:.2} GHz")).unwrap_or_else(|| "unmeasured".into())
    );
    let _ = writeln!(s, "\nPer-bench commit/rustc/governor/clock/affinity are in each report's header.");
    let _ = writeln!(s, "\n## lscpu\n{}", shell_line("lscpu", &[]));
    std::fs::write(dir.join("ENVIRONMENT.txt"), s)
        .map_err(|e| format!("write ENVIRONMENT.txt: {e}"))
}

/// Cycles per second of a 1s busy loop pinned to `core` — the clock the benches
/// will actually run at. Under turbo the cap is whatever HWP grants, and on
/// nohz_full cores `scaling_cur_freq` is stale, so we measure instead of trust.
fn effective_clock_ghz(core: &str) -> Option<f64> {
    let out = Command::new("taskset")
        .args(["-c", core, "perf", "stat", "-e", "cycles", "-x,", "--", "timeout", "1", "yes"])
        .stdout(std::process::Stdio::null())
        .output()
        .ok()?;
    // perf -x, writes CSV to stderr: value,unit,event,… (timeout's exit 124 is
    // expected — the counters still print).
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .lines()
        .find(|l| l.contains(",cycles"))
        .and_then(|l| l.split(',').next())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|cycles| cycles / 1e9)
}

/// One-line stdout of a command; "?" on any failure.
fn shell_line(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "?".into())
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
         cargo bench-all --compare <baseline.csv> <current.csv> [--noise <pct>]\n  \
         cargo bench-all capture [--out <dir>]    one-shot canonical capture into\n                                           \
         docs/perf/<run-id> (prep notes: see run_capture's doc comment)\n\n\
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
         cargo bench-all --compare target/bench-runs/<shaA>/matcher.csv target/bench-runs/<shaB>/matcher.csv",
        bench_crate_list()
    );
}
