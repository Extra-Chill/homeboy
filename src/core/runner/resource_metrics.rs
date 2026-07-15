use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::engine::command::{
    isolate_process_tree, supports_process_tree_isolation, wait_with_bounded_output,
    wait_with_bounded_output_until_cancelled_with_stdout_observer,
    CommandCaptureMetadata, StdoutLineObserver, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use crate::core::error::{Error, Result};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(any(target_os = "linux", test))]
const FALLBACK_RSS_LIMIT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const PREFERRED_HOST_HEADROOM_BYTES: u64 = 4 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const HOST_HEADROOM_DIVISOR: u64 = 10;
#[cfg(target_os = "linux")]
const DEFAULT_PROCESS_COUNT_LIMIT: u64 = 128;
#[cfg(target_os = "linux")]
const RSS_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES";
#[cfg(target_os = "linux")]
const PROCESS_COUNT_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT";

fn require_process_tree_isolation() -> Result<()> {
    if supports_process_tree_isolation() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "process-tree-isolation",
        "runner command execution requires process-tree isolation to persist child identity safely on this platform",
        None,
        None,
    ))
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_guard: Option<RunnerResourceGuardLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_violation: Option<RunnerResourceGuardViolation>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardLimits {
    pub rss_limit_bytes: u64,
    pub process_count_limit: u64,
    pub concurrency: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_capacity_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_headroom_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_budget_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_bytes: Option<u64>,
    pub rss_limit_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardViolation {
    pub reason: String,
    pub message: String,
    pub rss_bytes: u64,
    pub rss_limit_bytes: u64,
    pub process_count: u64,
    pub process_count_limit: u64,
}

#[derive(Debug)]
pub(crate) struct MeasuredOutput {
    pub output: Output,
    pub capture: CommandCaptureMetadata,
    pub metrics: RunnerResourceMetrics,
}

pub(crate) type RunnerCommandProgressSink =
    Arc<dyn Fn(Value) -> Result<()> + Send + Sync + 'static>;

#[derive(Debug, Default)]
struct MetricsState {
    sample_count: u64,
    peak_rss_bytes: u64,
    child_process_count_peak: u64,
    cpu_user_ms: u64,
    cpu_system_ms: u64,
    active_rss_bytes_peak: u64,
    aggregate_rss_bytes_peak: u64,
    effective_rss_limit_bytes_min: Option<u64>,
    guard_violation: Option<RunnerResourceGuardViolation>,
}

pub(crate) fn measured_command_output(
    command: &mut Command,
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let child = command.spawn().map_err(|err| {
        Error::internal_io(err.to_string(), Some("execute runner command".to_string()))
    })?;
    let pid = child.id();
    let collector = ResourceMetricsCollector::start(pid, started, None, None, concurrency_limit);
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

pub(crate) fn measured_command_output_until_cancelled_with_progress(
    command: &mut Command,
    mut is_cancelled: impl FnMut() -> bool,
    progress_sink: Option<RunnerCommandProgressSink>,
    require_child_identity_acknowledgement: bool,
    stdout_line_observer: Option<StdoutLineObserver>,
    child_started: Option<Arc<dyn Fn(u32) -> Result<()> + Send + Sync + 'static>>,
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    if require_child_identity_acknowledgement {
        require_process_tree_isolation()?;
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_tree(command);
    let started = Instant::now();
    let mut child = command.spawn().map_err(|err| {
        Error::internal_io(err.to_string(), Some("execute runner command".to_string()))
    })?;
    let pid = child.id();
    if let Some(child_started) = child_started {
        if let Err(error) = child_started(pid) {
            let cleanup = terminate_unpersisted_child_and_reap(&mut child);
            let cleanup_context = cleanup
                .err()
                .map(|cleanup_error| format!("; child cleanup also failed: {cleanup_error}"))
                .unwrap_or_default();
            return Err(Error::internal_io(
                format!(
                    "failed to persist spawned runner child identity: {error}{cleanup_context}"
                ),
                Some("persist runner child identity".to_string()),
            ));
        }
    }
    // The first PID event is an acknowledgement boundary: without durable
    // identity evidence, terminate and reap rather than leaving work running.
    if require_child_identity_acknowledgement {
        let progress = progress_sink.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "child-identity-acknowledgement",
                "durable runner execution requires a child identity acknowledgement sink",
                None,
                None,
            )
        })?;
        if let Err(error) = progress(runner_command_heartbeat_data(
            started.elapsed(),
            pid,
            process_tree_resource_summary(pid),
        )) {
            let cleanup = terminate_unpersisted_child_and_reap(&mut child);
            let cleanup_context = cleanup
                .err()
                .map(|error| format!("; child cleanup also failed: {error}"))
                .unwrap_or_default();
            return Err(Error::internal_io(
                format!(
                    "failed to persist spawned runner child identity: {error}{cleanup_context}"
                ),
                Some("persist runner child identity".to_string()),
            ));
        }
    }
    let guard_violation = Arc::new(Mutex::new(None));
    let collector = ResourceMetricsCollector::start(
        pid,
        started,
        progress_sink,
        Some(Arc::clone(&guard_violation)),
        concurrency_limit,
    );
    let bounded_output = wait_with_bounded_output_until_cancelled_with_stdout_observer(
        &mut child,
        DEFAULT_CAPTURE_LIMIT_BYTES,
        || {
            is_cancelled()
                || guard_violation
                    .lock()
                    .expect("resource guard mutex poisoned")
                    .is_some()
        },
        stdout_line_observer,
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

/// Terminate an unrecorded child with every available ownership boundary before
/// reaping it. The root fallback keeps cleanup safe where group/tree control is
/// unavailable, while the caller preserves the original persistence failure.
fn terminate_unpersisted_child_and_reap(child: &mut std::process::Child) -> Result<()> {
    let pid = child.id();
    let group = crate::core::process::terminate_isolated_process_group(pid);
    let tree = crate::core::process::terminate_process_tree_best_effort(pid);
    if group.is_err() || tree.is_err() {
        let _ = child.kill();
    }
    child
        .wait()
        .map_err(|error| Error::internal_io(error.to_string(), Some("reap runner child".to_string())))?;
    tree.or(group)
}

struct ResourceMetricsCollector {
    supported: bool,
    stop: Option<mpsc::Sender<()>>,
    state: Arc<Mutex<MetricsState>>,
    handle: Option<thread::JoinHandle<()>>,
    guard_limits: Option<RunnerResourceGuardLimits>,
    #[cfg(target_os = "linux")]
    registered_root_pid: Option<u32>,
}

impl ResourceMetricsCollector {
    fn start(
        root_pid: u32,
        started: Instant,
        progress_sink: Option<RunnerCommandProgressSink>,
        guard_violation: Option<Arc<Mutex<Option<RunnerResourceGuardViolation>>>>,
        concurrency_limit: Option<usize>,
    ) -> Self {
        let supported = cfg!(target_os = "linux") && std::path::Path::new("/proc").exists();
        let guard_limits = resolved_resource_guard_limits(concurrency_limit);
        let state = Arc::new(Mutex::new(MetricsState::default()));
        if !supported && progress_sink.is_none() {
            return Self {
                supported,
                stop: None,
                state,
                handle: None,
                guard_limits,
                #[cfg(target_os = "linux")]
                registered_root_pid: None,
            };
        }

        #[cfg(target_os = "linux")]
        register_active_runner(root_pid);

        let (stop, stop_rx) = mpsc::channel();
        sample(
            root_pid,
            &state,
            guard_violation.as_ref(),
            guard_limits.as_ref(),
        );
        let thread_state = Arc::clone(&state);
        let thread_guard_limits = guard_limits.clone();
        let mut last_heartbeat = Instant::now();
        let handle = thread::spawn(move || loop {
            sample(
                root_pid,
                &thread_state,
                guard_violation.as_ref(),
                thread_guard_limits.as_ref(),
            );
            if let Some(progress) = progress_sink.as_ref() {
                if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                    let _ = progress(runner_command_heartbeat_data(
                        started.elapsed(),
                        root_pid,
                        process_tree_resource_summary(root_pid),
                    ));
                    last_heartbeat = Instant::now();
                }
            }
            if stop_rx.recv_timeout(SAMPLE_INTERVAL).is_ok() {
                break;
            }
        });

        Self {
            supported,
            stop: Some(stop),
            state,
            handle: Some(handle),
            guard_limits,
            #[cfg(target_os = "linux")]
            registered_root_pid: Some(root_pid),
        }
    }

    fn finish(mut self, duration: Duration) -> RunnerResourceMetrics {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        #[cfg(target_os = "linux")]
        if let Some(root_pid) = self.registered_root_pid.take() {
            unregister_active_runner(root_pid);
        }
        let state = self.state.lock().expect("resource metrics mutex poisoned");
        let mut guard_limits = self.guard_limits;
        if let Some(limits) = guard_limits.as_mut() {
            if limits.aggregate_rss_budget_bytes.is_some() {
                limits.active_rss_bytes = Some(state.active_rss_bytes_peak);
                limits.aggregate_rss_bytes = Some(state.aggregate_rss_bytes_peak);
                if let Some(effective_limit) = state.effective_rss_limit_bytes_min {
                    limits.rss_limit_bytes = effective_limit;
                }
            }
        }
        RunnerResourceMetrics {
            duration_ms: duration.as_millis().try_into().unwrap_or(u64::MAX),
            cpu_user_ms: (state.sample_count > 0).then_some(state.cpu_user_ms),
            cpu_system_ms: (state.sample_count > 0).then_some(state.cpu_system_ms),
            peak_rss_bytes: (state.sample_count > 0).then_some(state.peak_rss_bytes),
            sample_count: state.sample_count,
            child_process_count_peak: (state.sample_count > 0)
                .then_some(state.child_process_count_peak),
            resource_guard: guard_limits,
            guard_violation: state.guard_violation.clone(),
            source: if self.supported {
                "linux_procfs_process_tree".to_string()
            } else {
                "duration_only".to_string()
            },
        }
    }
}

pub(crate) fn runner_command_heartbeat_data(
    elapsed: Duration,
    root_pid: u32,
    resources: Option<Value>,
) -> Value {
    json!({
        "phase": "heartbeat",
        "elapsed_ms": elapsed.as_millis(),
        "process": {
            "root_pid": root_pid,
            "resources": resources,
        },
    })
}

#[cfg(target_os = "linux")]
fn process_tree_resource_summary(root_pid: u32) -> Option<Value> {
    let snapshot = process_tree_snapshot(root_pid)?;
    Some(json!({
        "source": "linux_procfs_process_tree",
        "process_count": snapshot.process_count,
        "child_process_count": snapshot.process_count.saturating_sub(1),
        "rss_bytes": snapshot.rss_bytes,
        "cpu_user_ms": snapshot.cpu_user_ms,
        "cpu_system_ms": snapshot.cpu_system_ms,
    }))
}

#[cfg(not(target_os = "linux"))]
fn process_tree_resource_summary(_root_pid: u32) -> Option<Value> {
    None
}

#[cfg(target_os = "linux")]
fn sample(
    root_pid: u32,
    state: &Arc<Mutex<MetricsState>>,
    guard_violation: Option<&Arc<Mutex<Option<RunnerResourceGuardViolation>>>>,
    guard_limits: Option<&RunnerResourceGuardLimits>,
) {
    if let Some(snapshot) = process_tree_snapshot(root_pid) {
        let active_rss = update_active_runner_rss(root_pid, snapshot.rss_bytes);
        let mut state = state.lock().expect("resource metrics mutex poisoned");
        state.sample_count += 1;
        state.peak_rss_bytes = state.peak_rss_bytes.max(snapshot.rss_bytes);
        state.child_process_count_peak = state
            .child_process_count_peak
            .max(snapshot.process_count.saturating_sub(1));
        state.cpu_user_ms = state.cpu_user_ms.max(snapshot.cpu_user_ms);
        state.cpu_system_ms = state.cpu_system_ms.max(snapshot.cpu_system_ms);
        state.active_rss_bytes_peak = state.active_rss_bytes_peak.max(active_rss.other_rss_bytes);
        state.aggregate_rss_bytes_peak = state
            .aggregate_rss_bytes_peak
            .max(active_rss.aggregate_rss_bytes);
        let effective_rss_limit_bytes =
            guard_limits.map(|limits| effective_rss_limit(limits, active_rss.other_rss_bytes));
        if let Some(limit) = effective_rss_limit_bytes {
            state.effective_rss_limit_bytes_min = Some(
                state
                    .effective_rss_limit_bytes_min
                    .map_or(limit, |current| current.min(limit)),
            );
        }
        if state.guard_violation.is_none() {
            if let Some(violation) =
                resource_guard_violation(&snapshot, guard_limits, effective_rss_limit_bytes)
            {
                if let Some(shared_violation) = guard_violation {
                    *shared_violation
                        .lock()
                        .expect("resource guard mutex poisoned") = Some(violation.clone());
                }
                state.guard_violation = Some(violation);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn sample(
    _root_pid: u32,
    _state: &Arc<Mutex<MetricsState>>,
    _guard_violation: Option<&Arc<Mutex<Option<RunnerResourceGuardViolation>>>>,
    _guard_limits: Option<&RunnerResourceGuardLimits>,
) {
}

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
fn resource_guard_violation(
    snapshot: &ProcessSnapshot,
    limits: Option<&RunnerResourceGuardLimits>,
    effective_rss_limit_bytes: Option<u64>,
) -> Option<RunnerResourceGuardViolation> {
    let limits = limits?;
    classify_resource_guard_violation(
        snapshot.rss_bytes,
        snapshot.process_count,
        effective_rss_limit_bytes.unwrap_or(limits.rss_limit_bytes),
        limits.process_count_limit,
    )
}

#[cfg(target_os = "linux")]
fn resolved_resource_guard_limits(
    concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    let concurrency = u64::try_from(concurrency_limit.unwrap_or(1).max(1)).unwrap_or(u64::MAX);
    let memory_capacity_bytes =
        crate::core::resources::memory::probe_system_memory().map(|memory| memory.total_bytes);
    let explicit_rss_limit = std::env::var(RSS_LIMIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let rss_limit = resolved_rss_limit(memory_capacity_bytes, explicit_rss_limit);
    Some(RunnerResourceGuardLimits {
        rss_limit_bytes: rss_limit.rss_limit_bytes,
        process_count_limit: resource_guard_limit(
            PROCESS_COUNT_LIMIT_ENV,
            DEFAULT_PROCESS_COUNT_LIMIT,
        ),
        concurrency,
        memory_capacity_bytes,
        host_headroom_bytes: rss_limit.host_headroom_bytes,
        aggregate_rss_budget_bytes: rss_limit.aggregate_rss_budget_bytes,
        active_rss_bytes: None,
        aggregate_rss_bytes: None,
        rss_limit_source: rss_limit.source,
    })
}

#[cfg(any(target_os = "linux", test))]
struct ResolvedRssLimit {
    rss_limit_bytes: u64,
    host_headroom_bytes: Option<u64>,
    aggregate_rss_budget_bytes: Option<u64>,
    source: String,
}

#[cfg(any(target_os = "linux", test))]
fn resolved_rss_limit(
    memory_capacity_bytes: Option<u64>,
    explicit_rss_limit: Option<u64>,
) -> ResolvedRssLimit {
    if let Some(limit) = explicit_rss_limit {
        return ResolvedRssLimit {
            rss_limit_bytes: limit,
            host_headroom_bytes: None,
            aggregate_rss_budget_bytes: None,
            source: "explicit_override".to_string(),
        };
    }
    match memory_capacity_bytes {
        Some(capacity) => {
            // Small hosts cannot reserve the preferred 4 GiB without disabling
            // the guard, so reserve at most half of their capacity.
            let headroom = (capacity / HOST_HEADROOM_DIVISOR)
                .max(PREFERRED_HOST_HEADROOM_BYTES)
                .min(capacity / 2);
            let budget = capacity.saturating_sub(headroom);
            ResolvedRssLimit {
                rss_limit_bytes: budget,
                host_headroom_bytes: Some(headroom),
                aggregate_rss_budget_bytes: Some(budget),
                source: "active_load_aware".to_string(),
            }
        }
        None => ResolvedRssLimit {
            rss_limit_bytes: FALLBACK_RSS_LIMIT_BYTES,
            host_headroom_bytes: None,
            aggregate_rss_budget_bytes: None,
            source: "fallback".to_string(),
        },
    }
}

#[cfg(any(target_os = "linux", test))]
fn effective_rss_limit(limits: &RunnerResourceGuardLimits, active_rss_bytes: u64) -> u64 {
    limits
        .aggregate_rss_budget_bytes
        .map(|budget| budget.saturating_sub(active_rss_bytes).max(1))
        .unwrap_or(limits.rss_limit_bytes)
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct ActiveRunnerRss {
    other_rss_bytes: u64,
    aggregate_rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn active_runner_rss() -> &'static Mutex<std::collections::HashMap<u32, u64>> {
    static ACTIVE_RUNNER_RSS: std::sync::OnceLock<Mutex<std::collections::HashMap<u32, u64>>> =
        std::sync::OnceLock::new();
    ACTIVE_RUNNER_RSS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(target_os = "linux")]
fn register_active_runner(root_pid: u32) {
    active_runner_rss()
        .lock()
        .expect("active runner rss mutex poisoned")
        .insert(root_pid, 0);
}

#[cfg(target_os = "linux")]
fn unregister_active_runner(root_pid: u32) {
    active_runner_rss()
        .lock()
        .expect("active runner rss mutex poisoned")
        .remove(&root_pid);
}

#[cfg(target_os = "linux")]
fn update_active_runner_rss(root_pid: u32, rss_bytes: u64) -> ActiveRunnerRss {
    let mut runners = active_runner_rss()
        .lock()
        .expect("active runner rss mutex poisoned");
    runners.insert(root_pid, rss_bytes);
    let aggregate_rss_bytes: u64 = runners.values().copied().sum();
    ActiveRunnerRss {
        other_rss_bytes: aggregate_rss_bytes.saturating_sub(rss_bytes),
        aggregate_rss_bytes,
    }
}

#[cfg(not(target_os = "linux"))]
fn resolved_resource_guard_limits(
    _concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn classify_resource_guard_violation(
    rss_bytes: u64,
    process_count: u64,
    rss_limit_bytes: u64,
    process_count_limit: u64,
) -> Option<RunnerResourceGuardViolation> {
    let reason = if rss_limit_bytes > 0 && rss_bytes >= rss_limit_bytes {
        "rss_limit_exceeded"
    } else if process_count_limit > 0 && process_count >= process_count_limit {
        "process_count_limit_exceeded"
    } else {
        return None;
    };
    Some(RunnerResourceGuardViolation {
        reason: reason.to_string(),
        message: format!(
            "runner job resource guard stopped process tree after rss_bytes={rss_bytes}, process_count={process_count}; limits rss_bytes={rss_limit_bytes}, process_count={process_count_limit}"
        ),
        rss_bytes,
        rss_limit_bytes,
        process_count,
        process_count_limit,
    })
}

#[cfg(target_os = "linux")]
fn resource_guard_limit(env_name: &str, default_value: u64) -> u64 {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_value)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn heartbeat_payload_includes_elapsed_pid_and_optional_resources() {
        let payload = runner_command_heartbeat_data(
            Duration::from_millis(42_000),
            1234,
            Some(json!({
                "source": "fixture",
                "process_count": 3,
                "child_process_count": 2,
            })),
        );

        assert_eq!(payload["phase"], "heartbeat");
        assert_eq!(payload["elapsed_ms"], 42_000);
        assert_eq!(payload["process"]["root_pid"], 1234);
        assert_eq!(payload["process"]["resources"]["process_count"], 3);
        assert_eq!(payload["process"]["resources"]["child_process_count"], 2);
    }

    #[cfg(unix)]
    #[test]
    fn child_started_failure_kills_and_reaps_the_spawned_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let pid = Arc::new(AtomicU32::new(0));
        let callback_pid = Arc::clone(&pid);
        let error = measured_command_output_until_cancelled_with_progress(
            &mut command,
            || false,
            None,
            false,
            None,
            Some(Arc::new(move |child_pid| {
                callback_pid.store(child_pid, Ordering::SeqCst);
                Err(Error::internal_unexpected("persist child identity"))
            })),
            None,
        )
        .expect_err("callback failure returns");

        assert!(error.message.contains("persist child identity"));
        assert!(!crate::core::process::pid_is_running(
            pid.load(Ordering::SeqCst)
        ));
    }

    #[test]
    fn heartbeat_payload_allows_missing_resource_summary() {
        let payload = runner_command_heartbeat_data(Duration::from_millis(5), 9, None);

        assert_eq!(payload["phase"], "heartbeat");
        assert_eq!(payload["elapsed_ms"], 5);
        assert_eq!(payload["process"]["root_pid"], 9);
        assert!(payload["process"]["resources"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn command_progress_persists_child_identity_immediately_after_spawn() {
        assert!(supports_process_tree_isolation());
        let progress = Arc::new(Mutex::new(Vec::new()));
        let progress_sink = {
            let progress = Arc::clone(&progress);
            Arc::new(move |data: Value| {
                progress.lock().expect("progress lock").push(data);
                Ok(())
            })
        };
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        measured_command_output_until_cancelled_with_progress(
            &mut command,
            || false,
            Some(progress_sink),
            true,
            None,
            None,
            None,
        )
        .expect("command completes");

        let progress = progress.lock().expect("progress lock");
        assert!(progress.iter().any(|data| {
            data["phase"] == "heartbeat"
                && data["process"]["root_pid"]
                    .as_u64()
                    .is_some_and(|pid| pid > 0)
        }));
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_process_tree_isolation_refuses_before_spawn() {
        let mut command = Command::new("this-command-must-not-be-spawned");
        let error = measured_command_output_until_cancelled_with_progress(
            &mut command,
            || false,
            None,
            true,
            None,
            None,
            None,
        )
        .expect_err("unsupported platforms must fail before spawning a child");

        assert!(error.message.contains("process-tree isolation"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_initial_child_identity_persistence_terminates_and_reaps_the_process_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("should-not-exist");
        let spawned_pid = Arc::new(Mutex::new(None));
        let progress_sink = {
            let spawned_pid = Arc::clone(&spawned_pid);
            Arc::new(move |data: Value| {
                *spawned_pid.lock().expect("pid lock") = data["process"]["root_pid"].as_u64();
                Err(Error::internal_io(
                    "durable progress unavailable",
                    Some("test progress persistence".to_string()),
                ))
            })
        };
        let mut command = Command::new("sh");
        command.args(["-c", &format!("sleep 1; touch {}", marker.display())]);

        let error = measured_command_output_until_cancelled_with_progress(
            &mut command,
            || false,
            Some(progress_sink),
            true,
            None,
            None,
            None,
        )
        .expect_err("initial identity persistence failure must fail execution");

        assert!(!error.message.is_empty(), "persistence failure is surfaced");
        let pid = spawned_pid
            .lock()
            .expect("pid lock")
            .expect("initial callback received PID") as u32;
        assert!(!crate::core::process::pid_is_running(pid));
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "child process tree must not continue running"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_measured_command_execution_remains_available() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        let output = measured_command_output(&mut command, None)
            .expect("ordinary measured command execution remains available");

        assert!(output.output.status.success());
    }

    #[test]
    fn resource_guard_classifies_rss_limit_exceeded() {
        let violation = classify_resource_guard_violation(17, 3, 16, 128).expect("rss violation");

        assert_eq!(violation.reason, "rss_limit_exceeded");
        assert_eq!(violation.rss_bytes, 17);
        assert_eq!(violation.rss_limit_bytes, 16);
        assert_eq!(violation.process_count, 3);
    }

    #[test]
    fn resource_guard_classifies_process_count_limit_exceeded() {
        let violation =
            classify_resource_guard_violation(8, 129, 16, 128).expect("process count violation");

        assert_eq!(violation.reason, "process_count_limit_exceeded");
        assert_eq!(violation.process_count, 129);
        assert_eq!(violation.process_count_limit, 128);
    }

    #[test]
    fn resource_guard_allows_zero_limits_to_disable_guard() {
        assert!(classify_resource_guard_violation(u64::MAX, u64::MAX, 0, 0).is_none());
    }

    #[test]
    fn resource_guard_allows_idle_capacity_borrowing() {
        let lab_capacity = 89 * 1024 * 1024 * 1024 + 7 * 1024 * 1024 * 1024 / 10;
        let limits = resolved_rss_limit(Some(lab_capacity), None);
        let trusted_cook_rss = 20 * 1024 * 1024 * 1024;

        assert_eq!(limits.rss_limit_bytes, lab_capacity - lab_capacity / 10);
        assert!(limits.rss_limit_bytes > 19 * 1024 * 1024 * 1024);
        assert!(classify_resource_guard_violation(
            trusted_cook_rss,
            1,
            limits.rss_limit_bytes,
            128,
        )
        .is_none());
        assert_eq!(limits.source, "active_load_aware");
    }

    #[test]
    fn resource_guard_protects_the_aggregate_budget() {
        let resolved = resolved_rss_limit(Some(96 * 1024 * 1024 * 1024), None);
        let limits = RunnerResourceGuardLimits {
            rss_limit_bytes: resolved.rss_limit_bytes,
            process_count_limit: 128,
            concurrency: 8,
            memory_capacity_bytes: Some(96 * 1024 * 1024 * 1024),
            host_headroom_bytes: resolved.host_headroom_bytes,
            aggregate_rss_budget_bytes: resolved.aggregate_rss_budget_bytes,
            active_rss_bytes: None,
            aggregate_rss_bytes: None,
            rss_limit_source: resolved.source,
        };

        let effective_limit = effective_rss_limit(&limits, 70 * 1024 * 1024 * 1024);
        assert_eq!(
            effective_limit,
            limits.aggregate_rss_budget_bytes.unwrap() - 70 * 1024 * 1024 * 1024
        );
        let violation = classify_resource_guard_violation(
            20 * 1024 * 1024 * 1024,
            1,
            effective_limit,
            limits.process_count_limit,
        )
        .expect("aggregate pressure violation");
        assert_eq!(violation.reason, "rss_limit_exceeded");
    }

    #[test]
    fn resource_guard_keeps_a_limit_when_capacity_is_smaller_than_headroom() {
        let limits = resolved_rss_limit(Some(2 * 1024 * 1024 * 1024), None);

        assert_eq!(limits.rss_limit_bytes, 1024 * 1024 * 1024);
        assert_eq!(limits.source, "active_load_aware");
    }

    #[test]
    fn resource_guard_uses_fallback_when_memory_capacity_is_unavailable() {
        let limits = resolved_rss_limit(None, None);

        assert_eq!(limits.rss_limit_bytes, FALLBACK_RSS_LIMIT_BYTES);
        assert_eq!(limits.source, "fallback");
    }

    #[test]
    fn resource_guard_explicit_rss_limit_takes_precedence_over_active_load() {
        let limits =
            resolved_rss_limit(Some(96 * 1024 * 1024 * 1024), Some(7 * 1024 * 1024 * 1024));

        assert_eq!(limits.rss_limit_bytes, 7 * 1024 * 1024 * 1024);
        assert_eq!(limits.source, "explicit_override");
        assert_eq!(limits.aggregate_rss_budget_bytes, None);
    }
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
