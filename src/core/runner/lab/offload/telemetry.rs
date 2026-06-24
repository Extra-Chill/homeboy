//! Runner-agnostic host-level telemetry for Lab offload / benchmark runs (#3258).
//!
//! Lab scale benchmarks (10/100/500/1000 clients) need to tell *workload*
//! behavior apart from *runner capacity / cleanup* problems. The per-phase
//! timing in [`super::overhead`] (#3001) explains where time went; this module
//! captures the host-resource picture *around* the offloaded run boundary so a
//! report can answer "did the runner run out of capacity?" and "did the run
//! leave artifacts / child processes behind?".
//!
//! The model is a before/after pair of cheap [`HostResourceSnapshot`]s taken on
//! the controller around the run, diffed into a [`LabHostTelemetry`] record
//! that is attached to the offload metadata under `lab_host_telemetry`.
//!
//! Design constraints:
//! - **Best-effort + non-fatal.** Every metric is optional. If a value cannot
//!   be read on the current host/mode it is recorded as unavailable (`None`)
//!   with a `not measured` marker — capture NEVER returns an error and NEVER
//!   fails the benchmark.
//! - **Runner-agnostic.** The same snapshot model applies across local /
//!   SSH / daemon / reverse-tunnel runners; it measures the *controller* host
//!   that orchestrates the run plus the controller-side workspace/artifact dir
//!   the run reads and writes.
//! - **No new heavy dependencies.** Uses only `std` plus `/proc` reads where
//!   available; metrics that need a crate we do not already depend on are
//!   recorded as unavailable rather than pulling in `sysinfo`/`procfs`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::build_identity;

/// Marker recorded for any metric that could not be captured on the current
/// host or execution mode, so a report can distinguish "zero" from "unknown".
const NOT_MEASURED: &str = "not measured";

/// A cheap point-in-time snapshot of host resources, taken once before and once
/// after the offloaded run so growth/leak deltas can be computed.
///
/// All fields are best-effort: a `None` means the metric was not measurable in
/// this environment (it is surfaced as `not measured`, never as a hard zero).
#[derive(Debug, Clone, Default)]
pub(crate) struct HostResourceSnapshot {
    /// Recursive byte size of the watched workspace/artifact directory, if it
    /// exists and was walkable.
    watched_dir_bytes: Option<u64>,
    /// Resident set size (RSS) of the controller process in bytes, if readable
    /// (Linux `/proc/self/statm`).
    process_rss_bytes: Option<u64>,
    /// Total process count on the host, if readable (Linux `/proc` entries).
    host_process_count: Option<u64>,
    /// Count of direct child processes of the controller, if readable
    /// (Linux `/proc/[pid]/stat` ppid scan). Used as the stale-child baseline.
    child_process_count: Option<u64>,
    /// 1-minute load average, if readable (Linux `/proc/loadavg`). A cheap
    /// proxy for CPU pressure where a per-process CPU sample is not available.
    load_average_1m: Option<f64>,
}

impl HostResourceSnapshot {
    /// Capture a snapshot of the current host resources. Never fails: any metric
    /// that cannot be read is left `None`.
    ///
    /// `watched_dir` is the controller-side workspace/artifact directory whose
    /// byte growth across the run we want to track (e.g. the source checkout the
    /// run syncs from and writes structured output beside).
    pub(crate) fn capture(watched_dir: &Path) -> Self {
        Self {
            watched_dir_bytes: dir_size_bytes(watched_dir),
            process_rss_bytes: self_rss_bytes(),
            host_process_count: host_process_count(),
            child_process_count: child_process_count(),
            load_average_1m: load_average_1m(),
        }
    }
}

/// Host-level telemetry for a single Lab offload / benchmark run, derived from a
/// before/after [`HostResourceSnapshot`] pair plus static host identity.
///
/// Attached to the offload metadata under `lab_host_telemetry` so scale-test
/// reports can correlate failures with runner capacity (RSS / load / process
/// count) and cleanup health (artifact byte growth / leftover child processes).
#[derive(Debug, Clone)]
pub(crate) struct LabHostTelemetry {
    /// Controller hostname, if resolvable.
    hostname: Option<String>,
    /// Homeboy version / build identity orchestrating the run (already known —
    /// reused, not re-derived per metric).
    homeboy_version: String,
    homeboy_build: String,
    /// Path of the watched workspace/artifact directory the byte-growth delta
    /// was measured against.
    watched_dir: String,
    /// Wall time of the measured window in milliseconds.
    wall_ms: u64,
    before: HostResourceSnapshot,
    after: HostResourceSnapshot,
}

/// In-progress host-telemetry capture spanning the run boundary.
///
/// Construct with [`LabHostTelemetryCapture::start`] *before* the run, then call
/// [`LabHostTelemetryCapture::finish`] *after* the run to take the closing
/// snapshot and produce the [`LabHostTelemetry`] record. Best-effort: starting
/// and finishing capture never fail.
pub(crate) struct LabHostTelemetryCapture {
    watched_dir: PathBuf,
    started: Instant,
    before: HostResourceSnapshot,
}

impl LabHostTelemetryCapture {
    /// Take the opening snapshot. Call once before the offloaded run begins.
    pub(crate) fn start(watched_dir: &Path) -> Self {
        Self {
            watched_dir: watched_dir.to_path_buf(),
            started: Instant::now(),
            before: HostResourceSnapshot::capture(watched_dir),
        }
    }

    /// Render the host identity + opening snapshot as embeddable metadata
    /// *before* the run completes, so the metadata handed to the runner records
    /// the controller's pre-run host state and machine identity even though the
    /// closing snapshot (and thus the delta) is only known controller-side after
    /// the run. The `after` snapshot is left absent and marked `not measured`.
    pub(crate) fn before_metadata(&self) -> serde_json::Value {
        let identity = build_identity::current();
        LabHostTelemetry {
            hostname: hostname(),
            homeboy_version: identity.version,
            homeboy_build: identity.display,
            watched_dir: self.watched_dir.display().to_string(),
            wall_ms: 0,
            before: self.before.clone(),
            after: HostResourceSnapshot::default(),
        }
        .to_metadata()
    }

    /// Take the closing snapshot and assemble the telemetry record. Call once
    /// after the offloaded run completes (success or failure).
    pub(crate) fn finish(self) -> LabHostTelemetry {
        let after = HostResourceSnapshot::capture(&self.watched_dir);
        let identity = build_identity::current();
        LabHostTelemetry {
            hostname: hostname(),
            homeboy_version: identity.version,
            homeboy_build: identity.display,
            watched_dir: self.watched_dir.display().to_string(),
            wall_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            before: self.before,
            after,
        }
    }
}

impl LabHostTelemetry {
    /// Byte growth of the watched workspace/artifact dir across the run, if both
    /// endpoints were measurable. Negative growth (cleanup shrank the dir) is
    /// represented faithfully via a signed value.
    fn watched_dir_growth_bytes(&self) -> Option<i64> {
        let before = self.before.watched_dir_bytes?;
        let after = self.after.watched_dir_bytes?;
        Some(after as i64 - before as i64)
    }

    /// Best-effort count of child processes still alive after the run that were
    /// not present before it (a stale/leaked-child proxy). `None` when the child
    /// count could not be read on this host/mode.
    fn stale_child_process_count(&self) -> Option<u64> {
        let before = self.before.child_process_count?;
        let after = self.after.child_process_count?;
        Some(after.saturating_sub(before))
    }

    /// Render an optional `u64` metric as JSON, marking absent values rather
    /// than collapsing them to a misleading zero.
    fn opt_u64(value: Option<u64>) -> serde_json::Value {
        match value {
            Some(value) => serde_json::json!(value),
            None => serde_json::json!({ "available": false, "reason": NOT_MEASURED }),
        }
    }

    fn opt_i64(value: Option<i64>) -> serde_json::Value {
        match value {
            Some(value) => serde_json::json!(value),
            None => serde_json::json!({ "available": false, "reason": NOT_MEASURED }),
        }
    }

    fn opt_f64(value: Option<f64>) -> serde_json::Value {
        match value {
            Some(value) => serde_json::json!(value),
            None => serde_json::json!({ "available": false, "reason": NOT_MEASURED }),
        }
    }

    /// Serialize to a runner-agnostic metadata object:
    ///
    /// ```json
    /// {
    ///   "schema": "homeboy/lab-host-telemetry/v1",
    ///   "runner_machine": { "hostname": "...", "homeboy_version": "...", "homeboy_build": "..." },
    ///   "wall_ms": 1234,
    ///   "watched_dir": "/path/to/checkout",
    ///   "artifact_dir_growth_bytes": 4096,
    ///   "stale_child_process_count": 0,
    ///   "process_rss_bytes": { "before": 1, "after": 2 },
    ///   "host_process_count": { "before": 1, "after": 2 },
    ///   "load_average_1m": { "before": 0.1, "after": 0.2 }
    /// }
    /// ```
    ///
    /// Any field that could not be measured renders as
    /// `{ "available": false, "reason": "not measured" }`.
    pub(crate) fn to_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": "homeboy/lab-host-telemetry/v1",
            "runner_machine": {
                "hostname": self.hostname,
                "homeboy_version": self.homeboy_version,
                "homeboy_build": self.homeboy_build,
            },
            "wall_ms": self.wall_ms,
            "watched_dir": self.watched_dir,
            "artifact_dir_growth_bytes": Self::opt_i64(self.watched_dir_growth_bytes()),
            "stale_child_process_count": Self::opt_u64(self.stale_child_process_count()),
            "process_rss_bytes": {
                "before": Self::opt_u64(self.before.process_rss_bytes),
                "after": Self::opt_u64(self.after.process_rss_bytes),
            },
            "host_process_count": {
                "before": Self::opt_u64(self.before.host_process_count),
                "after": Self::opt_u64(self.after.host_process_count),
            },
            "child_process_count": {
                "before": Self::opt_u64(self.before.child_process_count),
                "after": Self::opt_u64(self.after.child_process_count),
            },
            "load_average_1m": {
                "before": Self::opt_f64(self.before.load_average_1m),
                "after": Self::opt_f64(self.after.load_average_1m),
            },
        })
    }
}

/// Recursive byte size of a directory tree. Best-effort: returns `None` if the
/// path does not exist or cannot be read; silently skips unreadable entries
/// rather than aborting the whole walk (a benchmark must not fail because one
/// transient temp file vanished mid-measure).
fn dir_size_bytes(root: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(root).ok()?;
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if !metadata.is_dir() {
        // Symlink or special file at the root: count its own size only, do not
        // follow it (avoids cycles / measuring outside the tree).
        return Some(metadata.len());
    }

    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Some(total)
}

/// Controller hostname, best-effort. Reads the `HOSTNAME` env var first, then
/// falls back to `/proc/sys/kernel/hostname` on Linux. `None` when neither is
/// available (no new dependency for hostname resolution).
fn hostname() -> Option<String> {
    if let Ok(value) = std::env::var("HOSTNAME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let value = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resident set size of the controller process in bytes, from
/// `/proc/self/statm` (Linux). `None` on non-Linux / unreadable hosts.
fn self_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    // Fields are in pages: size, resident, shared, ... — we want `resident`.
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = page_size_bytes();
    Some(resident_pages.saturating_mul(page_size))
}

/// Host page size in bytes. Defaults to the common 4 KiB when not resolvable;
/// RSS is a coarse capacity signal so an exact page size is not required.
fn page_size_bytes() -> u64 {
    // No libc dependency available for sysconf; 4 KiB is the near-universal
    // Linux page size and good enough for a coarse capacity signal.
    4096
}

/// Total number of processes on the host, counted from numeric `/proc` entries
/// (Linux). `None` on non-Linux / unreadable hosts.
fn host_process_count() -> Option<u64> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let count = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.chars().all(|ch| ch.is_ascii_digit()))
                .unwrap_or(false)
        })
        .count();
    u64::try_from(count).ok()
}

/// Best-effort count of direct child processes of the controller, by scanning
/// `/proc/[pid]/stat` for entries whose parent pid is our pid (Linux). `None`
/// on non-Linux / unreadable hosts.
fn child_process_count() -> Option<u64> {
    let my_pid = std::process::id();
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut count: u64 = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let stat_path = entry.path().join("stat");
        let Ok(stat) = std::fs::read_to_string(&stat_path) else {
            continue;
        };
        if let Some(ppid) = parse_ppid_from_proc_stat(&stat) {
            if ppid == my_pid {
                count = count.saturating_add(1);
            }
        }
    }
    Some(count)
}

/// Parse the ppid (4th field) out of a `/proc/[pid]/stat` line. The 2nd field
/// (comm) is parenthesized and may contain spaces, so split on the final `)`
/// before reading the space-separated state/ppid fields that follow.
fn parse_ppid_from_proc_stat(stat: &str) -> Option<u32> {
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?;
    // After `)` the fields are: state ppid pgrp ...
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// 1-minute load average from `/proc/loadavg` (Linux). `None` on non-Linux /
/// unreadable hosts.
fn load_average_1m() -> Option<f64> {
    let loadavg = std::fs::read_to_string("/proc/loadavg").ok()?;
    loadavg.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_dir_byte_growth_delta_across_the_run() {
        let dir = std::env::temp_dir().join(format!(
            "homeboy-host-telemetry-test-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Opening snapshot on an empty dir.
        let capture = LabHostTelemetryCapture::start(&dir);

        // Simulate the run writing an artifact into the watched dir.
        let payload = vec![0u8; 4096];
        std::fs::write(dir.join("artifact.bin"), &payload).unwrap();

        let telemetry = capture.finish();
        let metadata = telemetry.to_metadata();

        // The dir grew by at least the artifact we wrote.
        let growth = telemetry
            .watched_dir_growth_bytes()
            .expect("growth measured");
        assert!(growth >= 4096, "expected >= 4096 byte growth, got {growth}");
        assert_eq!(
            metadata["artifact_dir_growth_bytes"],
            serde_json::json!(growth)
        );
        assert_eq!(
            metadata["schema"],
            serde_json::json!("homeboy/lab-host-telemetry/v1")
        );
        // Runner machine identity always carries the homeboy version.
        assert!(metadata["runner_machine"]["homeboy_version"].is_string());
        assert!(metadata["watched_dir"].as_str().is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn degrades_gracefully_when_metrics_are_unavailable() {
        // A watched dir that does not exist makes dir size unmeasurable, and we
        // synthesize snapshots with all metrics absent to prove the serializer
        // marks them `not measured` rather than fabricating a zero.
        let telemetry = LabHostTelemetry {
            hostname: None,
            homeboy_version: "0.0.0".to_string(),
            homeboy_build: "homeboy 0.0.0".to_string(),
            watched_dir: "/nonexistent/path".to_string(),
            wall_ms: 0,
            before: HostResourceSnapshot::default(),
            after: HostResourceSnapshot::default(),
        };

        // Growth and stale-child are unmeasurable when endpoints are absent.
        assert!(telemetry.watched_dir_growth_bytes().is_none());
        assert!(telemetry.stale_child_process_count().is_none());

        let metadata = telemetry.to_metadata();
        assert_eq!(
            metadata["artifact_dir_growth_bytes"],
            serde_json::json!({ "available": false, "reason": NOT_MEASURED })
        );
        assert_eq!(
            metadata["stale_child_process_count"],
            serde_json::json!({ "available": false, "reason": NOT_MEASURED })
        );
        assert_eq!(
            metadata["process_rss_bytes"]["before"],
            serde_json::json!({ "available": false, "reason": NOT_MEASURED })
        );
        assert_eq!(
            metadata["load_average_1m"]["after"],
            serde_json::json!({ "available": false, "reason": NOT_MEASURED })
        );
    }

    #[test]
    fn dir_size_counts_nested_files() {
        let dir = std::env::temp_dir().join(format!(
            "homeboy-host-telemetry-nested-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let nested = dir.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("top.bin"), vec![1u8; 100]).unwrap();
        std::fs::write(nested.join("deep.bin"), vec![2u8; 200]).unwrap();

        let size = dir_size_bytes(&dir).expect("size measured");
        assert!(size >= 300, "expected >= 300 bytes, got {size}");

        // A nonexistent path is unmeasurable, not zero.
        assert!(dir_size_bytes(Path::new("/nonexistent/homeboy/telemetry")).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_ppid_from_proc_stat_line_with_spaced_comm() {
        // comm field contains a space and parens, ppid is the 4th overall field.
        let line = "1234 (weird (comm) name) S 4321 1234 1234 0 -1 ...";
        assert_eq!(parse_ppid_from_proc_stat(line), Some(4321));
    }
}
