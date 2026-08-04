//! Plan-owned, runner-local process services for agent-task execution.
//!
//! This deliberately owns only process allocation/readiness and lifecycle
//! evidence. Public URLs are opaque references supplied by a preview/tunnel
//! provider, keeping this contract independent of any product integration.

use std::fs::{File, OpenOptions};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
pub(crate) struct AgentTaskManagedServiceRecord {
    pub id: String,
    pub state: String,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub log_path: Option<String>,
    pub pid: Option<u32>,
    pub cleanup: Option<String>,
    pub provenance: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_identity: Option<ProcessStartIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_lease: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_attempts: Vec<Value>,
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
        let (port, port_lease, listener) = lease_port(&spec)?;
        spec.port = port;
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
        handoff_listener(&mut command, listener.as_ref())?;
        let mut containment = homeboy_core::process::ProcessContainment::prepare(&mut command)
            .map_err(|error| error.message)?;
        let child = command
            .spawn()
            .map_err(|error| format!("start managed service '{}': {error}", spec.id))?;
        containment.attach(&child).map_err(|error| error.message)?;
        let local_url = spec.port.map(|port| format!("http://{}:{port}", spec.host));
        let mut record = AgentTaskManagedServiceRecord {
            id: spec.id.clone(),
            state: "starting".to_string(),
            local_url: local_url.clone(),
            public_url: spec.public_url.clone(),
            log_path: Some(log_path.display().to_string()),
            pid: Some(child.id()),
            cleanup: None,
            provenance: json!({"schema":"homeboy/agent-task-managed-service/v3", "run_id": run_id, "argv": spec.command, "cwd": spec.cwd, "host": spec.host, "port": spec.port, "target": spec.target, "lifecycle": spec.lifecycle, "socket_handoff": spec.socket_handoff, "env_allowlist": spec.env_allowlist, "secret_env": spec.secret_env, "secret_env_plan": spec.secret_env_plan.as_ref().map(|plan| plan.redacted()), "endpoint_ownership": { "host": spec.host, "port": spec.port, "lease": port_lease.as_ref().map(|lease| lease.path.display().to_string()) }}),
            process_identity: process_start_identity(child.id())
                .map_err(|error| format!("inspect managed service process identity: {error}"))?,
            port_lease: port_lease
                .as_ref()
                .map(|lease| lease.path.display().to_string()),
            readiness_attempts: Vec::new(),
        };
        persist_record(run_id, &record)?;
        if let Err(error) = wait_ready(&spec, local_url.as_deref(), &mut record.readiness_attempts)
        {
            let _ = containment.terminate_on_failure_bounded(Duration::from_secs(2), false);
            record.state = "failed".to_string();
            record.cleanup = Some("terminated_after_readiness_failure".to_string());
            return Err(format!("managed service '{}': {error}", spec.id));
        }
        record.state = "ready".to_string();
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
            "browser_origin": record.public_url.clone().or(record.local_url.clone()),
            "lease_ref": format!("managed-service:{}", record.id), "readiness_evidence": record.provenance,
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

// Keep the old scheduler-private name while callers migrate to the generic
// execution-host supervisor API.
pub(super) type ManagedServices = AgentTaskServiceSupervisor;

fn lease_port(
    spec: &AgentTaskManagedService,
) -> Result<(Option<u16>, Option<PortLease>, Option<TcpListener>), String> {
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
                metadata["managed_services"]["fixture"]["browser_origin"],
                "https://preview.example.test/fixture"
            );
            let records = services.cleanup("success");
            assert_eq!(records[0].state, "stopped");
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
        });
    }
}
