use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::core::engine::resource::{
    ChildProcessIdentity, ExtensionChildProcessSample, ExtensionChildResourceSample,
    ExtensionChildResourceSummary,
};

pub(crate) struct ChildResourceMonitor {
    child: ChildProcessIdentity,
    started_at: String,
    started_instant: Instant,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<ChildProbeState>>,
}

#[derive(Default)]
struct ChildProbeState {
    peak_rss_bytes: Option<u64>,
    peak_cpu_percent: Option<f64>,
    peak_at_ms: Option<u128>,
    peak_child_count: Option<usize>,
    samples: Vec<ExtensionChildResourceSample>,
    warnings: Vec<String>,
}

impl ChildResourceMonitor {
    pub(crate) fn start(root_pid: u32, command_label: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle =
            std::thread::spawn(move || sample_child_until_stopped(root_pid, stop_for_thread));

        Self {
            child: ChildProcessIdentity {
                root_pid,
                command_label,
            },
            started_at: Utc::now().to_rfc3339(),
            started_instant: Instant::now(),
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn finish(mut self) -> ExtensionChildResourceSummary {
        self.stop.store(true, Ordering::Relaxed);
        let mut state = self
            .handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        state.warnings.sort();
        state.warnings.dedup();

        ExtensionChildResourceSummary {
            child: self.child,
            phase: None,
            started_at: self.started_at,
            finished_at: Utc::now().to_rfc3339(),
            duration_ms: self.started_instant.elapsed().as_millis(),
            sampled_peak_rss_bytes: state.peak_rss_bytes,
            sampled_peak_cpu_percent: state.peak_cpu_percent,
            sampled_peak_at_ms: state.peak_at_ms,
            sampled_peak_child_count: state.peak_child_count,
            samples: state.samples,
            warnings: state.warnings,
        }
    }
}

fn sample_child_until_stopped(root_pid: u32, stop: Arc<AtomicBool>) -> ChildProbeState {
    let mut state = ChildProbeState::default();
    let started = Instant::now();

    loop {
        match probe_child_resources(root_pid) {
            Ok(Some(mut sample)) => {
                sample.elapsed_ms = started.elapsed().as_millis();
                let rss_bytes = sample.rss_bytes;
                let cpu_percent = sample.cpu_percent;
                let child_count = sample.child_count;
                let peak_changed = state.peak_rss_bytes.is_none_or(|peak| rss_bytes > peak);
                state.peak_rss_bytes = Some(
                    state
                        .peak_rss_bytes
                        .map_or(rss_bytes, |peak| peak.max(rss_bytes)),
                );
                if peak_changed {
                    state.peak_at_ms = Some(sample.elapsed_ms);
                    state.peak_child_count = Some(child_count);
                }
                state.peak_cpu_percent = Some(
                    state
                        .peak_cpu_percent
                        .map_or(cpu_percent, |peak| peak.max(cpu_percent)),
                );
                state.samples.push(sample);
            }
            Ok(None) => {}
            Err(warning) => state.warnings.push(warning),
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    state
}

fn probe_child_resources(
    root_pid: u32,
) -> std::result::Result<Option<ExtensionChildResourceSample>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss=,%cpu=,comm="])
        .output()
        .map_err(|_| "extension_child_probe_unsupported".to_string())?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows = parse_process_rows(&stdout)?;
    let Some(root) = rows.iter().find(|row| row.pid == root_pid) else {
        return Ok(None);
    };
    let mut selected = vec![root.clone()];
    let mut index = 0;
    while index < selected.len() {
        let parent_pid = selected[index].pid;
        for row in rows.iter().filter(|row| row.parent_pid == parent_pid) {
            selected.push(row.clone());
        }
        index += 1;
    }

    selected.sort_by_key(|row| row.pid);
    let rss_bytes = selected.iter().map(|row| row.rss_bytes).sum::<u64>();
    let cpu_percent = selected.iter().map(|row| row.cpu_percent).sum::<f64>();

    Ok(Some(ExtensionChildResourceSample {
        elapsed_ms: 0,
        timestamp: Utc::now().to_rfc3339(),
        root_pid,
        phase: None,
        rss_bytes,
        cpu_percent,
        child_count: selected.len().saturating_sub(1),
        processes: selected,
    }))
}

fn parse_process_rows(
    stdout: &str,
) -> std::result::Result<Vec<ExtensionChildProcessSample>, String> {
    let mut rows = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            return Err("extension_child_pid_probe_unsupported".to_string());
        };
        let Some(parent_pid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            return Err("extension_child_ppid_probe_unsupported".to_string());
        };
        let Some(rss_kb) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
            return Err("extension_child_rss_probe_unsupported".to_string());
        };
        let Some(cpu_percent) = parts.next().and_then(|value| value.parse::<f64>().ok()) else {
            return Err("extension_child_cpu_probe_unsupported".to_string());
        };
        let command = parts.collect::<Vec<_>>().join(" ");
        rows.push(ExtensionChildProcessSample {
            pid,
            parent_pid,
            rss_bytes: rss_kb.saturating_mul(1024),
            cpu_percent,
            command,
        });
    }
    Ok(rows)
}
