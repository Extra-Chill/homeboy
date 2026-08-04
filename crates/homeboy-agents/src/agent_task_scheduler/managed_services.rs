//! Plan-owned, runner-local process services for agent-task execution.
//!
//! This deliberately owns only process allocation/readiness and lifecycle
//! evidence. Public URLs are opaque references supplied by a preview/tunnel
//! provider, keeping this contract independent of any product integration.

use std::fs::OpenOptions;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use homeboy_core::agent_task_config::AgentTaskManagedServiceReadiness;
use serde_json::{json, Value};

use super::{AgentTaskManagedService, AgentTaskManagedServiceReadinessKind};

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct AgentTaskManagedServiceRecord {
    pub id: String,
    pub state: String,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub log_path: Option<String>,
    pub pid: Option<u32>,
    pub cleanup: Option<String>,
    pub provenance: Value,
}

pub(super) struct ManagedServices {
    services: Vec<RunningService>,
}

struct RunningService {
    spec: AgentTaskManagedService,
    child: Child,
    containment: homeboy_core::process::ProcessContainment,
    record: AgentTaskManagedServiceRecord,
}

impl ManagedServices {
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
            if spec.id.trim().is_empty() || spec.command.is_empty() {
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
        let mut command = Command::new(&spec.command[0]);
        command
            .args(&spec.command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        command.envs(&spec.env);
        for name in &spec.secret_env {
            let value = std::env::var(name).map_err(|_| {
                format!(
                    "managed service '{}' requires secret environment variable '{name}'",
                    spec.id
                )
            })?;
            command.env(name, value);
        }
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
            provenance: json!({"schema":"homeboy/agent-task-managed-service/v1", "argv": spec.command, "cwd": spec.cwd, "host": spec.host, "port": spec.port, "secret_env": spec.secret_env}),
        };
        if let Err(error) = wait_ready(&spec, local_url.as_deref()) {
            let _ = containment.terminate_on_failure_bounded(Duration::from_secs(2), false);
            record.state = "failed".to_string();
            record.cleanup = Some("terminated_after_readiness_failure".to_string());
            return Err(format!("managed service '{}': {error}", spec.id));
        }
        record.state = "ready".to_string();
        self.services.push(RunningService {
            spec,
            child,
            containment,
            record,
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
                Ok(())
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
        }
        self.records()
    }
}

fn wait_ready(spec: &AgentTaskManagedService, local_url: Option<&str>) -> Result<(), String> {
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
            return Ok(());
        }
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
                "import http.server, os; http.server.HTTPServer(('127.0.0.1', int(os.environ['PORT'])), http.server.SimpleHTTPRequestHandler).serve_forever()".to_string(),
            ],
            cwd: None,
            env: HashMap::from([("PORT".to_string(), port.to_string())]),
            secret_env: Vec::new(),
            host: "127.0.0.1".to_string(),
            port: Some(port),
            readiness: Some(AgentTaskManagedServiceReadiness {
                kind: AgentTaskManagedServiceReadinessKind::Http,
                path: Some("/".to_string()), timeout_ms: Some(5_000),
            }),
            public_url: Some("https://preview.example.test/fixture".to_string()),
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
}
