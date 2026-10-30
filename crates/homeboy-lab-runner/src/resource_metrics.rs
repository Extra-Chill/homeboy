use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use homeboy_core::engine::command::{
    supports_process_tree_isolation, wait_with_bounded_output,
    wait_with_bounded_output_until_cancelled_with_stdout_observer_owned, CommandCaptureMetadata,
    ExecutionOwner, StdoutLineObserver, DEFAULT_CAPTURE_LIMIT_BYTES,
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
/// The default process-count limit per available core, and its floor.
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_LIMIT_PER_CORE: u64 = 16;
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_LIMIT_FLOOR: u64 = 128;
/// The default process-count ceiling per available core, and its floor.
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_CEILING_PER_CORE: u64 = 32;
#[cfg(any(target_os = "linux", test))]
const PROCESS_COUNT_CEILING_FLOOR: u64 = 256;

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
pub use homeboy_runner_contract::{
    RunnerCgroupMemoryEvidence, RunnerProcessNameCount, RunnerResourceGuardLimits,
    RunnerResourceGuardViolation, RunnerResourceMetrics,
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
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let child = command
        .spawn()
        .map_err(|error| runner_command_spawn_error(command, &error))?;
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
    child_started: Option<homeboy_core::daemon::runner_exec_driver::ExecChildStarted>,
    concurrency_limit: Option<usize>,
) -> Result<MeasuredOutput> {
    if require_child_identity_acknowledgement {
        require_process_tree_isolation()?;
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut owner = ExecutionOwner::spawn(command)
        .map_err(|error| runner_command_spawn_error(command, &error))?;
    let identity = owner.identity();
    let pid = identity.root_pid;
    if let Some(child_started) = child_started {
        if let Err(error) = child_started(identity.into()) {
            let cleanup = owner
                .drain_and_reap()
                .and_then(homeboy_core::engine::command::ExecutionOutcome::into_root_status)
                .map(|_| ());
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
            let cleanup = owner
                .drain_and_reap()
                .and_then(homeboy_core::engine::command::ExecutionOutcome::into_root_status)
                .map(|_| ());
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
    let bounded_output = wait_with_bounded_output_until_cancelled_with_stdout_observer_owned(
        &mut owner,
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

/// Captures cgroup-v2 evidence outside the child so the terminal metrics still
/// exist when the child is killed before it can write its own diagnostics.
#[cfg(target_os = "linux")]
struct CgroupMemoryCollector {
    files: Option<CgroupMemoryFiles>,
    before: Option<CgroupMemorySnapshot>,
}

#[cfg(not(target_os = "linux"))]
struct CgroupMemoryCollector;

#[cfg(target_os = "linux")]
#[derive(Default)]
struct CgroupMemorySnapshot {
    memory_limit_bytes: Option<u64>,
    memory_current_bytes: Option<u64>,
    memory_peak_bytes: Option<u64>,
    oom_kill_count: Option<u64>,
}

#[cfg(target_os = "linux")]
struct CgroupMemoryFiles {
    memory_limit: Option<std::fs::File>,
    memory_current: Option<std::fs::File>,
    memory_peak: Option<std::fs::File>,
    memory_events: Option<std::fs::File>,
}

impl CgroupMemoryCollector {
    #[cfg(target_os = "linux")]
    fn start(pid: u32) -> Self {
        // Pin the resolved directory before opening its counters: the child's
        // /proc/<pid>/root alias can disappear between those opens. If it has
        // already disappeared, subprocesses inherit the runner's cgroup, so
        // resolve the parent as a fallback.
        let directory = cgroup_v2_directory_for_pid(pid)
            .or_else(|| cgroup_v2_directory_for_pid(std::process::id()));
        let mut files = directory.map(open_cgroup_memory_files);
        let before = files.as_mut().map(read_cgroup_memory_snapshot);
        Self { files, before }
    }

    #[cfg(not(target_os = "linux"))]
    fn start(_pid: u32) -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    fn finish(self) -> RunnerCgroupMemoryEvidence {
        let Some(mut files) = self.files else {
            return cgroup_memory_unavailable();
        };
        let before = self.before.unwrap_or_default();
        let after = read_cgroup_memory_snapshot(&mut files);
        cgroup_memory_evidence(before, after)
    }

    #[cfg(not(target_os = "linux"))]
    fn finish(self) -> RunnerCgroupMemoryEvidence {
        RunnerCgroupMemoryEvidence::default()
    }
}

#[cfg(target_os = "linux")]
fn cgroup_memory_evidence(
    before: CgroupMemorySnapshot,
    after: CgroupMemorySnapshot,
) -> RunnerCgroupMemoryEvidence {
    let oom_kill_delta = match (before.oom_kill_count, after.oom_kill_count) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    let fully_observed = after.memory_current_bytes.is_some()
        && after.memory_peak_bytes.is_some()
        && before.oom_kill_count.is_some()
        && after.oom_kill_count.is_some();
    RunnerCgroupMemoryEvidence {
        status: if fully_observed {
            "observed"
        } else {
            "partial"
        }
        .to_string(),
        source: "linux_cgroup_v2".to_string(),
        scope: "child_cgroup".to_string(),
        // A counter delta establishes a cgroup event, not which member was
        // selected by the kernel OOM killer.
        oom_kill_attribution: "unknown".to_string(),
        memory_limit_bytes: after.memory_limit_bytes.or(before.memory_limit_bytes),
        memory_current_bytes: after.memory_current_bytes,
        memory_peak_bytes: after.memory_peak_bytes,
        oom_kill_count_before: before.oom_kill_count,
        oom_kill_count_after: after.oom_kill_count,
        oom_kill_delta,
    }
}

#[cfg(target_os = "linux")]
fn cgroup_memory_unavailable() -> RunnerCgroupMemoryEvidence {
    RunnerCgroupMemoryEvidence {
        status: "unavailable".to_string(),
        source: "linux_cgroup_v2".to_string(),
        scope: "unavailable".to_string(),
        ..RunnerCgroupMemoryEvidence::default()
    }
}

#[cfg(target_os = "linux")]
fn cgroup_v2_directory_for_pid(pid: u32) -> Option<std::fs::File> {
    let membership = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let mountinfo = std::fs::read_to_string(format!("/proc/{pid}/mountinfo")).ok()?;
    let membership = cgroup_v2_membership_path(&membership)?;
    let (mount_root, mount_point) = cgroup_v2_mount(&mountinfo)?;
    let relative = cgroup_relative_to_mount(&membership, &mount_root)?;
    // Resolve through the target's root namespace. Containerized children can
    // mount cgroup-v2 at a namespace-local root that is not visible at the
    // runner's `/sys/fs/cgroup` path.
    let directory = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("root")
        .join(mount_point.strip_prefix('/')?)
        .join(relative);
    std::fs::File::open(directory).ok()
}

#[cfg(target_os = "linux")]
fn cgroup_v2_membership_path(membership: &str) -> Option<String> {
    membership.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        (fields.next() == Some("0") && fields.next() == Some(""))
            .then(|| fields.next())
            .flatten()
            .map(str::to_string)
    })
}

#[cfg(target_os = "linux")]
fn cgroup_v2_mount(mountinfo: &str) -> Option<(String, String)> {
    mountinfo.lines().find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let separator = fields.iter().position(|field| *field == "-")?;
        (fields.get(separator + 1) == Some(&"cgroup2"))
            .then(|| Some((fields.get(3)?.to_string(), fields.get(4)?.to_string())))
            .flatten()
    })
}

#[cfg(target_os = "linux")]
fn cgroup_relative_to_mount(membership: &str, mount_root: &str) -> Option<std::path::PathBuf> {
    let mount_root = mount_root.trim_end_matches('/');
    let relative = if mount_root.is_empty() {
        membership.trim_start_matches('/')
    } else {
        membership
            .strip_prefix(mount_root)?
            .strip_prefix('/')
            .unwrap_or("")
    };
    let relative = std::path::Path::new(relative);
    relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
        .then(|| relative.to_path_buf())
}

#[cfg(target_os = "linux")]
fn open_cgroup_memory_files(directory: std::fs::File) -> CgroupMemoryFiles {
    let directory_path =
        std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let open = |name| std::fs::File::open(directory_path.join(name)).ok();
    CgroupMemoryFiles {
        memory_limit: open("memory.max"),
        memory_current: open("memory.current"),
        memory_peak: open("memory.peak"),
        memory_events: open("memory.events"),
    }
}

#[cfg(target_os = "linux")]
fn read_cgroup_memory_snapshot(files: &mut CgroupMemoryFiles) -> CgroupMemorySnapshot {
    CgroupMemorySnapshot {
        memory_limit_bytes: read_cgroup_u64(files.memory_limit.as_mut(), true),
        memory_current_bytes: read_cgroup_u64(files.memory_current.as_mut(), false),
        memory_peak_bytes: read_cgroup_u64(files.memory_peak.as_mut(), false),
        oom_kill_count: read_cgroup_event_count(files.memory_events.as_mut(), "oom_kill"),
    }
}

#[cfg(target_os = "linux")]
fn read_cgroup_u64(file: Option<&mut std::fs::File>, accepts_max: bool) -> Option<u64> {
    let value = read_cgroup_file(file)?;
    let value = value.trim();
    if accepts_max && value == "max" {
        return None;
    }
    value.parse().ok()
}

#[cfg(target_os = "linux")]
fn read_cgroup_event_count(file: Option<&mut std::fs::File>, name: &str) -> Option<u64> {
    let events = read_cgroup_file(file)?;
    events.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some(name))
            .then(|| fields.next()?.parse().ok())
            .flatten()
    })
}

#[cfg(target_os = "linux")]
fn read_cgroup_file(file: Option<&mut std::fs::File>) -> Option<String> {
    let file = file?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut value = String::new();
    file.read_to_string(&mut value).ok()?;
    Some(value)
}

struct ResourceMetricsCollector {
    supported: bool,
    state: Arc<Mutex<MetricsState>>,
    sampler: ActiveSampler,
    guard_limits: Option<RunnerResourceGuardLimits>,
    cgroup_memory: CgroupMemoryCollector,
}

/// The sampling thread and this job's entry in the active-runner RSS registry.
/// Stopping on drop releases both on every exit path, including a failed wait,
/// so a finished job never keeps reducing the RSS budget of later jobs.
#[derive(Default)]
struct ActiveSampler {
    stop: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<()>>,
    #[cfg(target_os = "linux")]
    registered_root_pid: Option<u32>,
}

impl ActiveSampler {
    fn stop(&mut self) {
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
    }
}

impl Drop for ActiveSampler {
    fn drop(&mut self) {
        self.stop();
    }
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
        let cgroup_memory = CgroupMemoryCollector::start(root_pid);
        if !supported && progress_sink.is_none() {
            return Self {
                supported,
                state,
                sampler: ActiveSampler::default(),
                guard_limits,
                cgroup_memory,
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
            state,
            sampler: ActiveSampler {
                stop: Some(stop),
                handle: Some(handle),
                #[cfg(target_os = "linux")]
                registered_root_pid: Some(root_pid),
            },
            guard_limits,
            cgroup_memory,
        }
    }

    fn finish(mut self, duration: Duration) -> RunnerResourceMetrics {
        self.sampler.stop();
        let state = self.state.lock().expect("resource metrics mutex poisoned");
        let cgroup_memory = self.cgroup_memory.finish();
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
            cgroup_memory,
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
    top_processes: Vec<RunnerProcessNameCount>,
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
    let mut names = Vec::new();
    let mut rss_bytes = 0_u64;
    let mut user_ticks = 0_u64;
    let mut system_ticks = 0_u64;

    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(stat) = stats.get(&pid) {
            names.push(stat.name.as_str());
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
        top_processes: top_process_names(names),
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
    .map(|violation| RunnerResourceGuardViolation {
        process_count_limit_source: limits.process_count_limit_source.clone(),
        process_count_limit_ceiling: limits.process_count_limit_ceiling,
        top_processes: snapshot.top_processes.clone(),
        ..violation
    })
}

/// Count process names, most frequent first (ties by name), bounded so a
/// fork bomb cannot grow the recorded violation.
#[cfg(any(target_os = "linux", test))]
fn top_process_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<RunnerProcessNameCount> {
    let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for name in names {
        *counts.entry(name).or_default() += 1;
    }
    let mut top: Vec<RunnerProcessNameCount> = counts
        .into_iter()
        .map(|(name, count)| RunnerProcessNameCount {
            name: name.to_string(),
            count,
        })
        .collect();
    top.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    top.truncate(homeboy_runner_contract::TOP_PROCESS_NAME_LIMIT);
    top
}

#[cfg(target_os = "linux")]
fn resolved_resource_guard_limits(
    concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    let concurrency = u64::try_from(concurrency_limit.unwrap_or(1).max(1)).unwrap_or(u64::MAX);
    let memory_capacity_bytes =
        homeboy_core::resources::memory::probe_system_memory().map(|memory| memory.total_bytes);
    let rss_limit = resolved_rss_limit(
        memory_capacity_bytes,
        daemon_policy_env(homeboy_runner_contract::RSS_LIMIT_ENV),
    );
    let process_count = resolve_process_count_limit(
        available_parallelism(),
        daemon_policy_env(homeboy_runner_contract::PROCESS_COUNT_LIMIT_ENV),
        daemon_policy_env(homeboy_runner_contract::PROCESS_COUNT_CEILING_ENV),
    );
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
    _concurrency_limit: Option<usize>,
) -> Option<RunnerResourceGuardLimits> {
    None
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq, Eq)]
struct ResolvedProcessCountLimit {
    limit: u64,
    requested: Option<u64>,
    ceiling: u64,
    source: String,
}

/// Resolve the runner-owned process-count limit. The default scales with the
/// runner's available parallelism because cargo, compilers, and test runners
/// fan out per core. The daemon's own environment can override the limit and
/// the ceiling; a ceiling of `0` disables the clamp and a limit of `0`
/// disables the guard.
#[cfg(any(target_os = "linux", test))]
fn resolve_process_count_limit(
    parallelism: u64,
    limit_override: Option<u64>,
    ceiling_override: Option<u64>,
) -> ResolvedProcessCountLimit {
    let ceiling = ceiling_override.unwrap_or_else(|| {
        parallelism
            .saturating_mul(PROCESS_COUNT_CEILING_PER_CORE)
            .max(PROCESS_COUNT_CEILING_FLOOR)
    });
    let (limit, source) = match limit_override {
        Some(limit) => (limit, "runner_override"),
        None => (
            parallelism
                .saturating_mul(PROCESS_COUNT_LIMIT_PER_CORE)
                .max(PROCESS_COUNT_LIMIT_FLOOR),
            "default",
        ),
    };
    let (limit, source) = if ceiling > 0 && limit > ceiling {
        (ceiling, "runner_ceiling")
    } else {
        (limit, source)
    };
    ResolvedProcessCountLimit {
        limit,
        requested: limit_override,
        ceiling,
        source: source.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn available_parallelism() -> u64 {
    thread::available_parallelism().map_or(1, |cores| cores.get().try_into().unwrap_or(u64::MAX))
}

/// A runner-owned guard policy value from the daemon's own environment.
#[cfg(target_os = "linux")]
fn daemon_policy_env(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
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
        process_count_limit_source: None,
        process_count_limit_ceiling: None,
        top_processes: Vec::new(),
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcessStat {
    pid: i32,
    ppid: i32,
    name: String,
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
    let start_command = stat.find('(')?;
    let end_command = stat.rfind(')')?;
    let name = stat.get(start_command + 1..end_command)?.to_string();
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
        name,
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
    use homeboy_core::daemon::runner_exec_driver::RunnerChildIdentity;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[cfg(target_os = "linux")]
    #[test]
    fn dropped_collector_releases_its_active_rss_registration() {
        let mut child = Command::new("sleep").arg("5").spawn().expect("spawn sleep");
        let pid = child.id();
        let collector = ResourceMetricsCollector::start(pid, Instant::now(), None, None, Some(1));
        let registered = || {
            active_runner_rss()
                .lock()
                .expect("active runner rss mutex poisoned")
                .contains_key(&pid)
        };
        assert!(registered());

        // A failed wait returns before `finish`; the job must still leave the
        // registry or its last RSS keeps shrinking every later job's budget.
        drop(collector);
        assert!(!registered());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    use std::os::unix::process::ExitStatusExt;

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
            Some(Arc::new(move |identity: RunnerChildIdentity| {
                callback_pid.store(identity.pid, Ordering::SeqCst);
                Err(Error::internal_unexpected("persist child identity"))
            })),
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

    #[cfg(unix)]
    #[test]
    fn runner_cancel_reaps_term_resistant_descendants_and_persists_root_identity() {
        struct ReapOnDrop(Arc<Mutex<Vec<u32>>>);
        impl Drop for ReapOnDrop {
            fn drop(&mut self) {
                for pid in self.0.lock().expect("cleanup pids").iter().copied() {
                    if pid != 0 && homeboy_core::process::pid_is_running(pid) {
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGKILL);
                        }
                    }
                }
            }
        }

        let temp = tempfile::tempdir().expect("unique tempfile dir");
        let pid_file = temp.path().join("descendant.pid");
        let cleanup_pids = Arc::new(Mutex::new(Vec::new()));
        let _cleanup = ReapOnDrop(Arc::clone(&cleanup_pids));
        let script = format!(
            "trap '' TERM; sleep 30 & echo $! > {}; wait",
            pid_file.display()
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        let persisted = Arc::new(AtomicU32::new(0));
        let persisted_pgid = Arc::new(AtomicU32::new(0));
        let callback_pid = Arc::clone(&persisted);
        let callback_pgid = Arc::clone(&persisted_pgid);
        let callback_cleanup = Arc::clone(&cleanup_pids);
        let output = measured_command_output_until_cancelled_with_progress(
            &mut command,
            || pid_file.exists(),
            None,
            false,
            None,
            Some(Arc::new(move |identity: RunnerChildIdentity| {
                let root_pid = identity.pid;
                callback_pid.store(root_pid, Ordering::SeqCst);
                callback_cleanup
                    .lock()
                    .expect("cleanup pids")
                    .push(root_pid);
                #[cfg(target_os = "linux")]
                {
                    let pgid = unsafe { libc::getpgid(root_pid as libc::pid_t) };
                    assert!(
                        pgid >= 0,
                        "getpgid must succeed while the runner root is still alive"
                    );
                    callback_pgid.store(pgid as u32, Ordering::SeqCst);
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = callback_pgid;
                }
                Ok(())
            })),
            None,
        )
        .expect("runner cancel completes");
        assert!(!output.output.status.success());
        let root_pid = persisted.load(Ordering::SeqCst);
        assert_ne!(root_pid, 0);
        #[cfg(target_os = "linux")]
        assert_eq!(
            persisted_pgid.load(Ordering::SeqCst),
            root_pid,
            "persisted runner identity is the live root leader"
        );
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant");
        cleanup_pids
            .lock()
            .expect("cleanup pids")
            .push(descendant_pid);
        assert!(
            !homeboy_core::process::pid_is_running(descendant_pid),
            "runner cancel left TERM-resistant descendant {descendant_pid}"
        );
        assert!(!homeboy_core::process::pid_is_running(root_pid));
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
                homeboy_engine_primitives::shell::quote_path(
                    &descendant_pid_file.display().to_string()
                )
            ),
        ]);

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
        assert!(!homeboy_core::process::pid_is_running(pid));
        let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            !homeboy_core::process::pid_is_running(descendant_pid as u32),
            "initial identity persistence failure left descendant {descendant_pid} runnable"
        );
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
    fn actual_runner_normal_exit_preserves_peak_metrics() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        let output = measured_command_output(&mut command, None)
            .expect("ordinary measured command execution remains available");

        assert!(output.output.status.success());
        assert!(
            !output.metrics.cgroup_memory.status.is_empty(),
            "unsupported cgroup telemetry must be explicit rather than look like a successful zero"
        );
        #[cfg(target_os = "linux")]
        assert!(
            output.metrics.cgroup_memory.memory_peak_bytes.is_some()
                || output.metrics.cgroup_memory.status != "observed",
            "a fully observed normal exit must retain cgroup peak memory"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn actual_runner_preserves_ordinary_sigkill_as_non_oom() {
        let mut command = Command::new("sh");
        command.args(["-c", "kill -KILL $$"]);

        let output = measured_command_output(&mut command, None)
            .expect("runner captures signal termination");

        assert_eq!(output.output.status.signal(), Some(libc::SIGKILL));
        assert_eq!(output.metrics.cgroup_memory.oom_kill_delta, Some(0));
        assert_eq!(output.metrics.cgroup_memory.oom_kill_attribution, "unknown");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_counters_remain_readable_after_the_child_proc_namespace_disappears() {
        let mut child = Command::new("sh")
            .args(["-c", "read release"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn blocked child");
        let directory = cgroup_v2_directory_for_pid(child.id());
        child.kill().expect("kill owned child");
        assert_eq!(
            child.wait().expect("reap child").signal(),
            Some(libc::SIGKILL)
        );
        assert!(!std::path::Path::new(&format!("/proc/{}/root", child.id())).exists());

        let mut files = open_cgroup_memory_files(directory.expect("pin live child cgroup"));
        assert!(
            read_cgroup_memory_snapshot(&mut files)
                .oom_kill_count
                .is_some(),
            "counter opens must use the retained directory, not the dead child's /proc alias"
        );
        assert_eq!(
            CgroupMemoryCollector::start(child.id())
                .finish()
                .oom_kill_delta,
            Some(0),
            "an already-reaped child uses the inherited parent cgroup"
        );
    }

    /// This test uses the production measurement boundary but only runs when a
    /// disposable cgroup-limited container explicitly opts in. Running an OOM
    /// allocator in an unconstrained developer cgroup would be unsafe.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires explicit opt-in in a disposable cgroup-limited container"]
    fn actual_runner_controlled_oom_is_authoritative_when_enabled() {
        require_controlled_oom_test();
        let test_binary = std::env::current_exe().expect("test binary path");
        let mut command = Command::new(test_binary);
        command.args([
            "resource_metrics::tests::synthetic_memory_hog",
            "--exact",
            "--ignored",
            "--nocapture",
        ]);

        let output = measured_command_output(&mut command, None)
            .expect("runner retains metrics after child OOM kill");

        assert_eq!(output.output.status.signal(), Some(libc::SIGKILL));
        assert_eq!(output.metrics.cgroup_memory.status, "observed");
        assert_eq!(output.metrics.cgroup_memory.oom_kill_delta, Some(1));
        assert_eq!(output.metrics.cgroup_memory.scope, "child_cgroup");
        assert_eq!(output.metrics.cgroup_memory.oom_kill_attribution, "unknown");
        assert!(output.metrics.cgroup_memory.memory_peak_bytes.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "OOM allocator subprocess invoked only by the controlled OOM test"]
    fn synthetic_memory_hog() {
        require_controlled_oom_test();
        let mut chunks = Vec::new();
        loop {
            chunks.push(vec![1_u8; 1024 * 1024]);
        }
    }

    #[cfg(target_os = "linux")]
    fn require_controlled_oom_test() {
        assert!(
            std::env::var_os("HOMEBOY_RUN_CONTROLLED_CGROUP_OOM_TEST").is_some(),
            "controlled OOM execution requires explicit container opt-in"
        );
        let directory = cgroup_v2_directory_for_pid(std::process::id())
            .expect("controlled OOM execution requires a readable cgroup v2 directory");
        let mut files = open_cgroup_memory_files(directory);
        let limit = read_cgroup_memory_snapshot(&mut files).memory_limit_bytes;
        assert!(limit.is_some_and(|bytes| bytes > 0 && bytes <= 256 * 1024 * 1024),
            "controlled OOM execution requires a disposable cgroup capped at 256 MiB or less; observed {limit:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_oom_kill_delta_is_authoritative_evidence() {
        let evidence = cgroup_memory_evidence(
            CgroupMemorySnapshot {
                memory_limit_bytes: Some(64),
                memory_current_bytes: Some(63),
                memory_peak_bytes: Some(64),
                oom_kill_count: Some(4),
            },
            CgroupMemorySnapshot {
                memory_limit_bytes: Some(64),
                memory_current_bytes: Some(12),
                memory_peak_bytes: Some(64),
                oom_kill_count: Some(5),
            },
        );

        assert_eq!(evidence.status, "observed");
        assert_eq!(evidence.oom_kill_delta, Some(1));
        assert_eq!(evidence.scope, "child_cgroup");
        assert_eq!(evidence.oom_kill_attribution, "unknown");
        assert_eq!(evidence.memory_limit_bytes, Some(64));
        assert_eq!(evidence.memory_peak_bytes, Some(64));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_evidence_does_not_misclassify_an_ordinary_sigkill_as_oom() {
        let evidence = cgroup_memory_evidence(
            CgroupMemorySnapshot {
                memory_limit_bytes: Some(64),
                memory_current_bytes: Some(24),
                memory_peak_bytes: Some(32),
                oom_kill_count: Some(4),
            },
            CgroupMemorySnapshot {
                memory_limit_bytes: Some(64),
                memory_current_bytes: Some(12),
                memory_peak_bytes: Some(32),
                oom_kill_count: Some(4),
            },
        );

        assert_eq!(evidence.status, "observed");
        assert_eq!(evidence.oom_kill_delta, Some(0));
        assert_ne!(evidence.oom_kill_delta, Some(1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_membership_and_mount_resolution_are_namespace_aware() {
        assert_eq!(
            cgroup_v2_membership_path("0::/runner/job\n"),
            Some("/runner/job".to_string())
        );
        assert_eq!(
            cgroup_relative_to_mount("/runner/job", "/runner"),
            Some(std::path::PathBuf::from("job"))
        );
        assert_eq!(
            cgroup_relative_to_mount("/runner", "/runner"),
            Some(std::path::PathBuf::new())
        );
        assert_eq!(
            cgroup_v2_mount("36 25 0:32 /runner /sys/fs/cgroup rw - cgroup2 cgroup rw\n"),
            Some(("/runner".to_string(), "/sys/fs/cgroup".to_string()))
        );
        assert!(cgroup_relative_to_mount("/runner/../../etc", "/runner").is_none());
        assert!(cgroup_v2_membership_path("1:name=/legacy\n").is_none());
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

    fn resolved(limit: u64, ceiling: u64, source: &str) -> ResolvedProcessCountLimit {
        ResolvedProcessCountLimit {
            limit,
            requested: None,
            ceiling,
            source: source.to_string(),
        }
    }

    #[test]
    fn default_process_limit_and_ceiling_scale_with_parallelism() {
        assert_eq!(
            resolve_process_count_limit(32, None, None),
            resolved(512, 1024, "default")
        );
        assert_eq!(
            resolve_process_count_limit(12, None, None),
            resolved(192, 384, "default")
        );
    }

    #[test]
    fn default_process_limit_and_ceiling_keep_their_floors_on_small_hosts() {
        for parallelism in [1, 4, 8] {
            assert_eq!(
                resolve_process_count_limit(parallelism, None, None),
                resolved(128, 256, "default")
            );
        }
    }

    #[test]
    fn daemon_env_overrides_the_process_limit_and_ceiling() {
        let limit = resolve_process_count_limit(32, Some(768), None);
        assert_eq!(limit.limit, 768);
        assert_eq!(limit.requested, Some(768));
        assert_eq!(limit.source, "runner_override");

        let ceiling = resolve_process_count_limit(32, None, Some(2048));
        assert_eq!(ceiling, resolved(512, 2048, "default"));

        let disabled = resolve_process_count_limit(32, Some(0), None);
        assert_eq!(disabled.limit, 0, "a limit of 0 disables the guard");
        assert_eq!(disabled.source, "runner_override");

        let unclamped = resolve_process_count_limit(32, Some(4096), Some(0));
        assert_eq!(unclamped.limit, 4096, "a ceiling of 0 disables the clamp");
    }

    #[test]
    fn process_limit_is_clamped_to_the_ceiling() {
        let override_capped = resolve_process_count_limit(32, Some(4096), None);
        assert_eq!(override_capped.limit, 1024);
        assert_eq!(override_capped.requested, Some(4096));
        assert_eq!(override_capped.source, "runner_ceiling");

        assert_eq!(
            resolve_process_count_limit(32, None, Some(300)),
            resolved(300, 300, "runner_ceiling")
        );
    }

    #[test]
    fn violation_breakdown_is_ordered_by_count_then_name_and_bounded() {
        let mut names = vec!["cc"; 21];
        names.extend(vec!["rustc"; 34]);
        names.extend(vec!["git"; 12]);
        names.extend(vec!["homeboy"; 12]);
        let leaked: Vec<String> = (0..20).map(|index| format!("leak-{index:02}")).collect();
        names.extend(leaked.iter().map(String::as_str));

        let top = top_process_names(names);

        assert_eq!(top.len(), homeboy_runner_contract::TOP_PROCESS_NAME_LIMIT);
        let rendered: Vec<(&str, u64)> = top
            .iter()
            .map(|process| (process.name.as_str(), process.count))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("rustc", 34),
                ("cc", 21),
                ("git", 12),
                ("homeboy", 12),
                ("leak-00", 1),
                ("leak-01", 1),
                ("leak-02", 1),
                ("leak-03", 1),
                ("leak-04", 1),
                ("leak-05", 1),
            ]
        );
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
