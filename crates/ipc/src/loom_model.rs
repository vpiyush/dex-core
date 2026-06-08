
use crate::{LapPolicy, PollResult, Queue };
use loom::thread;

#[test]
fn spsc_handoff() {
    loom::model(|| {
        let (q, mut prod) = Queue::<u64>::new(2);
        let mut cons = Queue::subscribe(&q, LapPolicy::SkipToLatest);
        let p = thread::spawn(move || {
            prod.publish(10);
            prod.publish(20);
        });

        let mut got = Vec::new();
        for _ in 0..3 {
            if let PollResult::Ready(v) = cons.poll() {
                got.push(v);
            }
        }
        p.join().unwrap();

        match got.as_slice() {
            [] | [10] | [10, 20] => {}
            other => panic!("unexpected response: {:?}", other),
        }
    })
}
