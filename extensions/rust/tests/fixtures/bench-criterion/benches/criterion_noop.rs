use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::time::Duration;

fn criterion_noop(c: &mut Criterion) {
    c.bench_function("tiny_workload", |b| {
        b.iter(|| bench_criterion_fixture::tiny_workload().wrapping_add(black_box(1)))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_millis(20))
        .warm_up_time(Duration::from_millis(5));
    targets = criterion_noop
}
criterion_main!(benches);
