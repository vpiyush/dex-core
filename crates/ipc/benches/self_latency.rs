//! ipc `Queue` publish/poll self-latency, on the `benchkit` harness.
//!
//! Run with: `cargo bench -p ipc --bench self_latency`
//!
//! Single-thread, self-timed: the cost of one `publish` and one `poll` (hit and
//! miss) in isolation. The end-to-end cross-core number (Blog #2's <50 ns p99)
//! is a separate bench (`cross_core`) — this one isolates the per-op work with
//! no coherence traffic.

use benchkit::{Iters, Report, RunEnv, Runner, Sample};
use ipc::{Consumer, LapPolicy, Producer, Queue};
use std::hint::black_box;

const STD: Iters = Iters::STANDARD;
const CAP: u32 = 1024;

// publish: one Relaxed store + release fence + data write + Release commit. The
// ring wraps every CAP publishes (overwriting), so publish always succeeds and
// occupancy never matters.
fn publish(r: &Runner) -> Sample {
    let (_q, mut prod) = Queue::<u64>::new(CAP);
    r.bench(
        "publish",
        STD,
        &mut prod,
        |_p, _i| {},
        |p, ()| p.publish(black_box(0xABCD_u64)),
    )
}

// poll (hit): prepare publishes one value (untimed); the timed poll reads it.
// Producer and consumer advance in lockstep, so the consumer is never lapped
// and the poll always lands on a freshly-committed slot.
fn poll_hit(r: &Runner) -> Sample {
    struct St {
        prod: Producer<u64>,
        cons: Consumer<u64>,
    }
    let (q, prod) = Queue::<u64>::new(CAP);
    let cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    let mut st = St { prod, cons };
    r.bench(
        "poll (hit)",
        STD,
        &mut st,
        |st, _i| {
            st.prod.publish(0xABCD);
        },
        |st, ()| black_box(st.cons.poll()),
    )
}

// poll (miss): the consumer is caught up, so every poll returns Empty — the
// cheapest path (one Acquire load + a compare, no data read).
fn poll_miss(r: &Runner) -> Sample {
    let (q, _prod) = Queue::<u64>::new(CAP);
    let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    r.bench(
        "poll (miss)",
        STD,
        &mut cons,
        |_c, _i| {},
        |c, ()| black_box(c.poll()),
    )
}

fn main() -> std::io::Result<()> {
    let runner = Runner::new();
    let env = RunEnv::detect(runner.calibration());

    let publish_s = publish(&runner);
    let poll_hit_s = poll_hit(&runner);
    let poll_miss_s = poll_miss(&runner);

    let scope = "single Queue<u64> op in isolation (publish / poll-hit / poll-miss); \
                 one thread, self-timed, no cross-core traffic.";
    let mut report = Report::new(&env, "ipc self-latency", scope);
    report.section("publish", &[&publish_s]);
    report.section("poll", &[&poll_hit_s, &poll_miss_s]);

    let paths = report.write_run("bench-runs", "ipc_self_latency")?;
    eprintln!(
        "benchmark run written:\n  {}\n  {}\n  {}/*.hdr",
        paths.markdown.display(),
        paths.csv.display(),
        paths.hdr_dir.display()
    );
    Ok(())
}
