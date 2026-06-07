#![cfg(not(loom))]
use ipc::{LapPolicy, PollResult, Queue};

#[test]
fn publish_then_poll_roundtrip() {
    let (q, mut prod) = Queue::<u64>::new(8);
    let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);

    for i in 0..5u64 {
        prod.publish(i*10);
    }
    for i in 0..5u64 {
       assert_eq!( cons.poll(), PollResult::Ready(i*10));
    }
    assert_eq!(cons.poll(), PollResult::Empty);
}

#[test]
fn subscribe_after_publish_sees_no_history() {
    let (q, mut prod) = Queue::<u64>::new(8);
    for i in 0..5u64 {
        prod.publish(i);
    }
    let mut late = Queue::subscribe(&q, LapPolicy::SkipToLatest);
    assert_eq!(late.poll(), PollResult::Empty); // history not replayed
}
