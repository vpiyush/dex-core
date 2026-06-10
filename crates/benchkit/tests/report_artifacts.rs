//! End-to-end: run scenarios, compose a Report, write the artifact set, and
//! validate the files exist and are well-formed. Exercises the full
//! Runner → Report → write_run path a consumer uses.

use std::hint::black_box;

use benchkit::{Delta, Iters, Report, RunEnv, Runner};

#[test]
fn writes_markdown_csv_and_hdr_files() {
    let runner = Runner::new();
    let env = RunEnv::detect(runner.calibration());
    let iters = Iters::new(500, 5_000);

    let light = runner.bench_static("light", iters, &mut 0u64, |s| {
        *s = s.wrapping_add(1);
        black_box(*s)
    });
    let heavy = runner.bench_static("heavy", iters, &mut 0u64, |s| {
        for _ in 0..50 {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        black_box(*s)
    });

    let mut report = Report::new(&env, "smoke_suite", "unit-test scope: synthetic ops");
    report.section("Ops", &[&light, &heavy]);
    report.delta("heavy - light", &heavy, &light, Delta::Sub, "heavy p50 minus light p50");

    // Markdown sanity.
    let md = report.to_markdown();
    assert!(md.contains("smoke_suite"));
    assert!(md.contains("measured under: back-to-back"));
    assert!(md.contains("| light |"));
    assert!(md.contains("Headline deltas"));

    // CSV: header + 2 data rows, regime column present.
    let csv = report.to_csv();
    let lines: Vec<&str> = csv.lines().collect();
    assert!(lines[0].starts_with("regime,"));
    assert_eq!(lines.iter().filter(|l| l.starts_with("back_to_back,")).count(), 2);

    // Write to a unique temp dir via the BENCH_OUT_DIR override.
    let dir = std::env::temp_dir().join(format!("benchkit_smoke_{}", std::process::id()));
    let dir_str = dir.to_str().unwrap();
    // SAFETY: single-threaded test; we set and use the override locally.
    unsafe { std::env::set_var("BENCH_OUT_DIR", dir_str) };
    let paths = report.write_run("smoke_suite").unwrap();
    unsafe { std::env::remove_var("BENCH_OUT_DIR") };

    assert!(paths.markdown.exists(), "markdown written");
    assert!(paths.csv.exists(), "csv written");
    assert!(paths.hdr_dir.join("light.hdr").exists(), "per-scenario hdr written");
    assert!(paths.hdr_dir.join("heavy.hdr").exists());

    // The hdr file is the online-plotter format.
    let hdr = std::fs::read_to_string(paths.hdr_dir.join("light.hdr")).unwrap();
    assert!(hdr.contains("Value") && hdr.contains("Percentile"));

    // The gnuplot script exists, targets the right SVG, and references each
    // scenario's .hdr via the relative hdr dirname (so it runs from bench-runs/).
    assert!(paths.gnuplot.exists(), "gnuplot script written");
    let gp = std::fs::read_to_string(&paths.gnuplot).unwrap();
    let svg_name = paths.svg.file_name().unwrap().to_string_lossy();
    let hdr_dirname = paths.hdr_dir.file_name().unwrap().to_string_lossy();
    assert!(gp.contains(&format!("set output '{svg_name}'")), "script outputs the run's SVG");
    assert!(gp.contains(&format!("{hdr_dirname}/light.hdr")), "script plots each scenario hdr");
    assert!(gp.contains(&format!("{hdr_dirname}/heavy.hdr")));
    assert!(gp.contains("using 4:1"), "plots latency (col1) vs 1/(1-percentile) (col4)");
    // No stray `%` in tic labels (gnuplot treats it as a format char).
    assert!(!gp.contains('%'), "tic labels must not contain % (gnuplot format char)");

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}
