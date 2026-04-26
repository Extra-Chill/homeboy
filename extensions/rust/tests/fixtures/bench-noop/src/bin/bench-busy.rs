//! Busy bench fixture — measurable workload.
//!
//! Does enough work per iteration that the timings_ns values land
//! comfortably above timer resolution and noticeably above bench-noop's
//! floor. Exercise the harness's percentile math on a non-degenerate
//! distribution.
//!
//! Same contract as bench-noop.rs.

use std::env;
use std::hint::black_box;
use std::time::Instant;

fn bench_main() -> u64 {
    // Sum integers 0..10_000 with black_box around each input to defeat
    // const-folding. ~10us per iteration on modern hardware — well above
    // timer noise without making the smoke slow.
    let mut acc: u64 = 0;
    for i in 0..10_000u64 {
        acc = acc.wrapping_add(black_box(i));
    }
    acc
}

fn main() {
    let iterations: usize = env::var("HOMEBOY_BENCH_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let mut timings = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let _ = bench_main();
        timings.push(start.elapsed().as_nanos() as u64);
    }

    let timings_csv: String = timings
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(",");

    println!("{{\"timings_ns\":[{}]}}", timings_csv);
}
