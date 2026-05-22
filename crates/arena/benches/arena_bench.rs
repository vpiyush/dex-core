
use criterion::{criterion_group, criterion_main, Criterion};
use arena::Arena;
use telemetry::primitives::{calibrate, rdtscp_then_lfence, rdtscp, Histogram, Nanos};

fn bench_alloc_remove(c: &mut Criterion) {
    let calibration = calibrate();

    let mut alloc_hist = Histogram::new("Arena alloc");
    let mut remove_hist = Histogram::new("Arena remove");

    let mut arena = Arena::<u64>::new(10_000);
    c.bench_function("alloc_remove", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let v = std::hint::black_box(44u64);
                let t0 = rdtscp_then_lfence();
                let idx = arena.alloc(v).expect("Arena full");
                let t1 = rdtscp();
                std::hint::black_box(idx);
                // separate histogram for alloc-only would be cleaner; or:
                alloc_hist.record(t1.since(t0).to_nanos(&calibration));
                let t2 = rdtscp_then_lfence();
                let _ =std::hint::black_box( arena.remove(idx));
                let t3 = rdtscp();
                remove_hist.record(t3.since(t2).to_nanos(&calibration));

            }
        });
    });
    println!("\n{}", alloc_hist.render_markdown());
    println!("\n{}", remove_hist.render_markdown());
}

criterion_group!(benches, bench_alloc_remove);
criterion_main!(benches);