#![cfg(not(loom))]
use ipc::{LapPolicy, PollResult, Queue};


#[test]
fn halt_is_sticky() {
    let (q, mut prod) = Queue::<u64>::new(4);
    let mut cons = Queue::subscribe(&q, LapPolicy::Halt);
    for i in 0..6u64 { prod.publish(i); } // 6 > capacity 4 → laps consumer at seq 0

    assert_eq!(cons.poll(), PollResult::Halted { last_safe_seq: 0, slots_lost: 6 });
    assert_eq!(cons.poll(), PollResult::Halted { last_safe_seq: 0, slots_lost: 6 }); // sticky
    assert!(cons.is_halted());
}

#[test]
fn skip_to_latest_resyncs_then_resumes() {
    let (q, mut prod) = Queue::<u64>::new(4);
    let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    for i in 0..6u64 { prod.publish(i); }

    assert_eq!(cons.poll(), PollResult::Skipped { slots_lost: 6, new_seq: 6 });
    assert_eq!(cons.poll(), PollResult::Empty); // resynced to live edge (seq 6), nothing there
    prod.publish(999);
    assert_eq!(cons.poll(), PollResult::Ready(999)); // resumes normally
}

#[test]
#[should_panic(expected = "lapped")]
fn panic_policy_panics() {
    let (q, mut prod) = Queue::<u64>::new(4);
    let mut cons = Queue::subscribe(&q, LapPolicy::Panic);
    for i in 0..6u64 { prod.publish(i); }
    let _ = cons.poll();
}

