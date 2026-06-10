//! Arena alloc/remove benchmarks, on the `benchkit` harness.
//!
//! Run with: `cargo bench -p arena --bench arena_bench`
//!
//! benchkit supplies the warmup/measure `rdtscp` fence, env detection, and
//! report rendering; this file supplies only the scenarios. Alloc and remove are
//! measured as SEPARATE scenarios (the old coupled loop's own comment wished for
//! this) so each gets its own clean percentile distribution.
//!
//! Steady-state design: both scenarios keep arena occupancy fixed so neither
//! drifts toward full/empty during measurement. `alloc` removes the just-alloc'd
//! slot in the *next* prepare; `remove` re-allocs a slot in prepare so the timed
//! remove always has something to free.

use benchkit::{Iters, Report, RunEnv, Runner, Sample};
use std::hint::black_box;

use arena::{Arena, ArenaIdx};

const STD: Iters = Iters::STANDARD;

// alloc: time one `alloc`, then free it untimed in the next prepare so occupancy
// stays flat (capacity never runs out across 1M+ iterations).
fn alloc(r: &Runner) -> Sample {
    struct St {
        arena: Arena<u64>,
        last: Option<ArenaIdx>,
    }
    let mut st = St { arena: Arena::<u64>::new(1_024), last: None };
    r.bench(
        "alloc",
        STD,
        &mut st,
        |st, _i| {
            if let Some(idx) = st.last.take() {
                st.arena.remove(idx);
            }
        },
        |st, ()| {
            let idx = st.arena.alloc(black_box(44u64)).expect("arena full");
            st.last = Some(idx);
            idx
        },
    )
}

// remove: re-alloc a slot in prepare (untimed) so the timed `remove` always has
// a live slot to free; occupancy stays flat.
fn remove(r: &Runner) -> Sample {
    struct St {
        arena: Arena<u64>,
        pending: Option<ArenaIdx>,
    }
    let mut st = St { arena: Arena::<u64>::new(1_024), pending: None };
    r.bench(
        "remove",
        STD,
        &mut st,
        |st, _i| {
            st.pending = Some(st.arena.alloc(44u64).expect("arena full"));
        },
        |st, ()| {
            let idx = st.pending.take().expect("prepare allocated a slot");
            black_box(st.arena.remove(idx))
        },
    )
}

fn main() -> std::io::Result<()> {
    let runner = Runner::new();
    let env = RunEnv::detect(runner.calibration());

    let alloc_s = alloc(&runner);
    let remove_s = remove(&runner);

    let scope = "single arena op (`alloc` / `remove`) on `Arena<u64>`; \
                 generational-index slot management, no I/O.";
    let mut report = Report::new(&env, "Arena benchmark", scope);
    report.section("Slot management", &[&alloc_s, &remove_s]);

    let paths = report.write_run("arena")?;
    eprintln!(
        "benchmark run written:\n  {}\n  {}\n  {}/*.hdr",
        paths.markdown.display(),
        paths.csv.display(),
        paths.hdr_dir.display()
    );
    Ok(())
}
