use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use homeboy_core::engine::command::{
    isolate_process_tree, supports_process_tree_isolation, terminate_process_tree_and_reap,
    wait_with_bounded_output, wait_with_bounded_output_until_cancelled_with_stdout_observer,
    CommandCaptureMetadata, StdoutLineObserver, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::redaction::RedactionPolicy;

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const ENVIRONMENT_DIAGNOSTIC_LIMIT: usize = 8;
#[cfg(any(target_os = "linux", test))]
const FALLBACK_RSS_LIMIT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const PREFERRED_HOST_HEADROOM_BYTES: u64 = 4 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const HOST_HEADROOM_DIVISOR: u64 = 10;
#[cfg(any(target_os = "linux", test))]
const DEFAULT_PROCESS_COUNT_LIMIT: u64 = 128;
#[cfg(any(target_os = "linux", test))]
const RSS_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES";
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT";
#[cfg(any(target_os = "linux", test))]
const DEFAULT_PROCESS_COUNT_LIMIT_CEILING: u64 = 256;
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_LIMIT_CEILING_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT";

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

// Resource-metrics data types now live in homeboy-runner-contract (pure serde,
// embedded in core api_jobs records) so core has no core -> runner edge.
// Re-exported so runner-internal call sites resolve unchanged.
pub use homeboy_lab_runner_contract::{
    RunnerResourceGuardLimits, RunnerResourceGuardViolation, RunnerResourceMetrics,
};

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
    resource_guard_env: &HashMap<String, String>,
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let child = command
        .spawn()
        .map_err(|error| runner_command_spawn_error(command, &error))?;
    let pid = child.id();
    let collector = ResourceMetricsCollector::start(
        pid,
        started,
        None,
        None,
        resource_guard_env,
        concurrency_limit,
    );
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
    resource_guard_env: &HashMap<String, String>,
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    if require_child_identity_acknowledgement {
        require_process_tree_isolation()?;
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_tree(command);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|error| runner_command_spawn_error(command, &error))?;
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
        resource_guard_env,
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

/// Build durable, secret-safe evidence for a failed pre-spawn command handoff.
///
/// `Command::spawn` is the last boundary before a child PID exists, so callers
/// need its OS details and command sizing rather than a generic IO error.
fn runner_command_spawn_error(command: &Command, error: &std::io::Error) -> Error {
    let policy = RedactionPolicy::default();
    let executable = os_str_bytes(command.get_program());
    let arguments = command.get_args().collect::<Vec<_>>();
    let argv_count = arguments.len() + 1;
    let argv_bytes = executable
        + arguments
            .iter()
            .map(|argument| os_str_bytes(argument))
            .sum::<usize>();
    let max_argument_bytes = arguments
        .iter()
        .map(|argument| os_str_bytes(argument))
        .chain(std::iter::once(executable))
        .max()
        .unwrap_or_default();
    let mut environment_variables = command
        .get_envs()
        .map(|(key, value)| {
            let key_bytes = os_str_bytes(key);
            let value_bytes = value.map(os_str_bytes).unwrap_or_default();
            let entry_bytes = key_bytes + 1 + value_bytes;
            json!({
                "name": key.to_string_lossy(),
                "key_bytes": key_bytes,
                "value_bytes": value_bytes,
                "entry_bytes": entry_bytes,
            })
        })
        .collect::<Vec<_>>();
    environment_variables.sort_by(|left, right| {
        right["entry_bytes"]
            .as_u64()
            .cmp(&left["entry_bytes"].as_u64())
            .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
    });
    let environment_count = environment_variables.len();
    let environment_bytes = environment_variables
        .iter()
        .filter_map(|entry| entry["entry_bytes"].as_u64())
        .sum::<u64>();
    let largest_environment_variable = environment_variables.first().cloned();
    environment_variables.truncate(ENVIRONMENT_DIAGNOSTIC_LIMIT);
    let raw_os_error = error.raw_os_error();

    Error::new(
        homeboy_core::error::ErrorCode::InternalIoError,
        "Runner command spawn failed",
        json!({
            "operation": "runner_command_spawn",
            "classification": if raw_os_error == Some(libc::E2BIG) {
                "argv_environment_too_large"
            } else {
                "spawn_failed"
            },
            "io_error": error.to_string(),
            "io_error_kind": format!("{:?}", error.kind()),
            "raw_os_error": raw_os_error,
            "executable": policy.redact_string(&command.get_program().to_string_lossy()),
            "cwd": command
                .get_current_dir()
                .map(|cwd| policy.redact_string(&cwd.to_string_lossy())),
            "argv_count": argv_count,
            "argv_bytes": argv_bytes,
            "max_argument_bytes": max_argument_bytes,
            "environment_count": environment_count,
            "environment_bytes": environment_bytes,
            "largest_environment_variable": largest_environment_variable,
            "environment_variables": environment_variables,
        }),
    )
}

fn os_str_bytes(value: &OsStr) -> usize {
    value.as_encoded_bytes().len()
}

/// Terminate and reap an unrecorded child through the same bounded, verified
/// process-tree lifecycle used by cancellation.
fn terminate_unpersisted_child_and_reap(child: &mut std::process::Child) -> Result<()> {
    terminate_process_tree_and_reap(child)
        .map(|_| ())
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("reap runner child".to_string()))
        })
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
        resource_guard_env: &HashMap<String, String>,
        concurrency_limit: Option<usize>,
    ) -> Self {
        let supported = cfg!(target_os = "linux") && std::path::Path::new("/proc").exists();
        let guard_limits = resolved_resource_guard_limits(resource_guard_env, concurrency_limit);
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
    resource_guard_env: &HashMap<String, String>,
    concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    let concurrency = u64::try_from(concurrency_limit.unwrap_or(1).max(1)).unwrap_or(u64::MAX);
    let memory_capacity_bytes =
        homeboy_core::resources::memory::probe_system_memory().map(|memory| memory.total_bytes);
    let explicit_rss_limit = std::env::var(RSS_LIMIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let rss_limit = resolved_rss_limit(memory_capacity_bytes, explicit_rss_limit);
    let process_count = resolved_process_count_limit(resource_guard_env);
    Some(RunnerResourceGuardLimits {
        rss_limit_bytes: rss_limit.rss_limit_bytes,
        process_count_limit: process_count.limit,
        process_count_limit_source: Some(process_count.source),
        requested_process_count_limit: process_count.requested,
        process_count_limit_ceiling: Some(process_count.ceiling),
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
    _resource_guard_env: &HashMap<String, String>,
    _concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    None
}

#[cfg(any(target_os = "linux", test))]
struct ResolvedProcessCountLimit {
    limit: u64,
    requested: Option<u64>,
    ceiling: u64,
    source: String,
}

#[cfg(any(target_os = "linux", test))]
fn resolved_process_count_limit(
    resource_guard_env: &HashMap<String, String>,
) -> ResolvedProcessCountLimit {
    let runner_default_override = std::env::var(PROCESS_COUNT_LIMIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let runner_default = runner_default_override.unwrap_or(DEFAULT_PROCESS_COUNT_LIMIT);
    let ceiling = resource_guard_limit(
        PROCESS_COUNT_LIMIT_CEILING_ENV,
        DEFAULT_PROCESS_COUNT_LIMIT_CEILING,
    );
    let requested = resource_guard_env
        .get(PROCESS_COUNT_LIMIT_ENV)
        .and_then(|value| value.trim().parse::<u64>().ok());
    resolve_process_count_limit(
        runner_default,
        ceiling,
        requested,
        runner_default_override.is_some(),
    )
}

#[cfg(any(target_os = "linux", test))]
fn resolve_process_count_limit(
    runner_default: u64,
    ceiling: u64,
    requested: Option<u64>,
    runner_default_is_override: bool,
) -> ResolvedProcessCountLimit {
    let (limit, source) = match requested {
        Some(0) if runner_default_is_override => (runner_default, "runner_override"),
        Some(0) => (runner_default, "job_override_rejected"),
        Some(_) if ceiling == 0 => (runner_default, "job_override_rejected"),
        Some(requested) if requested > ceiling => (ceiling, "job_override_capped"),
        Some(requested) => (requested, "job_override"),
        None if runner_default_is_override => (runner_default, "runner_override"),
        None => (runner_default, "default"),
    };
    ResolvedProcessCountLimit {
        limit,
        requested,
        ceiling,
        source: source.to_string(),
    }
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

#[cfg(any(target_os = "linux", test))]
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

    #[test]
    fn spawn_error_preserves_e2big_command_sizing_without_exposing_secrets() {
        let mut command = Command::new("runner?token=secret");
        command
            .arg("small")
            .arg("x".repeat(64))
            .current_dir("/work?token=secret")
            .env_clear()
            .env("PUBLIC", "value")
            .env("TOKEN", "secret");

        let error =
            runner_command_spawn_error(&command, &std::io::Error::from_raw_os_error(libc::E2BIG));

        assert_eq!(error.code.as_str(), "internal.io_error");
        assert_eq!(error.details["operation"], "runner_command_spawn");
        assert_eq!(
            error.details["classification"],
            "argv_environment_too_large"
        );
        assert_eq!(error.details["raw_os_error"], libc::E2BIG);
        assert_eq!(error.details["argv_count"], 3);
        assert_eq!(error.details["argv_bytes"], 19 + 5 + 64);
        assert_eq!(error.details["max_argument_bytes"], 64);
        assert_eq!(error.details["environment_count"], 2);
        assert_eq!(error.details["environment_bytes"], 24);
        assert_eq!(
            error.details["largest_environment_variable"]["name"],
            "PUBLIC"
        );
        assert_eq!(
            error.details["largest_environment_variable"]["value_bytes"],
            5
        );
        assert_eq!(error.details["environment_variables"][1]["name"], "TOKEN");
        assert!(!error.details.to_string().contains("secret"));
        assert_eq!(error.details["executable"], "runner?token=[REDACTED]");
        assert_eq!(error.details["cwd"], "/work?token=[REDACTED]");
        assert_ne!(error.details["io_error_kind"], serde_json::Value::Null);
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
            &HashMap::new(),
            None,
        )
        .expect_err("callback failure returns");

        // `Error::internal_io` carries a fixed "IO error" message and puts the
        // formatted cause in `details["error"]`, so assert the child-identity
        // failure surfaces there rather than on the top-level message.
        assert!(error.details["error"]
            .as_str()
            .is_some_and(|detail| detail.contains("persist child identity")));
        assert!(!homeboy_core::process::pid_is_running(
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
            &HashMap::new(),
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
            &HashMap::new(),
            None,
        )
        .expect_err("unsupported platforms must fail before spawning a child");

        assert!(error.message.contains("process-tree isolation"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_initial_child_identity_persistence_terminates_and_reaps_the_process_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let descendant_pid_file = temp.path().join("descendant.pid");
        let callback_pid_file = descendant_pid_file.clone();
        let spawned_pid = Arc::new(Mutex::new(None));
        let progress_sink = {
            let spawned_pid = Arc::clone(&spawned_pid);
            Arc::new(move |data: Value| {
                *spawned_pid.lock().expect("pid lock") = data["process"]["root_pid"].as_u64();
                wait_for_path(&callback_pid_file);
                Err(Error::internal_io(
                    "durable progress unavailable",
                    Some("test progress persistence".to_string()),
                ))
            })
        };
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!(
                "sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
                shell_quote_path(&descendant_pid_file)
            ),
        ]);

        let error = measured_command_output_until_cancelled_with_progress(
            &mut command,
            || false,
            Some(progress_sink),
            true,
            None,
            None,
            &HashMap::new(),
            None,
        )
        .expect_err("initial identity persistence failure must fail execution");

        assert!(!error.message.is_empty(), "persistence failure is surfaced");
        let pid = spawned_pid
            .lock()
            .expect("pid lock")
            .expect("initial callback received PID") as u32;
        assert!(!homeboy_core::process::pid_is_running(pid));
        let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert_ne!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[cfg(unix)]
    fn shell_quote_path(path: &std::path::Path) -> String {
        format!(
            "'{}'",
            path.display().to_string().replace('\'', "'\\\"'\\\"'")
        )
    }

    #[cfg(unix)]
    fn wait_for_path(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_measured_command_execution_remains_available() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        let output = measured_command_output(&mut command, &HashMap::new(), None)
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
            process_count_limit_source: Some("default".to_string()),
            requested_process_count_limit: None,
            process_count_limit_ceiling: Some(256),
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

    #[test]
    fn trusted_job_process_count_override_is_bounded_by_runner_ceiling() {
        let accepted = resolve_process_count_limit(128, 256, Some(200), false);
        assert_eq!(accepted.limit, 200);
        assert_eq!(accepted.requested, Some(200));
        assert_eq!(accepted.ceiling, 256);
        assert_eq!(accepted.source, "job_override");

        let capped = resolve_process_count_limit(128, 256, Some(512), false);
        assert_eq!(capped.limit, 256);
        assert_eq!(capped.requested, Some(512));
        assert_eq!(capped.source, "job_override_capped");
    }

    #[test]
    fn job_cannot_disable_runner_process_guard() {
        let resolved = resolve_process_count_limit(128, 256, Some(0), false);
        assert_eq!(resolved.limit, 128);
        assert_eq!(resolved.source, "job_override_rejected");

        let runner_disabled = resolve_process_count_limit(0, 256, Some(0), true);
        assert_eq!(runner_disabled.limit, 0);
        assert_eq!(runner_disabled.source, "runner_override");

        let disabled_ceiling = resolve_process_count_limit(128, 0, Some(200), false);
        assert_eq!(disabled_ceiling.limit, 128);
        assert_eq!(disabled_ceiling.source, "job_override_rejected");
    }

    #[test]
    fn job_ceiling_does_not_inherit_a_larger_runner_default() {
        let runner_default = resolve_process_count_limit(512, 256, None, true);
        assert_eq!(runner_default.limit, 512);
        assert_eq!(runner_default.source, "runner_override");

        let requested = resolve_process_count_limit(512, 256, Some(512), true);
        assert_eq!(requested.limit, 256);
        assert_eq!(requested.source, "job_override_capped");
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
