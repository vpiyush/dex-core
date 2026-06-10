//! ipc cross-core hop latency — the Blog #2 deliverable (HLD budgets ~30–50 ns/hop).
//!
//! Producer (core A) stamps `origin = rdtscp()` into a synthetic, order-event-
//! sized message and publishes at a fixed rate — **open-loop**, so a slow reader
//! shows up as a fat tail instead of being silently skipped (coordinated
//! omission). Consumer (core B) reads and records `now − origin` into an HDR
//! histogram with CO correction. Reports p50 / p99 / p99.99.
//!
//! Run: `cargo bench -p ipc --bench cross_core`
//! For a CREDIBLE number, isolate the cores and pin the clock first:
//!   sudo cpupower frequency-set -g performance
//!   # boot with `isolcpus=2,3`; otherwise expect OS-jitter in the tail.
//! Assumes an invariant, cross-core-synchronised TSC (true on modern single-
//! socket x86) — otherwise the core-A → core-B delta is meaningless.

use ipc::{LapPolicy, PollResult, Queue};
use std::sync::atomic::{AtomicBool, Ordering};
use telemetry::primitives::{calibrate, rdtscp, Histogram, Nanos, TscTicks};

// 48-byte payload (~ order-event-sized); element 0 carries the rdtscp origin
// tick. Hop cost is cache-line-transfer bound, so this stands in for any
// <=56-byte wire message.
type Msg = [u64; 6];

const PRODUCER_CORE: usize = 2;
const CONSUMER_CORE: usize = 3;
const CAP: u32 = 1024; // ample headroom — the consumer never laps at this rate
const SAMPLES: u64 = 1_000_000; // ~1s of data at the send rate
const SEND_INTERVAL_NS: u64 = 1_000; // 1 µs → 1M msg/s, well below saturation

fn pin(core: usize) {
    let ids = core_affinity::get_core_ids().expect("core ids unavailable");
    let id = *ids.get(core).expect("requested core does not exist");
    assert!(core_affinity::set_for_current(id), "failed to pin to core {core}");
}

fn main() {
    let cal = calibrate();
    let interval_ticks = (SEND_INTERVAL_NS as f64 * cal.ticks_per_nanosecond) as u64;

    let (q, mut prod) = Queue::<Msg>::new(CAP);
    let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    let done = AtomicBool::new(false);
    let mut hist = Histogram::new("cross_core_hop_ns");

    std::thread::scope(|s| {
        // producer: paced, open-loop, pinned to core A
        s.spawn(|| {
            pin(PRODUCER_CORE);
            let mut next = rdtscp().0;
            for _ in 0..SAMPLES {
                next = next.wrapping_add(interval_ticks);
                while rdtscp().0 < next {
                    std::hint::spin_loop();
                }
                let mut msg: Msg = [0; 6];
                msg[0] = rdtscp().0; // origin = actual send time
                prod.publish(msg);
            }
            done.store(true, Ordering::Relaxed);
        });
        // consumer: records hop latency (CO-corrected), pinned to core B
        s.spawn(|| {
            pin(CONSUMER_CORE);
            let mut recorded = 0u64;
            while recorded < SAMPLES {
                match cons.poll() {
                    PollResult::Ready(msg) => {
                        let lat = rdtscp().since(TscTicks(msg[0])).to_nanos(&cal);
                        hist.record_correct(lat, Nanos(SEND_INTERVAL_NS));
                        recorded += 1;
                    }
                    PollResult::Empty => {
                        if done.load(Ordering::Relaxed) {
                            break; // producer finished and we've drained
                        }
                        std::hint::spin_loop();
                    }
                    PollResult::Skipped { .. } => {} // not expected at this rate
                    PollResult::Halted { .. } => unreachable!("SkipToLatest never halts"),
                }
            }
        });
    });

    // benchkit::out_dir resolves BENCH_OUT_DIR / workspace-root bench-runs —
    // a bare relative path would land in crates/ipc/ (cargo bench sets the
    // package dir as cwd), invisible next to every other artifact.
    let out = benchkit::out_dir();
    std::fs::create_dir_all(&out).expect("create out dir");
    let hdr_path = out.join("ipc_cross_core.hdr");
    std::fs::write(&hdr_path, hist.render_hdr_percentiles(5)).expect("write hdr");

    println!(
        "ipc cross-core hop latency — cores {PRODUCER_CORE}→{CONSUMER_CORE}, {} samples \
         (open-loop {SEND_INTERVAL_NS} ns; assumes invariant cross-core TSC)",
        hist.len()
    );
    println!("  min     {:>6} ns", hist.min().as_u64());
    println!("  p50     {:>6} ns", hist.p50().as_u64());
    println!("  p99     {:>6} ns", hist.p99().as_u64());
    println!("  p99.99  {:>6} ns", hist.p99_99().as_u64());
    println!("  max     {:>6} ns", hist.max().as_u64());
    eprintln!("hdr written: {}", hdr_path.display());
}
