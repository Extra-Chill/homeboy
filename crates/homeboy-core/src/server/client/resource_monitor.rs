use homeboy_engine_primitives::process_table;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::engine::resource::{
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
            peak: crate::engine::resource::ChildResourcePeakSample {
                sampled_peak_rss_bytes: state.peak_rss_bytes,
                sampled_peak_cpu_percent: state.peak_cpu_percent,
            },
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
    let snapshot = process_table::snapshot(false)
        .map_err(|_| "extension_child_probe_unsupported".to_string())?;
    let rows: Vec<_> = snapshot
        .iter()
        .map(|row| ExtensionChildProcessSample {
            pid: row.pid,
            parent_pid: row.parent_pid,
            rss_bytes: row.rss_kib.unwrap_or(0).saturating_mul(1024),
            cpu_percent: row.cpu_percent.unwrap_or(0.0),
            command: row.executable.clone(),
        })
        .collect();
    let Some(root) = rows.iter().find(|row| row.pid == root_pid) else {
        return Ok(None);
    };
    let mut selected = vec![root.clone()];
    for (pid, _) in
        process_table::descendants(root_pid, rows.iter().map(|row| (row.pid, row.parent_pid)))
    {
        if let Some(row) = rows.iter().find(|row| row.pid == pid) {
            selected.push(row.clone());
        }
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
