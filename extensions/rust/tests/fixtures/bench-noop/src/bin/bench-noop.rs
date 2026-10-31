//! Noop bench fixture — measures the harness floor.
//!
//! Exercises the contract end-to-end without doing anything substantive
//! per iteration. Useful for asserting that the dispatcher round-trips
//! a valid envelope; per-iteration timings will be in the nanoseconds-to-
//! low-microseconds range (compiler optimization-dependent).
//!
//! Contract (must match scripts/bench/bench-runner.sh):
//!   - Read iteration count from $HOMEBOY_BENCH_ITERATIONS (default 10).
//!   - Time each iteration with std::time::Instant.
//!   - Emit a single JSON object on the last stdout line:
//!         {"timings_ns": [u64, ...], "peak_rss_bytes": u64}

use std::env;
use std::hint::black_box;
use std::time::Instant;

fn bench_main() -> u64 {
    // black_box on a constant prevents the compiler from optimizing the
    // call away entirely. The work itself is intentionally trivial — this
    // fixture measures harness overhead, not workload work.
    black_box(1u64).wrapping_add(black_box(2u64))
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

    // Render JSON manually — fixture has no serde dep on purpose. Real
    // components will likely use serde, but the contract doesn't require it.
    let timings_csv: String = timings
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(",");

    println!("{{\"timings_ns\":[{}]}}", timings_csv);
}
