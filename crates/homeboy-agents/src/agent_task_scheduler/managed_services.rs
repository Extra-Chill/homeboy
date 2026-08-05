//! Plan-owned, runner-local process services for agent-task execution.
//!
//! This deliberately owns only process allocation/readiness and lifecycle
//! evidence. Public URLs are opaque references supplied by a preview/tunnel
//! provider, keeping this contract independent of any product integration.

use std::fs::{File, OpenOptions};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use homeboy_core::agent_task_config::AgentTaskManagedServiceReadiness;
use homeboy_core::process::{
    process_identity_state_with_start_identity, process_start_identity, terminate_process_tree,
    ProcessIdentityState, ProcessStartIdentity,
};
use serde_json::{json, Value};

use super::{
    AgentTaskManagedService, AgentTaskManagedServiceLifecycle, AgentTaskManagedServiceReadinessKind,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskManagedServiceRecord {
    pub id: String,
    pub state: String,
    /// Generated before exec. A stale reconciler can distinguish a planned
    /// launch that never acquired a process from a live owned process.
    pub launch_token: String,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub log_path: Option<String>,
    pub pid: Option<u32>,
    pub cleanup: Option<String>,
    pub provenance: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_identity: Option<ProcessStartIdentity>,
    /// The process group created before exec. Unlike the leader PID it remains
    /// useful when a service daemonizes and its direct parent exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_runner_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_lease: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_attempts: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_origin_evidence: Option<Value>,
}

/// Durable input for the independently running service supervisor.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskServiceWorkerRequest {
    pub schema: String,
    pub operation: String,
    pub run_id: String,
    #[serde(default)]
    pub services: Vec<AgentTaskManagedService>,
    pub parent_pid: u32,
    #[serde(default = "default_worker_ttl_ms")]
    pub parent_ttl_ms: u64,
}

impl AgentTaskServiceWorkerRequest {
    pub const SCHEMA: &'static str = "homeboy/agent-task-service-worker-request/v1";
}

/// Worker-owned state. The identity is committed before a payload process can
/// be spawned, allowing reconciliation to distinguish a crashed worker from a
/// scheduler that never managed to invoke one.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskServiceWorkerState {
    pub schema: String,
    pub run_id: String,
    pub state: String,
    pub worker_pid: u32,
    pub worker_identity: Option<ProcessStartIdentity>,
    pub parent_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_identity: Option<ProcessStartIdentity>,
    pub heartbeat_unix_ms: u64,
    #[serde(default)]
    pub services: Vec<AgentTaskManagedServiceRecord>,
    pub detail: Option<String>,
}

impl AgentTaskServiceWorkerState {
    pub const SCHEMA: &'static str = "homeboy/agent-task-service-worker-state/v1";
}

fn default_worker_ttl_ms() -> u64 {
    30_000
}

/// Execution-host service supervisor. It is instantiated by whichever host
/// executes the plan (controller or Lab runner), never by a remote caller.
pub(crate) struct AgentTaskServiceSupervisor {
    services: Vec<RunningService>,
}

struct RunningService {
    spec: AgentTaskManagedService,
    child: Child,
    containment: homeboy_core::process::ProcessContainment,
    record: AgentTaskManagedServiceRecord,
    port_lease: Option<PortLease>,
    listener: Option<TcpListener>,
}

struct PortLease {
    path: std::path::PathBuf,
    _file: File,
}

type PortLeaseAllocation = (Option<u16>, Option<PortLease>, Option<TcpListener>);

impl Drop for PortLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl AgentTaskServiceSupervisor {
    pub(super) fn start(specs: &[AgentTaskManagedService], run_id: &str) -> Result<Self, String> {
        let mut services = Self {
            services: Vec::new(),
        };
        for spec in specs {
            if spec.version != AgentTaskManagedService::VERSION {
                return Err(format!(
                    "managed service '{}' has unsupported version {}; supported version is {}",
                    spec.id,
                    spec.version,
                    AgentTaskManagedService::VERSION
                ));
            }
            if !safe_service_id(&spec.id) || spec.command.is_empty() {
                return Err("managed service requires a non-empty id and command argv".to_string());
            }
            if services
                .services
                .iter()
                .any(|service| service.spec.id == spec.id)
            {
                return Err(format!(
                    "managed service id '{}' is declared more than once",
                    spec.id
                ));
            }
            if let Err(error) = services.start_one(spec.clone(), run_id) {
                services.cleanup("startup_failure");
                return Err(error);
            }
        }
        Ok(services)
    }

    fn start_one(&mut self, spec: AgentTaskManagedService, run_id: &str) -> Result<(), String> {
        let log_dir = homeboy_core::paths::homeboy_data()
            .map_err(|error| error.message)?
            .join("agent-task-runs")
            .join(run_id)
            .join("services");
        std::fs::create_dir_all(&log_dir).map_err(|error| error.to_string())?;
        let log_path = log_dir.join(format!("{}.log", spec.id));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|error| format!("open managed service log: {error}"))?;
        let stderr = log
            .try_clone()
            .map_err(|error| format!("clone managed service log: {error}"))?;
        let mut spec = spec;
        if spec.socket_handoff
            && matches!(
                spec.readiness.as_ref().map(|readiness| readiness.kind),
                None | Some(AgentTaskManagedServiceReadinessKind::Tcp)
            )
        {
            return Err(format!(
                "managed service '{}' socket handoff requires HTTP readiness so payload ownership is observed",
                spec.id
            ));
        }
        let (port, port_lease, mut listener) = lease_port(&spec)?;
        spec.port = port;
        let local_url = spec.port.map(|port| format!("http://{}:{port}", spec.host));
        let launch_token = uuid::Uuid::new_v4().to_string();
        let owner_runner_id = std::env::var(homeboy_lab_runner_contract::RUNNER_ID_ENV).ok();
        let owner_runner_job_id = std::env::var("HOMEBOY_RUNNER_JOB_ID").ok();
        let mut record = AgentTaskManagedServiceRecord {
            id: spec.id.clone(),
            state: "planned".to_string(),
            launch_token: launch_token.clone(),
            local_url: local_url.clone(),
            public_url: spec.public_url.clone(),
            log_path: Some(log_path.display().to_string()),
            pid: None,
            cleanup: None,
            provenance: json!({"schema":"homeboy/agent-task-managed-service/v3", "run_id": run_id, "argv": spec.command, "cwd": spec.cwd, "host": spec.host, "port": spec.port, "target": spec.target, "lifecycle": spec.lifecycle, "socket_handoff": spec.socket_handoff, "env_allowlist": spec.env_allowlist, "secret_env": spec.secret_env, "secret_env_plan": spec.secret_env_plan.as_ref().map(|plan| plan.redacted()), "owner": { "runner_id": owner_runner_id, "runner_job_id": owner_runner_job_id }, "endpoint_ownership": { "host": spec.host, "port": spec.port, "lease": port_lease.as_ref().map(|lease| lease.path.display().to_string()) }}),
            process_identity: None,
            process_group_id: None,
            owner_runner_id,
            owner_runner_job_id,
            port_lease: port_lease
                .as_ref()
                .map(|lease| lease.path.display().to_string()),
            readiness_attempts: Vec::new(),
            browser_origin_evidence: None,
        };
        // This durable intent is committed before the execution host can run
        // the service payload, so cancellation/reconciliation sees a launch
        // even across a controller or runner interruption.
        persist_record(run_id, &record)?;
        let mut command = Command::new(&spec.command[0]);
        command
            .args(&spec.command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        // A service never receives the controller's ambient environment by
        // accident. The plan declares public inheritance and secret handoff.
        command.env_clear();
        for name in &spec.env_allowlist {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        command.envs(&spec.env);
        if let Some(plan) = &spec.secret_env_plan {
            let values = crate::agent_task_secrets::resolve_secret_env_plan(plan)
                .map_err(|error| format!("managed service '{}': {}", spec.id, error.message))?;
            command.envs(values);
        } else {
            let values = crate::agent_task_secrets::resolve_secret_env(&spec.secret_env)
                .map_err(|error| format!("managed service '{}': {}", spec.id, error.message))?;
            command.envs(values);
        }
        if let Some(port) = spec.port {
            command.env(spec.port_env.as_deref().unwrap_or("PORT"), port.to_string());
        }
        command.env("HOMEBOY_SERVICE_LAUNCH_TOKEN", &launch_token);
        handoff_listener(&mut command, listener.as_ref())?;
        let mut containment = homeboy_core::process::ProcessContainment::prepare(&mut command)
            .map_err(|error| error.message)?;
        let child = command
            .spawn()
            .map_err(|error| format!("start managed service '{}': {error}", spec.id))?;
        // The child owns the duplicated FD after exec. Keeping the supervisor's
        // copy open would let a TCP handshake succeed even when the payload
        // ignored the handed-off listener.
        drop(listener.take());
        containment.attach(&child).map_err(|error| error.message)?;
        record.state = "starting".to_string();
        record.pid = Some(child.id());
        record.process_group_id = containment.process_group_id();
        record.process_identity = process_start_identity(child.id())
            .map_err(|error| format!("inspect managed service process identity: {error}"))?;
        persist_record(run_id, &record)?;
        if let Err(error) = wait_ready(&spec, local_url.as_deref(), &mut record.readiness_attempts)
        {
            let _ = containment.terminate_on_failure_bounded(Duration::from_secs(2), false);
            record.state = "failed".to_string();
            record.cleanup = Some("terminated_after_readiness_failure".to_string());
            return Err(format!("managed service '{}': {error}", spec.id));
        }
        record.state = "ready".to_string();
        record.browser_origin_evidence = observe_browser_origin(&spec);
        persist_record(run_id, &record)?;
        self.services.push(RunningService {
            spec,
            child,
            containment,
            record,
            port_lease,
            listener,
        });
        Ok(())
    }

    pub(super) fn bind_into(&self, inputs: &mut Value, metadata: &mut Value) {
        let values = self.records().into_iter().map(|record| (record.id.clone(), json!({
            "local_url": record.local_url, "public_url": record.public_url,
            "browser_origin_probe": record.browser_origin_evidence,
            "lease_ref": format!("managed-service:{}", record.id),
            "readiness_attempts": record.readiness_attempts,
            "endpoint_ownership": record.provenance["endpoint_ownership"],
            "service_owner": { "pid": record.pid, "process_group_id": record.process_group_id, "runner_id": record.owner_runner_id, "runner_job_id": record.owner_runner_job_id },
        }))).collect::<serde_json::Map<_, _>>();
        if !inputs.is_object() {
            *inputs = json!({});
        }
        inputs["services"] = Value::Object(values.clone());
        if !metadata.is_object() {
            *metadata = json!({});
        }
        metadata["managed_services"] = Value::Object(values);
    }

    pub(super) fn records(&self) -> Vec<AgentTaskManagedServiceRecord> {
        self.services
            .iter()
            .map(|service| service.record.clone())
            .collect()
    }

    pub(super) fn cleanup(mut self, reason: &str) -> Vec<AgentTaskManagedServiceRecord> {
        for service in &mut self.services {
            let exited = service.child.try_wait().ok().flatten().is_some();
            let cleanup = if exited {
                // A daemonized child can outlive its direct service leader.
                // ProcessContainment owns the platform-specific scope marker
                // needed to reap those descendants after a normal leader exit.
                service
                    .containment
                    .cleanup_after_leader_exit_bounded(Duration::from_secs(2))
            } else {
                service
                    .containment
                    .terminate_on_failure_bounded(Duration::from_secs(2), false)
            };
            let _ = service.child.wait();
            service.record.state = "stopped".to_string();
            service.record.cleanup = Some(match cleanup {
                Ok(()) => format!("cleaned_up:{reason}"),
                Err(error) => format!("cleanup_failed:{reason}:{}", error.message),
            });
            if let Some(run_id) = service
                .record
                .provenance
                .get("run_id")
                .and_then(Value::as_str)
            {
                let _ = persist_record(run_id, &service.record);
            }
        }
        self.records()
    }
}

fn observe_browser_origin(spec: &AgentTaskManagedService) -> Option<Value> {
    let probe = spec.browser_origin_probe.as_ref()?;
    let requested_url = probe.url.as_deref().or(spec.public_url.as_deref())?;
    match reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .and_then(|client| client.get(requested_url).send())
    {
        Ok(response) => {
            let observed_url = response.url().to_string();
            let origin = response.url().origin().ascii_serialization();
            Some(json!({
                "schema": "homeboy/browser-origin-probe/v1",
                "provider": probe.provider,
                "requested_url": requested_url,
                "observed_url": observed_url,
                "origin": origin,
                "status": response.status().as_u16(),
            }))
        }
        Err(error) => Some(json!({
            "schema": "homeboy/browser-origin-probe/v1",
            "provider": probe.provider,
            "requested_url": requested_url,
            "error": error.to_string(),
        })),
    }
}

/// Scheduler-side handle. In production it owns no payload process: it only
/// invokes and polls the durable worker. The local variant keeps hermetic unit
/// fixtures independent of the compiled CLI binary.
pub(crate) enum ManagedServices {
    Local(AgentTaskServiceSupervisor),
    Worker { run_id: String },
}

impl ManagedServices {
    #[cfg(test)]
    pub(super) fn start(specs: &[AgentTaskManagedService], run_id: &str) -> Result<Self, String> {
        AgentTaskServiceSupervisor::start(specs, run_id).map(Self::Local)
    }

    #[cfg(not(test))]
    pub(super) fn start(specs: &[AgentTaskManagedService], run_id: &str) -> Result<Self, String> {
        let request = AgentTaskServiceWorkerRequest {
            schema: AgentTaskServiceWorkerRequest::SCHEMA.to_string(),
            operation: "start".to_string(),
            run_id: run_id.to_string(),
            services: specs.to_vec(),
            parent_pid: std::process::id(),
            parent_ttl_ms: default_worker_ttl_ms(),
        };
        let request_path = service_worker_request_path(run_id)?;
        write_json_atomically(&request_path, &request)?;
        let worker = std::env::var_os("HOMEBOY_SERVICE_SUPERVISOR_WORKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_exe().expect("current Homeboy executable"));
        Command::new(worker)
            .args(["self", "service-supervisor-worker", "--request"])
            .arg(&request_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start managed service worker: {error}"))?;
        for _ in 0..600 {
            if let Some(state) = read_service_worker_state(run_id)? {
                match state.state.as_str() {
                    "ready" => {
                        return Ok(Self::Worker {
                            run_id: run_id.to_string(),
                        })
                    }
                    "failed" => {
                        return Err(state
                            .detail
                            .unwrap_or_else(|| "managed service worker failed".to_string()))
                    }
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("managed service worker did not become ready".to_string())
    }

    pub(super) fn bind_into(&self, inputs: &mut Value, metadata: &mut Value) {
        let records = self.records();
        let values = records.into_iter().map(|record| (record.id.clone(), json!({
            "local_url": record.local_url, "public_url": record.public_url,
            "browser_origin_probe": record.browser_origin_evidence,
            "lease_ref": format!("managed-service:{}", record.id),
            "readiness_attempts": record.readiness_attempts,
            "endpoint_ownership": record.provenance["endpoint_ownership"],
            "service_owner": { "pid": record.pid, "process_group_id": record.process_group_id, "runner_id": record.owner_runner_id, "runner_job_id": record.owner_runner_job_id },
        }))).collect::<serde_json::Map<_, _>>();
        if !inputs.is_object() {
            *inputs = json!({});
        }
        inputs["services"] = Value::Object(values.clone());
        if !metadata.is_object() {
            *metadata = json!({});
        }
        metadata["managed_services"] = Value::Object(values);
    }

    pub(super) fn records(&self) -> Vec<AgentTaskManagedServiceRecord> {
        match self {
            Self::Local(supervisor) => supervisor.records(),
            Self::Worker { run_id } => read_service_worker_state(run_id)
                .ok()
                .flatten()
                .map(|state| state.services)
                .unwrap_or_default(),
        }
    }

    pub(super) fn cleanup(self, reason: &str) -> Vec<AgentTaskManagedServiceRecord> {
        match self {
            Self::Local(supervisor) => supervisor.cleanup(reason),
            Self::Worker { run_id } => {
                if let Ok(Some(mut state)) = read_service_worker_state(&run_id) {
                    state.state = "stop_requested".to_string();
                    state.heartbeat_unix_ms = now_unix_ms();
                    let _ = write_json_atomically(
                        &service_worker_state_path(&run_id).unwrap_or_default(),
                        &state,
                    );
                }
                for _ in 0..600 {
                    if let Ok(Some(state)) = read_service_worker_state(&run_id) {
                        if state.state == "stopped" {
                            return state.services;
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                reconcile_run_services(&run_id, reason).unwrap_or_default()
            }
        }
    }
}

fn lease_port(spec: &AgentTaskManagedService) -> Result<PortLeaseAllocation, String> {
    let Some(requested) = spec.port else {
        return Ok((None, None, None));
    };
    if requested == 0 && !spec.socket_handoff {
        return Err(format!(
            "managed service '{}' dynamic ports require socket_handoff",
            spec.id
        ));
    }
    let listener = if spec.socket_handoff {
        Some(
            TcpListener::bind((spec.host.as_str(), requested)).map_err(|error| {
                format!(
                    "managed service '{}' port allocation collision on {}:{requested}: {error}",
                    spec.id, spec.host
                )
            })?,
        )
    } else {
        None
    };
    let port = listener
        .as_ref()
        .map(|listener| {
            listener
                .local_addr()
                .map(|address| address.port())
                .map_err(|error| format!("read managed service port: {error}"))
        })
        .transpose()?
        .unwrap_or(requested);
    let lease_dir = homeboy_core::paths::homeboy_data()
        .map_err(|error| error.message)?
        .join("agent-task-service-ports");
    std::fs::create_dir_all(&lease_dir)
        .map_err(|error| format!("create managed service lease directory: {error}"))?;
    let host = spec
        .host
        .replace(|character: char| !character.is_ascii_alphanumeric(), "_");
    let path = lease_dir.join(format!("{host}-{port}.lease"));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| {
            format!(
                "managed service '{}' port allocation collision on {}:{port}",
                spec.id, spec.host
            )
        })?;
    Ok((Some(port), Some(PortLease { path, _file: file }), listener))
}

fn safe_service_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn handoff_listener(command: &mut Command, listener: Option<&TcpListener>) -> Result<(), String> {
    let Some(listener) = listener else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let descriptor = unsafe { libc::dup(listener.as_raw_fd()) };
        if descriptor < 0 {
            return Err(format!(
                "duplicate service listener: {}",
                std::io::Error::last_os_error()
            ));
        }
        command.env("HOMEBOY_LISTEN_FD", "3");
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(descriptor, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if descriptor != 3 {
                    libc::close(descriptor);
                }
                Ok(())
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        Err("service socket handoff is unsupported on this execution host".to_string())
    }
}

fn persist_record(run_id: &str, record: &AgentTaskManagedServiceRecord) -> Result<(), String> {
    let path = homeboy_core::paths::homeboy_data()
        .map_err(|error| error.message)?
        .join("agent-task-runs")
        .join(run_id)
        .join("services")
        .join(format!("{}.json", record.id));
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("persist managed service ownership: {error}"))?;
    std::fs::rename(temporary, path)
        .map_err(|error| format!("commit managed service ownership: {error}"))
}

fn worker_root(run_id: &str) -> Result<PathBuf, String> {
    Ok(homeboy_core::paths::homeboy_data()
        .map_err(|error| error.message)?
        .join("agent-task-runs")
        .join(run_id)
        .join("service-supervisor"))
}

pub fn service_worker_request_path(run_id: &str) -> Result<PathBuf, String> {
    Ok(worker_root(run_id)?.join("request.json"))
}

pub fn service_worker_state_path(run_id: &str) -> Result<PathBuf, String> {
    Ok(worker_root(run_id)?.join("state.json"))
}

fn write_json_atomically<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| "worker path has no parent".to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        pid > 0
            && pid <= i32::MAX as u32
            && unsafe {
                libc::kill(pid as libc::pid_t, 0) == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            }
    }
    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}

fn parent_process_identity(pid: u32) -> Result<Option<ProcessStartIdentity>, String> {
    if pid == 0 {
        return Ok(None);
    }
    process_start_identity(pid).map_err(|error| error.to_string())
}

pub fn read_service_worker_state(
    run_id: &str,
) -> Result<Option<AgentTaskServiceWorkerState>, String> {
    match std::fs::read(service_worker_state_path(run_id)?) {
        Ok(raw) => {
            let state: AgentTaskServiceWorkerState =
                serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
            (state.schema == AgentTaskServiceWorkerState::SCHEMA)
                .then_some(state)
                .ok_or_else(|| "unsupported service worker state schema".to_string())
                .map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Run an internal service supervisor operation. A `start` invocation stays
/// resident and owns payload creation, readiness, logs, and containment.
pub fn run_service_worker(request_path: &Path) -> Result<(), String> {
    let request: AgentTaskServiceWorkerRequest =
        serde_json::from_slice(&std::fs::read(request_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if request.schema != AgentTaskServiceWorkerRequest::SCHEMA {
        return Err("unsupported service worker request schema".to_string());
    }
    let state_path = service_worker_state_path(&request.run_id)?;
    match request.operation.as_str() {
        "status" => return read_service_worker_state(&request.run_id).map(|_| ()),
        "stop" => {
            let mut state = read_service_worker_state(&request.run_id)?
                .ok_or_else(|| "service worker state is absent".to_string())?;
            state.state = "stop_requested".to_string();
            state.heartbeat_unix_ms = now_unix_ms();
            return write_json_atomically(&state_path, &state);
        }
        "reconcile" => {
            let records = reconcile_run_services(&request.run_id, "worker_reconcile")?;
            return write_json_atomically(
                &state_path,
                &AgentTaskServiceWorkerState {
                    schema: AgentTaskServiceWorkerState::SCHEMA.to_string(),
                    run_id: request.run_id,
                    state: "stopped".to_string(),
                    worker_pid: std::process::id(),
                    worker_identity: process_start_identity(std::process::id())
                        .map_err(|error| error.to_string())?,
                    parent_pid: request.parent_pid,
                    parent_identity: parent_process_identity(request.parent_pid)?,
                    heartbeat_unix_ms: now_unix_ms(),
                    services: records,
                    detail: Some("reconciled".to_string()),
                },
            );
        }
        "start" => {}
        _ => {
            return Err(
                "service worker operation must be start, status, stop, or reconcile".to_string(),
            )
        }
    }
    let mut state = AgentTaskServiceWorkerState {
        schema: AgentTaskServiceWorkerState::SCHEMA.to_string(),
        run_id: request.run_id.clone(),
        state: "starting".to_string(),
        worker_pid: std::process::id(),
        worker_identity: process_start_identity(std::process::id())
            .map_err(|error| error.to_string())?,
        parent_pid: request.parent_pid,
        parent_identity: parent_process_identity(request.parent_pid)?,
        heartbeat_unix_ms: now_unix_ms(),
        services: Vec::new(),
        detail: None,
    };
    write_json_atomically(&state_path, &state)?;
    let supervisor = match AgentTaskServiceSupervisor::start(&request.services, &request.run_id) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            state.state = "failed".to_string();
            state.detail = Some(error);
            state.heartbeat_unix_ms = now_unix_ms();
            return write_json_atomically(&state_path, &state);
        }
    };
    state.state = "ready".to_string();
    state.services = supervisor.records();
    state.heartbeat_unix_ms = now_unix_ms();
    write_json_atomically(&state_path, &state)?;
    let started_unix_ms = state.heartbeat_unix_ms;
    loop {
        thread::sleep(Duration::from_millis(50));
        let requested_stop = read_service_worker_state(&request.run_id)?
            .is_some_and(|latest| latest.state == "stop_requested");
        // PID liveness is authoritative when a scheduler process exists. A
        // process-less handoff (parent PID 0) is explicitly bounded by TTL.
        let parent_lost = if request.parent_pid == 0 {
            now_unix_ms().saturating_sub(started_unix_ms) >= request.parent_ttl_ms
        } else {
            !matches!(
                process_identity_state_with_start_identity(
                    request.parent_pid,
                    None,
                    state.parent_identity.as_ref(),
                ),
                ProcessIdentityState::Live
            )
        };
        if requested_stop || parent_lost {
            state.services = supervisor.cleanup(if requested_stop {
                "stop"
            } else {
                "parent_lost"
            });
            state.state = "stopped".to_string();
            state.heartbeat_unix_ms = now_unix_ms();
            state.detail = Some(
                if requested_stop {
                    "stopped by request"
                } else {
                    "parent lost"
                }
                .to_string(),
            );
            return write_json_atomically(&state_path, &state);
        }
        state.heartbeat_unix_ms = now_unix_ms();
        write_json_atomically(&state_path, &state)?;
    }
}

/// Invoke a non-resident worker operation on its execution host. This keeps
/// controller recovery transport-independent: the caller only needs the run id
/// and asks the owning runner to resolve its own state directory.
pub fn run_service_worker_operation(run_id: &str, operation: &str) -> Result<(), String> {
    let request = AgentTaskServiceWorkerRequest {
        schema: AgentTaskServiceWorkerRequest::SCHEMA.to_string(),
        operation: operation.to_string(),
        run_id: run_id.to_string(),
        services: Vec::new(),
        parent_pid: 0,
        parent_ttl_ms: default_worker_ttl_ms(),
    };
    let request_path = service_worker_request_path(run_id)?;
    write_json_atomically(&request_path, &request)?;
    run_service_worker(&request_path)
}

/// Request cleanup from the execution host recorded by the controller handoff.
/// The runner command uses its own HOME/data root, so no runner-local path is
/// ever interpreted by the controller.
pub(crate) fn reconcile_run_services_on_owner(
    run_id: &str,
    owner: Option<&Value>,
    reason: &str,
) -> Result<Value, String> {
    let Some(owner) = owner else {
        return Ok(
            json!({ "transport": "local", "services": reconcile_run_services(run_id, reason)? }),
        );
    };
    let Some(runner_id) = owner.get("runner_id").and_then(Value::as_str) else {
        return Ok(
            json!({ "transport": "local", "services": reconcile_run_services(run_id, reason)? }),
        );
    };
    let command = |operation: &str| {
        vec![
            "self".to_string(),
            "service-supervisor-worker".to_string(),
            "--run-id".to_string(),
            run_id.to_string(),
            "--operation".to_string(),
            operation.to_string(),
        ]
    };
    let cwd = owner
        .get("remote_workspace")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let status = crate::agent_task_lifecycle::with_runner_continuation(|provider| {
        provider.run_continuation_exec(runner_id, cwd, &command("status"), run_id)
    })
    .map_err(|error| error.message)?;
    let stop = crate::agent_task_lifecycle::with_runner_continuation(|provider| {
        provider.run_continuation_exec(runner_id, cwd, &command("stop"), run_id)
    })
    .map_err(|error| error.message)?;
    let reconcile = crate::agent_task_lifecycle::with_runner_continuation(|provider| {
        provider.run_continuation_exec(runner_id, cwd, &command("reconcile"), run_id)
    })
    .map_err(|error| error.message)?;
    Ok(json!({
        "transport": "runner_command",
        "runner_id": runner_id,
        "state_ref": owner.get("state_ref").cloned().unwrap_or(Value::Null),
        "status_exit_code": status,
        "stop_exit_code": stop,
        "reconcile_exit_code": reconcile,
        "reason": reason,
    }))
}

/// Reap service leaders left by an interrupted controller. The persisted kernel
/// process-start identity prevents a recycled PID from ever being signalled.
pub(crate) fn reconcile_run_services(
    run_id: &str,
    reason: &str,
) -> Result<Vec<AgentTaskManagedServiceRecord>, String> {
    let directory = homeboy_core::paths::homeboy_data()
        .map_err(|error| error.message)?
        .join("agent-task-runs")
        .join(run_id)
        .join("services");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read managed service ledger: {error}")),
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(mut record) = serde_json::from_slice::<AgentTaskManagedServiceRecord>(&bytes) else {
            continue;
        };
        if matches!(record.state.as_str(), "stopped" | "failed") {
            records.push(record);
            continue;
        }
        let outcome = match (record.pid, record.process_identity.as_ref()) {
            (Some(pid), identity) => {
                match process_identity_state_with_start_identity(pid, None, identity) {
                    ProcessIdentityState::Live => terminate_process_tree(pid)
                        .map(|_| "reaped".to_string())
                        .unwrap_or_else(|error| format!("cleanup_failed:{error}")),
                    ProcessIdentityState::Dead => "already_exited".to_string(),
                    ProcessIdentityState::IdentityMismatch => {
                        "ownership_mismatch_not_signalled".to_string()
                    }
                    ProcessIdentityState::Unverifiable => {
                        "ownership_unverifiable_not_signalled".to_string()
                    }
                }
            }
            _ => "no_process_identity".to_string(),
        };
        // A daemon can deliberately exit its leader while retaining descendants.
        // The pre-exec process-group identity is the durable containment boundary
        // for that case and is safe to signal independently of PID reuse.
        let outcome = match (record.process_group_id, outcome.as_str()) {
            (Some(group), "already_exited" | "no_process_identity") => {
                match homeboy_core::process::isolated_process_group_is_running(group) {
                    Ok(true) => homeboy_core::process::terminate_isolated_process_group(group)
                        .map(|_| "reaped_process_group".to_string())
                        .unwrap_or_else(|error| format!("cleanup_failed:{error}")),
                    Ok(false) => outcome,
                    Err(error) => format!("cleanup_unverifiable:{error}"),
                }
            }
            _ => outcome,
        };
        record.state = "stopped".to_string();
        record.cleanup = Some(format!("{outcome}:{reason}"));
        persist_record(run_id, &record)?;
        records.push(record);
    }
    Ok(records)
}

fn wait_ready(
    spec: &AgentTaskManagedService,
    local_url: Option<&str>,
    attempts: &mut Vec<Value>,
) -> Result<(), String> {
    let Some(port) = spec.port else {
        return Ok(());
    };
    let readiness = spec.readiness.as_ref();
    let timeout = readiness
        .and_then(|probe| probe.timeout_ms)
        .unwrap_or(10_000);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    loop {
        let ready = match readiness
            .map(|probe| probe.kind)
            .unwrap_or(AgentTaskManagedServiceReadinessKind::Tcp)
        {
            AgentTaskManagedServiceReadinessKind::Tcp => {
                TcpStream::connect((spec.host.as_str(), port)).is_ok()
            }
            AgentTaskManagedServiceReadinessKind::Http => local_url
                .and_then(|url| {
                    readiness
                        .and_then(|probe| probe.path.as_deref())
                        .map(|path| format!("{url}{path}"))
                })
                .map(|url| {
                    reqwest::blocking::get(url)
                        .map(|response| response.status().is_success())
                        .unwrap_or(false)
                })
                .unwrap_or(false),
        };
        if ready {
            attempts.push(json!({"status":"ready", "at":"observed", "endpoint": local_url}));
            return Ok(());
        }
        attempts.push(json!({"status":"not_ready", "endpoint": local_url}));
        if Instant::now() >= deadline {
            return Err(format!("readiness probe timed out after {timeout}ms"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use homeboy_core::test_support::with_isolated_home;

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        listener.local_addr().expect("local address").port()
    }

    fn fixture(port: u16) -> AgentTaskManagedService {
        AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "fixture".to_string(),
            command: vec![
                "python3".to_string(), "-u".to_string(), "-c".to_string(),
                "import socket,http.server; s=socket.fromfd(3,socket.AF_INET,socket.SOCK_STREAM); h=http.server.HTTPServer(('127.0.0.1',0),http.server.SimpleHTTPRequestHandler,False); h.socket=s; h.server_address=s.getsockname(); h.server_activate(); h.serve_forever()".to_string(),
            ],
            cwd: None,
            env: HashMap::from([("PORT".to_string(), port.to_string())]),
            env_allowlist: vec!["PATH".to_string()],
            secret_env: Vec::new(),
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: Some(port),
            port_env: Some("PORT".to_string()),
            socket_handoff: true,
            readiness: Some(AgentTaskManagedServiceReadiness {
                kind: AgentTaskManagedServiceReadinessKind::Http,
                path: Some("/".to_string()), timeout_ms: Some(5_000),
            }),
            public_url: Some("https://preview.example.test/fixture".to_string()),
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        }
    }

    #[test]
    fn neutral_http_fixture_binds_provenance_and_is_cleaned_up() {
        with_isolated_home(|_| {
            let services = ManagedServices::start(&[fixture(free_port())], "fixture-run")
                .expect("start fixture");
            let mut inputs = Value::Null;
            let mut metadata = Value::Null;
            services.bind_into(&mut inputs, &mut metadata);
            assert!(inputs["services"]["fixture"]["local_url"]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:"));
            assert_eq!(
                inputs["services"]["fixture"]["public_url"],
                "https://preview.example.test/fixture"
            );
            assert_eq!(
                metadata["managed_services"]["fixture"]["browser_origin_probe"],
                Value::Null
            );
            let records = services.cleanup("success");
            assert_eq!(records[0].state, "stopped");
            assert!(!records[0].launch_token.is_empty());
            assert!(records[0].process_identity.is_some());
            assert!(records[0].process_group_id.is_some());
            assert_eq!(records[0].cleanup.as_deref(), Some("cleaned_up:success"));
            assert!(std::path::Path::new(records[0].log_path.as_deref().unwrap()).exists());
            assert_eq!(
                records[0].readiness_attempts.last().unwrap()["status"],
                "ready"
            );
        });
    }

    #[test]
    fn readiness_failure_reaps_any_service_started_earlier() {
        with_isolated_home(|_| {
            let mut failing = fixture(free_port());
            failing.id = "never-ready".to_string();
            failing.command = vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()];
            failing.readiness.as_mut().unwrap().timeout_ms = Some(50);
            let error =
                match ManagedServices::start(&[fixture(free_port()), failing], "failure-run") {
                    Ok(_) => panic!("readiness should fail"),
                    Err(error) => error,
                };
            assert!(error.contains("never-ready"));
        });
    }

    #[test]
    fn rejects_a_second_live_lease_for_the_same_port() {
        with_isolated_home(|_| {
            let port = free_port();
            let first =
                ManagedServices::start(&[fixture(port)], "lease-first").expect("first lease");
            let mut second = fixture(port);
            second.id = "second".to_string();
            let error = match ManagedServices::start(&[second], "lease-second") {
                Ok(_) => panic!("port lease collision"),
                Err(error) => error,
            };
            assert!(error.contains("port allocation collision"));
            first.cleanup("test");
        });
    }

    #[test]
    fn rejects_unsafe_service_ids_and_dynamic_ports_without_handoff() {
        with_isolated_home(|_| {
            let mut unsafe_id = fixture(0);
            unsafe_id.id = "../escape".to_string();
            assert!(ManagedServices::start(&[unsafe_id], "safe-id").is_err());

            let mut unowned_socket = fixture(0);
            unowned_socket.socket_handoff = false;
            let error = match ManagedServices::start(&[unowned_socket], "socket-required") {
                Ok(_) => panic!("dynamic port must stay owned by the supervisor"),
                Err(error) => error,
            };
            assert!(error.contains("socket_handoff"));

            let mut tcp_only = fixture(free_port());
            tcp_only.readiness = None;
            let error = match ManagedServices::start(&[tcp_only], "tcp-payload-required") {
                Ok(_) => panic!("socket handoff must observe the payload"),
                Err(error) => error,
            };
            assert!(error.contains("requires HTTP readiness"));
        });
    }

    fn worker_request(
        run_id: &str,
        operation: &str,
        services: Vec<AgentTaskManagedService>,
        parent_pid: u32,
    ) -> PathBuf {
        let request = AgentTaskServiceWorkerRequest {
            schema: AgentTaskServiceWorkerRequest::SCHEMA.to_string(),
            operation: operation.to_string(),
            run_id: run_id.to_string(),
            services,
            parent_pid,
            parent_ttl_ms: 10,
        };
        let path = service_worker_request_path(run_id).expect("worker request path");
        write_json_atomically(&path, &request).expect("write worker request");
        path
    }

    fn wait_for_worker_state(run_id: &str, expected: &str) -> AgentTaskServiceWorkerState {
        for _ in 0..200 {
            if let Some(state) = read_service_worker_state(run_id).expect("read worker state") {
                if state.state == expected {
                    return state;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("worker did not reach {expected}");
    }

    #[test]
    fn worker_persists_identity_before_a_payload_spawn_failure() {
        with_isolated_home(|_| {
            let mut broken = fixture(free_port());
            broken.command = vec!["homeboy-command-that-does-not-exist".to_string()];
            let request =
                worker_request("payload-crash", "start", vec![broken], std::process::id());
            run_service_worker(&request).expect("worker records startup failure");
            let state = wait_for_worker_state("payload-crash", "failed");
            assert!(
                state.worker_identity.is_some(),
                "worker identity precedes payload spawn"
            );
            assert!(state
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("start managed service"));
        });
    }

    #[test]
    fn worker_stop_operation_publishes_terminal_service_state() {
        with_isolated_home(|_| {
            let request = worker_request(
                "worker-stop",
                "start",
                vec![fixture(free_port())],
                std::process::id(),
            );
            let worker = thread::spawn(move || run_service_worker(&request));
            let state = wait_for_worker_state("worker-stop", "ready");
            assert_eq!(state.services[0].state, "ready");
            let stop = worker_request("worker-stop", "stop", Vec::new(), std::process::id());
            run_service_worker(&stop).expect("stop operation");
            worker.join().expect("worker joins").expect("worker stops");
            let stopped = wait_for_worker_state("worker-stop", "stopped");
            assert_eq!(stopped.services[0].state, "stopped");
        });
    }

    #[cfg(unix)]
    #[test]
    fn worker_self_cleans_when_its_scheduler_parent_is_gone() {
        with_isolated_home(|_| {
            let request = worker_request(
                "orphaned-worker",
                "start",
                vec![fixture(free_port())],
                999_999,
            );
            run_service_worker(&request).expect("worker exits after parent loss");
            let stopped = wait_for_worker_state("orphaned-worker", "stopped");
            assert_eq!(stopped.detail.as_deref(), Some("parent lost"));
            assert_eq!(stopped.services[0].state, "stopped");
        });
    }

    #[test]
    fn processless_worker_handoff_expires_at_its_ttl() {
        with_isolated_home(|_| {
            let request = worker_request("ttl-worker", "start", Vec::new(), 0);
            run_service_worker(&request).expect("worker expires processless handoff");
            assert_eq!(
                wait_for_worker_state("ttl-worker", "stopped")
                    .detail
                    .as_deref(),
                Some("parent lost")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn stale_reconcile_reaps_daemonized_descendants_by_persisted_process_group() {
        with_isolated_home(|_| {
            let service = AgentTaskManagedService {
                version: AgentTaskManagedService::VERSION,
                id: "daemon".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "sleep 30 & exit 0".to_string(),
                ],
                cwd: None,
                env: HashMap::new(),
                env_allowlist: vec!["PATH".to_string()],
                secret_env: Vec::new(),
                secret_env_plan: None,
                host: "127.0.0.1".to_string(),
                port: None,
                port_env: None,
                socket_handoff: false,
                readiness: None,
                public_url: None,
                browser_origin_probe: None,
                lifecycle: AgentTaskManagedServiceLifecycle::Plan,
                target: None,
            };
            let services = ManagedServices::start(&[service], "orphaned-daemon")
                .expect("start daemonizing service");
            let group = services.records()[0]
                .process_group_id
                .expect("persisted process group");
            // Simulate controller loss: the ledger survives but no in-memory
            // supervisor is available to reap the daemonized descendant.
            std::mem::forget(services);
            std::thread::sleep(Duration::from_millis(100));

            let records = reconcile_run_services("orphaned-daemon", "stale_controller")
                .expect("reconcile persisted service ledger");
            assert_eq!(records[0].state, "stopped");
            assert!(records[0]
                .cleanup
                .as_deref()
                .unwrap_or_default()
                .contains("reaped_process_group"));
            assert!(
                !homeboy_core::process::isolated_process_group_is_running(group)
                    .expect("inspect process group")
            );
        });
    }
}
