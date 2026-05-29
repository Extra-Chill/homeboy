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
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

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
    let release_homeboy_bin: PathBuf = [&manifest_dir, "target", "release", "homeboy"]
        .iter()
        .collect();

    ensure_release_homeboy(&manifest_dir, &release_homeboy_bin);
    let homeboy_bin = stable_homeboy_copy(&release_homeboy_bin);

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
    let mut peak_rss_bytes: Vec<u64> = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let start = Instant::now();
        let output = run_audit_iteration(&homeboy_bin, &fixture_path);
        let elapsed = start.elapsed();

        match output.status {
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
        if let Some(rss) = output.peak_rss_bytes {
            peak_rss_bytes.push(rss);
        }
        eprintln!(
            "[bench-audit-self] iteration {}/{}: {:.2}ms, peak_rss={}",
            i + 1,
            iterations,
            elapsed.as_secs_f64() * 1000.0,
            output
                .peak_rss_bytes
                .map(|rss| rss.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    // Emit the contract JSON on the last stdout line.
    let csv: String = timings_ns
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(",");
    println!("{}", result_json(&csv, &peak_rss_bytes));
}

fn result_json(timings_csv: &str, peak_rss_bytes: &[u64]) -> String {
    let rss_csv: String = peak_rss_bytes
        .iter()
        .map(|rss| rss.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let max_peak_rss = peak_rss_bytes.iter().copied().max().unwrap_or(0);
    format!(
        "{{\"timings_ns\":[{}],\"peak_rss_bytes\":{},\"peak_rss_bytes_by_iteration\":[{}]}}",
        timings_csv, max_peak_rss, rss_csv
    )
}

struct AuditIterationOutput {
    status: std::io::Result<std::process::ExitStatus>,
    peak_rss_bytes: Option<u64>,
}

fn audit_command(homeboy_bin: &PathBuf, fixture_path: &str) -> Command {
    let mut command = Command::new(homeboy_bin);
    command
        .args([
            "audit",
            "homeboy", // positional component id required by audit subcommand
            "--path",
            fixture_path,
            "--ignore-baseline",
            "--json-summary",
        ])
        .stdout(Stdio::null()) // we only care about timing, not output
        .stderr(Stdio::null());
    command
}

#[cfg(unix)]
fn run_audit_iteration(homeboy_bin: &PathBuf, fixture_path: &str) -> AuditIterationOutput {
    let child = match audit_command(homeboy_bin, fixture_path).spawn() {
        Ok(child) => child,
        Err(err) => {
            return AuditIterationOutput {
                status: Err(err),
                peak_rss_bytes: None,
            }
        }
    };

    let mut raw_status = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let waited = unsafe {
        libc::wait4(
            child.id() as libc::pid_t,
            &mut raw_status,
            0,
            usage.as_mut_ptr(),
        )
    };
    if waited < 0 {
        return AuditIterationOutput {
            status: Err(std::io::Error::last_os_error()),
            peak_rss_bytes: None,
        };
    }

    let usage = unsafe { usage.assume_init() };
    AuditIterationOutput {
        status: Ok(std::process::ExitStatus::from_raw(raw_status)),
        peak_rss_bytes: Some(maxrss_to_bytes(usage.ru_maxrss)),
    }
}

#[cfg(not(unix))]
fn run_audit_iteration(homeboy_bin: &PathBuf, fixture_path: &str) -> AuditIterationOutput {
    AuditIterationOutput {
        status: audit_command(homeboy_bin, fixture_path).status(),
        peak_rss_bytes: None,
    }
}

#[cfg(target_os = "macos")]
fn maxrss_to_bytes(maxrss: libc::c_long) -> u64 {
    u64::try_from(maxrss).unwrap_or(0)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn maxrss_to_bytes(maxrss: libc::c_long) -> u64 {
    u64::try_from(maxrss).unwrap_or(0).saturating_mul(1024)
}

fn ensure_release_homeboy(manifest_dir: &str, homeboy_bin: &PathBuf) {
    if homeboy_bin.exists() {
        return;
    }

    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--manifest-path",
            &format!("{}/Cargo.toml", manifest_dir),
            "--bin",
            "homeboy",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(status) if status.success() && homeboy_bin.exists() => {}
        Ok(status) => {
            eprintln!(
                "FATAL: failed to build release Homeboy binary at {} (exit {})",
                homeboy_bin.display(),
                status.code().unwrap_or(-1)
            );
            std::process::exit(2);
        }
        Err(err) => {
            eprintln!("FATAL: failed to spawn cargo build for Homeboy: {}", err);
            std::process::exit(2);
        }
    }
}

fn stable_homeboy_copy(homeboy_bin: &PathBuf) -> PathBuf {
    let stable_path = env::temp_dir().join(format!(
        "homeboy-bench-audit-self-{}{}",
        std::process::id(),
        env::consts::EXE_SUFFIX
    ));
    if let Err(err) = fs::copy(homeboy_bin, &stable_path) {
        eprintln!(
            "FATAL: failed to copy Homeboy binary from {} to {}: {}",
            homeboy_bin.display(),
            stable_path.display(),
            err
        );
        std::process::exit(2);
    }
    stable_path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_json_includes_max_and_per_iteration_peak_rss() {
        let value: serde_json::Value = serde_json::from_str(&result_json("10,20", &[4096, 8192]))
            .expect("result payload should be valid JSON");

        assert_eq!(value["timings_ns"], serde_json::json!([10, 20]));
        assert_eq!(value["peak_rss_bytes"], serde_json::json!(8192));
        assert_eq!(
            value["peak_rss_bytes_by_iteration"],
            serde_json::json!([4096, 8192])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn maxrss_to_bytes_preserves_macos_byte_units() {
        assert_eq!(maxrss_to_bytes(4096), 4096);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn maxrss_to_bytes_converts_unix_kib_units() {
        assert_eq!(maxrss_to_bytes(4), 4096);
    }
}
