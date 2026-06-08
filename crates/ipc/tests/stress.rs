#![cfg(not(loom))]
//! Concurrent torn-read stress. The payload `[u64; N]` is filled with a single
//! monotonic counter, so every element should be equal; a torn read (one that
//! mixed two writes) would show mismatched elements and is caught. The ring is
//! kept tight so the producer constantly overwrites the consumer's position.
//!
//! This is the primary coverage for the racy data path that loom can't model
//! (the optimistic read is a deliberate C11 race). It runs the real code on
//! real hardware at volume — what loom, a model, never does.

use ipc::{LapPolicy, PollResult, Queue};
use std::sync::atomic::{AtomicBool, Ordering};

const ITERS: u64 = 2_000_000;

/// Generate a stress test: one producer publishes `[n; $words]` for `n` in
/// `0..ITERS`; one `SkipToLatest` consumer asserts every value it reads is
/// internally consistent (no tear) and never goes backwards (cursor/lap sanity).
macro_rules! stress_test {
    ($name:ident, words = $words:literal, capacity = $cap:literal) => {
        #[test]
        fn $name() {
            let (q, mut prod) = Queue::<[u64; $words]>::new($cap);
            let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
            let done = AtomicBool::new(false);

            std::thread::scope(|s| {
                s.spawn(|| {
                    for n in 0..ITERS {
                        prod.publish([n; $words]);
                    }
                    done.store(true, Ordering::Relaxed);
                });
                s.spawn(|| {
                    let mut last = 0u64;
                    while !done.load(Ordering::Relaxed) {
                        if let PollResult::Ready(v) = cons.poll() {
                            let first = v[0];
                            assert!(v.iter().all(|&x| x == first), "torn read: {v:?}");
                            assert!(first >= last, "went backwards: {first} after {last}");
                            last = first;
                        }
                    }
                });
            });
        }
    };
}

stress_test!(stress_16b_cap2, words = 2, capacity = 2); // tightest ring → max overwrite pressure
stress_test!(stress_32b_cap8, words = 4, capacity = 8);
stress_test!(stress_56b_cap4, words = 7, capacity = 4); // 56-byte payload (max that fits a slot)
