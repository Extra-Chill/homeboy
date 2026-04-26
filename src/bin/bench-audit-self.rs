//! Self-audit bench — measures `homeboy audit` end-to-end against a fixture
//! component (homeboy itself).
//!
//! This is the canonical dogfood for the rust-bench capability. The bench
//! harness invokes this binary; it shells out to the `homeboy` CLI binary
//! built alongside it, runs `homeboy audit homeboy --ignore-baseline` N times,
//! and emits per-iteration timings as the contract requires.
//!
//! WHY SHELL OUT INSTEAD OF CALLING THE LIB DIRECTLY
//!
//! The bench-pair workflow (`homeboy bench homeboy --rig main,perf-branch`)
//! cares about the full user-facing perf experience: argument parsing,
//! component resolution, audit pipeline, report assembly, output rendering.
//! A library-only call would skip the CLI surface and underestimate the
//! cost users actually pay. Shelling out via std::process::Command is the
//! honest measurement.
//!
//! CONTRACT
//!
//! See homeboy-extensions/rust/scripts/bench/bench-runner.sh.
//! Reads HOMEBOY_BENCH_ITERATIONS, runs that many audit invocations,
//! emits {"timings_ns": [...], "peak_rss_bytes": N} on the last stdout line.

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() {
    let iterations: usize = env::var("HOMEBOY_BENCH_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    // Locate the homeboy CLI binary from the same workspace target dir.
    // CARGO_MANIFEST_DIR points at the homeboy crate root; the harness
    // invokes us via `cargo run --release --bin bench-audit-self`, so the
    // sibling binary lives at target/release/homeboy.
    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set (run via cargo)");
    let homeboy_bin: PathBuf = [&manifest_dir, "target", "release", "homeboy"]
        .iter()
        .collect();

    if !homeboy_bin.exists() {
        eprintln!(
            "FATAL: homeboy binary not found at {} — run `cargo build --release` first",
            homeboy_bin.display()
        );
        std::process::exit(2);
    }

    // Audit fixture: homeboy itself. The bench measures `homeboy audit homeboy`,
    // which scans the same source tree we're running from. Substantive enough
    // to give measurable wall time (seconds) without being so large the bench
    // takes forever.
    let fixture_path = manifest_dir.clone();

    eprintln!(
        "[bench-audit-self] iterations={}, binary={}, fixture={}",
        iterations,
        homeboy_bin.display(),
        fixture_path
    );

    let mut timings_ns: Vec<u64> = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let start = Instant::now();
        let status = Command::new(&homeboy_bin)
            .args([
                "audit",
                "homeboy", // positional component id required by audit subcommand
                "--path",
                &fixture_path,
                "--ignore-baseline",
                "--json-summary",
            ])
            .stdout(Stdio::null()) // we only care about timing, not output
            .stderr(Stdio::null())
            .status();
        let elapsed = start.elapsed();

        match status {
            Ok(s) if s.success() || s.code() == Some(1) => {
                // exit 1 = audit found findings; that's a normal "I did work"
                // outcome, not a bench failure. Anything else (panics,
                // 2 = validation error) is a real failure.
            }
            Ok(s) => {
                eprintln!(
                    "FATAL: iteration {}/{} — homeboy audit exited {} (unexpected)",
                    i + 1,
                    iterations,
                    s.code().unwrap_or(-1)
                );
                std::process::exit(3);
            }
            Err(e) => {
                eprintln!(
                    "FATAL: iteration {}/{} — failed to spawn homeboy: {}",
                    i + 1,
                    iterations,
                    e
                );
                std::process::exit(4);
            }
        }

        timings_ns.push(elapsed.as_nanos() as u64);
        eprintln!(
            "[bench-audit-self] iteration {}/{}: {:.2}ms",
            i + 1,
            iterations,
            elapsed.as_secs_f64() * 1000.0
        );
    }

    // Emit the contract JSON on the last stdout line.
    let csv: String = timings_ns
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(",");
    println!("{{\"timings_ns\":[{}]}}", csv);
}
