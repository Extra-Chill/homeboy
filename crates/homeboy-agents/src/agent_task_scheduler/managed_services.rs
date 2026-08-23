//! Plan-owned, runner-local process services for agent-task execution.
//!
//! This deliberately owns only process allocation/readiness and lifecycle
//! evidence. Public URLs are opaque references supplied by a preview/tunnel
//! provider, keeping this contract independent of any product integration.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use homeboy_core::process::{
    process_identity_state_with_start_identity, process_start_identity,
    terminate_isolated_process_group_with_grace, terminate_process_tree_with_grace,
    ProcessIdentityState, ProcessStartIdentity,
};
use serde_json::{json, Value};

use super::{AgentTaskEvidenceRef, AgentTaskManagedService, AgentTaskManagedServiceReadinessKind};

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
    /// The supervisor combines process stdout and stderr in one retained file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_uri: Option<String>,
    pub pid: Option<u32>,
    pub cleanup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_cleanup_deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_outcome: Option<String>,
    /// Set only after this supervisor successfully waits for the service leader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_reaped: Option<bool>,
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

const WORKER_CLEANUP_COORDINATION_MARGIN: Duration = Duration::from_secs(1);
const WORKER_STARTUP_COORDINATION_MARGIN: Duration = Duration::from_secs(6);

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
            spec.validate_cleanup_deadline()?;
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
            stdout_uri: Some(format!("file://{}", log_path.display())),
            stderr_uri: Some(format!("file://{}", log_path.display())),
            pid: None,
            cleanup: None,
            requested_cleanup_deadline_ms: Some(spec.cleanup_deadline_ms),
            cleanup_outcome: None,
            cleanup_reaped: None,
            provenance: json!({"schema":"homeboy/agent-task-managed-service/v3", "run_id": run_id, "argv": spec.command, "cwd": spec.cwd, "host": spec.host, "port": spec.port, "target": spec.target, "lifecycle": spec.lifecycle, "socket_handoff": spec.socket_handoff, "cleanup_deadline_ms": spec.cleanup_deadline_ms, "env_allowlist": spec.env_allowlist, "secret_env": spec.secret_env, "secret_env_plan": spec.secret_env_plan.as_ref().map(|plan| plan.redacted()), "owner": { "runner_id": owner_runner_id, "runner_job_id": owner_runner_job_id }, "endpoint_ownership": { "host": spec.host, "port": spec.port, "lease": port_lease.as_ref().map(|lease| lease.path.display().to_string()) }}),
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
            let cleanup = containment
                .cleanup_with_grace(Duration::from_millis(spec.cleanup_deadline_ms), false);
            // A complete containment cleanup verifies its owned leader and
            // descendants before returning.
            let reaped = cleanup.as_ref().is_ok_and(|cleanup| cleanup.complete);
            record.state = "failed".to_string();
            record.cleanup = Some("terminated_after_readiness_failure".to_string());
            record.cleanup_outcome = Some(cleanup_outcome(cleanup));
            record.cleanup_reaped = Some(reaped);
            let _ = persist_record(run_id, &record);
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

    pub(super) fn records(&self) -> Vec<AgentTaskManagedServiceRecord> {
        self.services
            .iter()
            .map(|service| service.record.clone())
            .collect()
    }

    pub(super) fn cleanup(mut self, reason: &str) -> Vec<AgentTaskManagedServiceRecord> {
        // Each containment boundary is independent. Terminate them together so
        // total cleanup is bounded by the longest declared grace period, then
        // persist ordered results here to avoid ledger write races.
        let cleanups = thread::scope(|scope| {
            self.services
                .iter_mut()
                .map(|service| {
                    scope.spawn(move || {
                        let exited = service.child.try_wait().ok().flatten().is_some();
                        let cleanup = service.containment.cleanup_with_grace(
                            Duration::from_millis(service.spec.cleanup_deadline_ms),
                            exited,
                        );
                        let child_reaped = service.child.wait().is_ok();
                        let reaped =
                            cleanup.as_ref().is_ok_and(|cleanup| cleanup.complete) || child_reaped;
                        (cleanup, reaped)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        (
                            Err(homeboy_core::Error::internal_unexpected(
                                "managed service cleanup worker panicked",
                            )),
                            false,
                        )
                    })
                })
                .collect::<Vec<_>>()
        });
        for (service, (cleanup, reaped)) in self.services.iter_mut().zip(cleanups) {
            service.record.state = "stopped".to_string();
            service.record.cleanup_outcome = Some(match &cleanup {
                Ok(cleanup) if !cleanup.complete => format!(
                    "incomplete:{}",
                    cleanup.detail.as_deref().unwrap_or("unknown")
                ),
                Ok(cleanup) if cleanup.forced => "deadline_escalation_forced".to_string(),
                Ok(_) => "graceful".to_string(),
                Err(_) => "failed".to_string(),
            });
            service.record.cleanup = Some(match cleanup {
                Ok(cleanup) if cleanup.complete => format!("cleaned_up:{reason}"),
                Ok(cleanup) => format!(
                    "cleanup_incomplete:{reason}:{}",
                    cleanup.detail.unwrap_or_else(|| "unknown".to_string())
                ),
                Err(error) => format!("cleanup_failed:{reason}:{}", error.message),
            });
            service.record.cleanup_reaped = Some(reaped);
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

fn cleanup_outcome(
    cleanup: Result<homeboy_core::process::ProcessContainmentCleanup, homeboy_core::Error>,
) -> String {
    match cleanup {
        Ok(cleanup) if !cleanup.complete => format!(
            "incomplete:{}",
            cleanup.detail.unwrap_or_else(|| "unknown".to_string())
        ),
        Ok(cleanup) if cleanup.forced => "deadline_escalation_forced".to_string(),
        Ok(_) => "graceful".to_string(),
        Err(_) => "failed".to_string(),
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
        // A plan that declares no services has nothing to supervise. Spawning a
        // durable worker anyway costs a process and then blocks the scheduler
        // for up to six seconds waiting for a readiness state that describes an
        // empty set -- on every run, and most runs declare no services.
        //
        // It also fails outright in any host whose `current_exe()` is not the
        // Homeboy CLI. An integration test binary spawned as
        // `self service-supervisor-worker --request <path>` reads those as
        // libtest filters, runs no tests, exits, and never writes a state file,
        // so the poll runs to exhaustion and the plan fails before reaching its
        // first step.
        if specs.is_empty() {
            return AgentTaskServiceSupervisor::start(specs, run_id).map(Self::Local);
        }
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
        wait_for_service_worker_start(specs, run_id)
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
                let initial_state = read_service_worker_state(&run_id).ok().flatten();
                if let Some(mut state) = initial_state.clone() {
                    state.state = "stop_requested".to_string();
                    state.heartbeat_unix_ms = now_unix_ms();
                    let _ = write_json_atomically(
                        &service_worker_state_path(&run_id).unwrap_or_default(),
                        &state,
                    );
                }
                let wait_budget = initial_state
                    .as_ref()
                    .map(|state| worker_cleanup_wait_budget(&state.services))
                    .transpose();
                let Ok(wait_budget) = wait_budget else {
                    // The worker remains the only owner for a requested stop.
                    // A corrupt ledger must not cause controller-side concurrent
                    // reconciliation against a potentially live worker.
                    return initial_state
                        .map(|state| state.services)
                        .unwrap_or_default();
                };
                let deadline =
                    Instant::now() + wait_budget.unwrap_or(WORKER_CLEANUP_COORDINATION_MARGIN);
                while Instant::now() < deadline {
                    if let Ok(Some(state)) = read_service_worker_state(&run_id) {
                        if state.state == "stopped" {
                            return state.services;
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                // A late worker owns the processes until an independently
                // proven interruption triggers reconciliation elsewhere.
                let _ = reason;
                read_service_worker_state(&run_id)
                    .ok()
                    .flatten()
                    .map(|state| state.services)
                    .unwrap_or_default()
            }
        }
    }
}

fn wait_for_service_worker_start(
    specs: &[AgentTaskManagedService],
    run_id: &str,
) -> Result<ManagedServices, String> {
    let deadline = Instant::now() + worker_start_wait_budget(specs);
    while Instant::now() < deadline {
        if let Some(state) = read_service_worker_state(run_id)? {
            match state.state.as_str() {
                "ready" => {
                    return Ok(ManagedServices::Worker {
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

fn worker_cleanup_wait_budget(
    records: &[AgentTaskManagedServiceRecord],
) -> Result<Duration, String> {
    let cleanup_ms = records
        .iter()
        .map(requested_cleanup_deadline)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    Ok(Duration::from_millis(cleanup_ms).saturating_add(WORKER_CLEANUP_COORDINATION_MARGIN))
}

fn worker_start_wait_budget(specs: &[AgentTaskManagedService]) -> Duration {
    let readiness_ms = specs.iter().fold(0_u64, |total, spec| {
        if spec.port.is_none() {
            return total;
        }
        total.saturating_add(
            spec.readiness
                .as_ref()
                .and_then(|readiness| readiness.timeout_ms)
                .unwrap_or(10_000),
        )
    });
    let cleanup_ms = specs
        .iter()
        .map(|spec| spec.cleanup_deadline_ms)
        .max()
        .unwrap_or(0)
        .saturating_mul(2);
    Duration::from_millis(readiness_ms)
        .saturating_add(Duration::from_millis(cleanup_ms))
        .saturating_add(WORKER_STARTUP_COORDINATION_MARGIN)
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
    let data_root = homeboy_core::paths::homeboy_data().map_err(|error| error.message)?;
    persist_record_at(&data_root, run_id, record)
}

fn persist_record_at(
    data_root: &Path,
    run_id: &str,
    record: &AgentTaskManagedServiceRecord,
) -> Result<(), String> {
    let path = data_root
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

pub(super) fn startup_failure_evidence(run_id: &str) -> (Vec<AgentTaskEvidenceRef>, Value) {
    let records = read_service_records(run_id);
    let services = records
        .iter()
        .map(|record| {
            let record_uri = service_record_path(run_id, &record.id)
                .map(|path| format!("file://{}", path.display()));
            json!({
                "id": record.id,
                "state": record.state,
                "record_uri": record_uri,
                "stdout_uri": record.stdout_uri,
                "stderr_uri": record.stderr_uri,
                "stdout_excerpt": record.log_path.as_deref().and_then(redacted_log_excerpt),
                "readiness_attempts": record.readiness_attempts,
                "cleanup": record.cleanup,
                "cleanup_outcome": record.cleanup_outcome,
            })
        })
        .collect::<Vec<_>>();
    let mut evidence_refs = Vec::new();
    for record in &records {
        if let Ok(path) = service_record_path(run_id, &record.id) {
            evidence_refs.push(AgentTaskEvidenceRef {
                kind: "managed-service-record".to_string(),
                uri: format!("file://{}", path.display()),
                label: Some(format!("managed service '{}' startup record", record.id)),
            });
        }
        for (kind, uri) in [
            ("managed-service-stdout", record.stdout_uri.as_ref()),
            ("managed-service-stderr", record.stderr_uri.as_ref()),
        ] {
            if let Some(uri) = uri {
                evidence_refs.push(AgentTaskEvidenceRef {
                    kind: kind.to_string(),
                    uri: uri.clone(),
                    label: Some(format!("managed service '{}' {kind}", record.id)),
                });
            }
        }
    }
    (
        evidence_refs,
        json!({
            "run_id": run_id,
            "service_supervisor_state_uri": service_worker_state_path(run_id)
                .ok()
                .map(|path| format!("file://{}", path.display())),
            "services": services,
        }),
    )
}

fn redacted_log_excerpt(path: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(4 * 1024);
    File::open(path)
        .ok()?
        .take(4 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(homeboy_core::redaction::redact_string(
        &String::from_utf8_lossy(&bytes),
    ))
}

fn service_record_path(run_id: &str, service_id: &str) -> Result<PathBuf, String> {
    Ok(homeboy_core::paths::homeboy_data()
        .map_err(|error| error.message)?
        .join("agent-task-runs")
        .join(run_id)
        .join("services")
        .join(format!("{service_id}.json")))
}

fn read_service_records(run_id: &str) -> Vec<AgentTaskManagedServiceRecord> {
    let directory = match homeboy_core::paths::homeboy_data() {
        Ok(path) => path.join("agent-task-runs").join(run_id).join("services"),
        Err(_) => return Vec::new(),
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
        .collect()
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
            state.services = read_service_records(&request.run_id);
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

/// Reconcile service ownership for one run below an explicit controller data
/// root. Request cleanup from the execution host recorded by the controller
/// handoff; the runner command uses its own HOME/data root, so no runner-local
/// path is ever interpreted by the controller.
///
/// Only the *local* branch reads a root at all: the runner branch dispatches
/// commands the runner interprets against its own HOME. Splitting them here is
/// what lets a rooted cancellation prove its local service ledger was reaped in
/// the same installation it wrote the terminal record into, instead of reaping
/// whichever ledger the process environment happened to point at (#7505).
///
/// The ambient `reconcile_run_services_on_owner` that used to resolve
/// `paths::homeboy_data()` for callers is gone: both remaining callers —
/// rooted cancellation and rooted lost-job terminalization — hold the lifecycle
/// store whose record they are about to replace, and reaping a different home's
/// ledger than the one that record lives in is exactly the split #7505 forbids.
pub(crate) fn reconcile_run_services_on_owner_at(
    data_root: &Path,
    run_id: &str,
    owner: Option<&Value>,
    reason: &str,
) -> Result<Value, String> {
    let Some(owner) = owner else {
        return Ok(json!({
            "transport": "local",
            "services": reconcile_run_services_at(data_root, run_id, reason)?,
        }));
    };
    let Some(runner_id) = owner.get("runner_id").and_then(Value::as_str) else {
        return Ok(json!({
            "transport": "local",
            "services": reconcile_run_services_at(data_root, run_id, reason)?,
        }));
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
    let data_root = homeboy_core::paths::homeboy_data().map_err(|error| error.message)?;
    reconcile_run_services_at(&data_root, run_id, reason)
}

/// Reconcile controller-local service ownership below an explicit lifecycle
/// data root rather than the calling process's ambient data root.
pub(crate) fn reconcile_run_services_at(
    data_root: &Path,
    run_id: &str,
    reason: &str,
) -> Result<Vec<AgentTaskManagedServiceRecord>, String> {
    let directory = data_root
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
        let cleanup_deadline = match requested_cleanup_deadline(&record) {
            Ok(deadline) => deadline,
            Err(error) => {
                record.state = "failed".to_string();
                record.cleanup_outcome = Some("failed_invalid_cleanup_deadline".to_string());
                record.cleanup = Some(format!("cleanup_failed_invalid_deadline:{reason}:{error}"));
                persist_record_at(data_root, run_id, &record)?;
                records.push(record);
                continue;
            }
        };
        record.requested_cleanup_deadline_ms = Some(cleanup_deadline);
        if matches!(record.state.as_str(), "stopped" | "failed") {
            records.push(record);
            continue;
        }
        let outcome = match (record.pid, record.process_identity.as_ref()) {
            (Some(pid), identity) => {
                match process_identity_state_with_start_identity(pid, None, identity) {
                    ProcessIdentityState::Live => terminate_process_tree_with_grace(
                        pid,
                        Duration::from_millis(cleanup_deadline),
                    )
                    .map(|termination| {
                        record.cleanup_outcome = Some(if termination.signal == "SIGKILL" {
                            "deadline_escalation_forced".to_string()
                        } else {
                            "graceful".to_string()
                        });
                        "reaped".to_string()
                    })
                    .unwrap_or_else(|error| format!("cleanup_failed:{error}")),
                    ProcessIdentityState::Dead => "already_exited".to_string(),
                    ProcessIdentityState::IdentityMismatch => {
                        record.cleanup_outcome = Some("skipped_ownership_mismatch".to_string());
                        "ownership_mismatch_not_signalled".to_string()
                    }
                    ProcessIdentityState::Unverifiable => {
                        record.cleanup_outcome = Some("skipped_ownership_unverifiable".to_string());
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
                    Ok(true) => terminate_isolated_process_group_with_grace(
                        group,
                        Duration::from_millis(cleanup_deadline),
                    )
                    .map(|forced| {
                        record.cleanup_outcome = Some(if forced {
                            "deadline_escalation_forced".to_string()
                        } else {
                            "graceful".to_string()
                        });
                        "reaped_process_group".to_string()
                    })
                    .unwrap_or_else(|error| format!("cleanup_failed:{error}")),
                    Ok(false) => outcome,
                    Err(error) => format!("cleanup_unverifiable:{error}"),
                }
            }
            _ => outcome,
        };
        record.state = if matches!(
            record.cleanup_outcome.as_deref(),
            Some("skipped_ownership_mismatch" | "skipped_ownership_unverifiable")
        ) || outcome.starts_with("cleanup_failed")
        {
            "failed".to_string()
        } else {
            "stopped".to_string()
        };
        if record.cleanup_outcome.is_none() {
            record.cleanup_outcome = Some(if outcome.starts_with("cleanup_failed") {
                "failed".to_string()
            } else {
                "graceful".to_string()
            });
        }
        record.cleanup = Some(format!("{outcome}:{reason}"));
        persist_record_at(data_root, run_id, &record)?;
        records.push(record);
    }
    Ok(records)
}

fn requested_cleanup_deadline(record: &AgentTaskManagedServiceRecord) -> Result<u64, String> {
    let validate = |value: u64, source: &str| {
        if value == 0 || value > AgentTaskManagedService::MAX_CLEANUP_DEADLINE_MS {
            return Err(format!(
                "managed service '{}' {source} cleanup deadline {value}ms is outside 1..={}ms",
                record.id,
                AgentTaskManagedService::MAX_CLEANUP_DEADLINE_MS
            ));
        }
        Ok(value)
    };
    let requested = record
        .requested_cleanup_deadline_ms
        .map(|value| validate(value, "requested"))
        .transpose()?;
    let provenance = match record.provenance.get("cleanup_deadline_ms") {
        Some(value) => Some(
            value
                .as_u64()
                .ok_or_else(|| {
                    format!(
                        "managed service '{}' provenance cleanup deadline is invalid",
                        record.id
                    )
                })
                .and_then(|value| validate(value, "provenance"))?,
        ),
        None => None,
    };
    match (requested, provenance) {
        (Some(requested), Some(provenance)) if requested != provenance => Err(format!(
            "managed service '{}' requested and provenance cleanup deadlines disagree",
            record.id
        )),
        (Some(requested), _) => Ok(requested),
        (_, Some(provenance)) => Ok(provenance),
        (None, None) => Ok(AgentTaskManagedService::DEFAULT_CLEANUP_DEADLINE_MS),
    }
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
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    reqwest::blocking::Client::builder()
                        .timeout(remaining.min(Duration::from_secs(1)))
                        .build()
                        .and_then(|client| client.get(url).send())
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

    use super::*;
    use homeboy_core::test_support::with_isolated_home;

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        listener.local_addr().expect("local address").port()
    }

    fn assert_cleanup_outcome(record: &AgentTaskManagedServiceRecord, expected: &str) -> bool {
        let outcome = record.cleanup_outcome.as_deref();
        if outcome == Some(expected) {
            return false;
        }
        assert!(
            outcome.is_some_and(|outcome| {
                outcome.starts_with("incomplete:process-scope discovery was incomplete:")
            }),
            "expected {expected} cleanup outcome or fail-closed procfs discovery, got {outcome:?}"
        );
        true
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

    fn persisted_record(
        id: &str,
        requested_cleanup_deadline_ms: Option<u64>,
        provenance_deadline: Value,
    ) -> AgentTaskManagedServiceRecord {
        serde_json::from_value(json!({
            "id": id,
            "state": "ready",
            "launch_token": "fixture",
            "local_url": null,
            "public_url": null,
            "log_path": null,
            "pid": null,
            "cleanup": null,
            "requested_cleanup_deadline_ms": requested_cleanup_deadline_ms,
            "provenance": { "run_id": "persisted-fixture", "cleanup_deadline_ms": provenance_deadline }
        }))
        .expect("persisted service record")
    }

    #[test]
    fn worker_cleanup_wait_budget_uses_the_longest_deadline_not_the_service_count() {
        let records = vec![
            persisted_record("first", Some(4_000), json!(4_000)),
            persisted_record("second", Some(3_000), json!(3_000)),
        ];
        assert_eq!(
            worker_cleanup_wait_budget(&records).expect("valid cleanup budget"),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn stale_reconcile_fails_closed_on_a_corrupt_persisted_deadline() {
        with_isolated_home(|_| {
            let run_id = "corrupt-cleanup-deadline";
            let directory = homeboy_core::paths::homeboy_data()
                .expect("home")
                .join("agent-task-runs")
                .join(run_id)
                .join("services");
            std::fs::create_dir_all(&directory).expect("service ledger directory");
            let mut record = persisted_record("corrupt", Some(0), json!(2_000));
            record.provenance["run_id"] = json!(run_id);
            persist_record(run_id, &record).expect("persist corrupt record");

            let records = reconcile_run_services(run_id, "test").expect("reconcile ledger");
            assert_eq!(records[0].state, "failed");
            assert_eq!(
                records[0].cleanup_outcome.as_deref(),
                Some("failed_invalid_cleanup_deadline")
            );
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stale_reconcile_records_ownership_mismatch_as_skipped_failure() {
        with_isolated_home(|_| {
            let run_id = "ownership-mismatch";
            let directory = homeboy_core::paths::homeboy_data()
                .expect("home")
                .join("agent-task-runs")
                .join(run_id)
                .join("services");
            std::fs::create_dir_all(&directory).expect("service ledger directory");
            let mut record = persisted_record("mismatch", Some(100), json!(100));
            record.provenance["run_id"] = json!(run_id);
            record.pid = Some(std::process::id());
            #[cfg(target_os = "linux")]
            {
                record.process_identity = Some(ProcessStartIdentity::Linux { starttime_ticks: 0 });
            }
            #[cfg(target_os = "macos")]
            {
                record.process_identity = Some(ProcessStartIdentity::Macos {
                    start_seconds: 0,
                    start_microseconds: 0,
                });
            }
            persist_record(run_id, &record).expect("persist mismatched record");

            let records = reconcile_run_services(run_id, "test").expect("reconcile ledger");
            assert_eq!(records[0].state, "failed");
            assert_eq!(
                records[0].cleanup_outcome.as_deref(),
                Some("skipped_ownership_mismatch")
            );
        });
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
}
