use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::engine::command::{
    isolate_process_tree, wait_with_bounded_output, wait_with_bounded_output_until_cancelled,
    CommandCaptureMetadata, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use crate::core::error::{Error, Result};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceMetrics {
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    pub sample_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_process_count_peak: Option<u64>,
    pub source: String,
}

#[derive(Debug)]
pub(crate) struct MeasuredOutput {
    pub output: Output,
    pub capture: CommandCaptureMetadata,
    pub metrics: RunnerResourceMetrics,
}

#[derive(Debug, Default)]
struct MetricsState {
    sample_count: u64,
    peak_rss_bytes: u64,
    child_process_count_peak: u64,
    cpu_user_ms: u64,
    cpu_system_ms: u64,
}

pub(crate) fn measured_command_output(command: &mut Command) -> Result<MeasuredOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let child = command.spawn().map_err(|err| {
        Error::internal_io(err.to_string(), Some("execute runner command".to_string()))
    })?;
    let pid = child.id();
    let collector = ResourceMetricsCollector::start(pid);
    let bounded_output =
        wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES).map_err(|err| {
            Error::internal_io(err.to_string(), Some("wait for runner command".to_string()))
        })?;
    let capture = bounded_output.capture.clone();
    let output = bounded_output.into_output();
    let metrics = collector.finish(started.elapsed());
    Ok(MeasuredOutput {
        output,
        capture,
        metrics,
    })
}

pub(crate) fn measured_command_output_until_cancelled(
    command: &mut Command,
    is_cancelled: impl FnMut() -> bool,
) -> Result<MeasuredOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_tree(command);
    let started = Instant::now();
    let mut child = command.spawn().map_err(|err| {
        Error::internal_io(err.to_string(), Some("execute runner command".to_string()))
    })?;
    let pid = child.id();
    let collector = ResourceMetricsCollector::start(pid);
    let bounded_output = wait_with_bounded_output_until_cancelled(
        &mut child,
        DEFAULT_CAPTURE_LIMIT_BYTES,
        is_cancelled,
    )
    .map_err(|err| {
        Error::internal_io(err.to_string(), Some("wait for runner command".to_string()))
    })?;
    let capture = bounded_output.capture.clone();
    let output = bounded_output.into_output();
    let metrics = collector.finish(started.elapsed());
    Ok(MeasuredOutput {
        output,
        capture,
        metrics,
    })
}

struct ResourceMetricsCollector {
    supported: bool,
    stop: Option<mpsc::Sender<()>>,
    state: Arc<Mutex<MetricsState>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ResourceMetricsCollector {
    fn start(root_pid: u32) -> Self {
        let supported = cfg!(target_os = "linux") && std::path::Path::new("/proc").exists();
        let state = Arc::new(Mutex::new(MetricsState::default()));
        if !supported {
            return Self {
                supported,
                stop: None,
                state,
                handle: None,
            };
        }

        let (stop, stop_rx) = mpsc::channel();
        sample(root_pid, &state);
        let thread_state = Arc::clone(&state);
        let handle = thread::spawn(move || loop {
            sample(root_pid, &thread_state);
            if stop_rx.recv_timeout(SAMPLE_INTERVAL).is_ok() {
                break;
            }
        });

        Self {
            supported,
            stop: Some(stop),
            state,
            handle: Some(handle),
        }
    }

    fn finish(mut self, duration: Duration) -> RunnerResourceMetrics {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let state = self.state.lock().expect("resource metrics mutex poisoned");
        RunnerResourceMetrics {
            duration_ms: duration.as_millis().try_into().unwrap_or(u64::MAX),
            cpu_user_ms: (state.sample_count > 0).then_some(state.cpu_user_ms),
            cpu_system_ms: (state.sample_count > 0).then_some(state.cpu_system_ms),
            peak_rss_bytes: (state.sample_count > 0).then_some(state.peak_rss_bytes),
            sample_count: state.sample_count,
            child_process_count_peak: (state.sample_count > 0)
                .then_some(state.child_process_count_peak),
            source: if self.supported {
                "linux_procfs_process_tree".to_string()
            } else {
                "duration_only".to_string()
            },
        }
    }
}

#[cfg(target_os = "linux")]
fn sample(root_pid: u32, state: &Arc<Mutex<MetricsState>>) {
    if let Some(snapshot) = process_tree_snapshot(root_pid) {
        let mut state = state.lock().expect("resource metrics mutex poisoned");
        state.sample_count += 1;
        state.peak_rss_bytes = state.peak_rss_bytes.max(snapshot.rss_bytes);
        state.child_process_count_peak = state
            .child_process_count_peak
            .max(snapshot.process_count.saturating_sub(1));
        state.cpu_user_ms = state.cpu_user_ms.max(snapshot.cpu_user_ms);
        state.cpu_system_ms = state.cpu_system_ms.max(snapshot.cpu_system_ms);
    }
}

#[cfg(not(target_os = "linux"))]
fn sample(_root_pid: u32, _state: &Arc<Mutex<MetricsState>>) {}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcessSnapshot {
    process_count: u64,
    rss_bytes: u64,
    cpu_user_ms: u64,
    cpu_system_ms: u64,
}

#[cfg(target_os = "linux")]
fn process_tree_snapshot(root_pid: u32) -> Option<ProcessSnapshot> {
    let stats = read_process_stats();
    let root_pid = root_pid as i32;
    stats.get(&root_pid)?;

    let mut children: std::collections::HashMap<i32, Vec<i32>> = std::collections::HashMap::new();
    for stat in stats.values() {
        children.entry(stat.ppid).or_default().push(stat.pid);
    }

    let mut queue = std::collections::VecDeque::from([root_pid]);
    let mut seen = std::collections::HashSet::new();
    let mut rss_bytes = 0_u64;
    let mut user_ticks = 0_u64;
    let mut system_ticks = 0_u64;

    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(stat) = stats.get(&pid) {
            rss_bytes = rss_bytes.saturating_add(stat.rss_bytes);
            user_ticks = user_ticks.saturating_add(stat.user_ticks);
            system_ticks = system_ticks.saturating_add(stat.system_ticks);
            if let Some(child_pids) = children.get(&pid) {
                queue.extend(child_pids);
            }
        }
    }

    let clock_ticks = clock_ticks_per_second().max(1);
    Some(ProcessSnapshot {
        process_count: seen.len().try_into().unwrap_or(u64::MAX),
        rss_bytes,
        cpu_user_ms: user_ticks.saturating_mul(1000) / clock_ticks,
        cpu_system_ms: system_ticks.saturating_mul(1000) / clock_ticks,
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcessStat {
    pid: i32,
    ppid: i32,
    user_ticks: u64,
    system_ticks: u64,
    rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn read_process_stats() -> std::collections::HashMap<i32, ProcessStat> {
    let mut stats = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return stats;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<i32>() else {
            continue;
        };
        if let Some(stat) = read_process_stat(pid) {
            stats.insert(pid, stat);
        }
    }
    stats
}

#[cfg(target_os = "linux")]
fn read_process_stat(pid: i32) -> Option<ProcessStat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end_command = stat.rfind(')')?;
    let fields: Vec<&str> = stat[end_command + 2..].split_whitespace().collect();
    let ppid = fields.get(1)?.parse().ok()?;
    let user_ticks = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0)
        + fields.get(13).and_then(|v| v.parse().ok()).unwrap_or(0);
    let system_ticks = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0)
        + fields.get(14).and_then(|v| v.parse().ok()).unwrap_or(0);
    let rss_pages: u64 = fields.get(21).and_then(|v| v.parse().ok()).unwrap_or(0);
    Some(ProcessStat {
        pid,
        ppid,
        user_ticks,
        system_ticks,
        rss_bytes: rss_pages.saturating_mul(page_size_bytes()),
    })
}

#[cfg(target_os = "linux")]
fn clock_ticks_per_second() -> u64 {
    static CLOCK_TICKS_PER_SECOND: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

    *CLOCK_TICKS_PER_SECOND.get_or_init(|| command_u64("getconf", &["CLK_TCK"]).unwrap_or(100))
}

#[cfg(target_os = "linux")]
fn page_size_bytes() -> u64 {
    static PAGE_SIZE_BYTES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

    *PAGE_SIZE_BYTES.get_or_init(|| command_u64("getconf", &["PAGESIZE"]).unwrap_or(4096))
}

#[cfg(target_os = "linux")]
fn command_u64(command: &str, args: &[&str]) -> Option<u64> {
    let output = Command::new(command).args(args).output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}
