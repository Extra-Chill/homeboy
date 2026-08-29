//! Typed Lab client for Homeboy orchestration resources.
//!
//! After a session is connected, capability and run retrieval uses HTTP
//! (`local_url`, or the broker URL when that is the session endpoint). This
//! module does not shell out to Homeboy and does not execute a routine remote
//! `ssh ... curl` probe.
//!
//! Remaining SSH in this crate is bootstrap, daemon install, forwarding,
//! emergency recovery, and host diagnostics — not these resource operations.

use std::time::Duration;

use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde_json::Value;

use homeboy_control_plane_contract::{
    ControlPlaneCapabilities, ControlPlaneError, ControlPlaneResult, ControlPlaneRun, RunId,
};
use homeboy_lab_runner_contract::RunnerSession;

/// HTTP client for control-plane capabilities and run reads.
pub struct ControlPlaneClient {
    http: Client,
    base_url: String,
}

impl ControlPlaneClient {
    /// Build a client from a connected session's HTTP endpoint.
    ///
    /// This constructor accepts only an HTTP client. Routine reads therefore
    /// have no CLI or SSH command executor available to invoke.
    pub fn from_connected_session(
        session: &RunnerSession,
        http: Client,
    ) -> Result<Self, ControlPlaneError> {
        let base_url = session
            .local_url
            .as_deref()
            .or(session.broker_url.as_deref())
            .ok_or_else(|| {
                ControlPlaneError::unavailable(
                    "connected session has no HTTP endpoint",
                    "homeboy runner connect",
                )
            })?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn capabilities(&self) -> Result<ControlPlaneCapabilities, ControlPlaneError> {
        self.get("/v1/control-plane/capabilities")
    }

    pub fn run(&self, id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        self.get(&format!("/v1/control-plane/runs/{}", id.as_str()))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ControlPlaneError> {
        let url = format!("{}{path}", self.base_url);
        let response = self.http.get(&url).send().map_err(|error| {
            ControlPlaneError::transport(
                format!("control-plane GET {path}: {error}"),
                "homeboy runner connect",
            )
        })?;
        let status = response.status().as_u16();
        let body = response.text().map_err(|error| {
            ControlPlaneError::transport(
                format!("read control-plane response {path}: {error}"),
                "homeboy runner connect",
            )
        })?;
        parse_control_plane_body(&body, status, path)
    }
}

fn parse_control_plane_body<T: DeserializeOwned>(
    body: &str,
    status: u16,
    path: &str,
) -> Result<T, ControlPlaneError> {
    let envelope: Value = serde_json::from_str(body).map_err(|error| {
        ControlPlaneError::transport(
            format!("malformed control-plane JSON for {path}: {error}"),
            "homeboy runner connect",
        )
    })?;
    let payload = envelope
        .get("data")
        .cloned()
        .or_else(|| envelope.get("error").cloned())
        .unwrap_or(envelope);
    let result_value = payload.get("body").cloned().unwrap_or(payload);
    let result: ControlPlaneResult<T> = serde_json::from_value(result_value).map_err(|error| {
        ControlPlaneError::transport(
            format!("malformed control-plane result for {path}: {error}"),
            "homeboy runner connect",
        )
    })?;
    if result.ok {
        result.resource.ok_or_else(|| {
            ControlPlaneError::unavailable(
                format!("control-plane response for {path} omitted the resource"),
                "homeboy runner connect",
            )
        })
    } else {
        Err(result.error.unwrap_or_else(|| {
            ControlPlaneError::unavailable(
                format!("control-plane request {path} failed ({status})"),
                "homeboy runner connect",
            )
        }))
    }
}

pub fn default_http_client() -> Result<Client, ControlPlaneError> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            ControlPlaneError::transport(
                format!("build control-plane HTTP client: {error}"),
                "homeboy runner connect",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneClient;
    use homeboy_control_plane_contract::{
        ControlPlaneCapabilities, ControlPlaneErrorClass, ControlPlaneOperation, ControlPlaneRef,
        ControlPlaneResult, ControlPlaneRun, ControlPlaneRunState, MissionId, RunId,
        CONTROL_PLANE_RUN_SCHEMA,
    };
    use homeboy_lab_runner_contract::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    fn sample_run() -> ControlPlaneRun {
        let run = RunId::new(AGENT_TASK_RUN).expect("run");
        let mut resource = ControlPlaneRun::new(
            run.clone(),
            ControlPlaneRef::Mission(MissionId::new(AGENT_TASK_COOK).expect("mission")),
            ControlPlaneRef::Run(run.clone()),
        );
        resource.mission = Some(MissionId::new(AGENT_TASK_COOK).expect("mission"));
        resource.state = ControlPlaneRunState::Succeeded;
        resource.created_at = "2026-01-01T00:00:00Z".to_string();
        resource
    }

    fn connected_session(local_url: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("server".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: Some(local_url.to_string()),
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "0.0.0+test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn reverse_session(broker_url: &str) -> RunnerSession {
        let mut session = connected_session("");
        session.mode = RunnerTunnelMode::Reverse;
        session.local_url = None;
        session.broker_url = Some(broker_url.to_string());
        session
    }

    fn spawn_control_plane_http(
        capabilities: ControlPlaneCapabilities,
        run: ControlPlaneRun,
        fail_path: Option<&'static str>,
        expected_requests: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let handle = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0; 4096];
                let length = stream.read(&mut request).unwrap_or(0);
                let header = String::from_utf8_lossy(&request[..length]);
                let path = header
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("");
                let (status, envelope) = if fail_path == Some(path) {
                    let error = homeboy_control_plane_contract::ControlPlaneError::not_found(
                        "agent-task run not found: missing",
                        "homeboy agent-task active",
                    );
                    (
                        404,
                        json!({
                            "success": false,
                            "data": {
                                "status": 404,
                                "endpoint": "control_plane.runs.show",
                                "body": ControlPlaneResult::<ControlPlaneRun>::err(error)
                            }
                        }),
                    )
                } else if path == "/v1/control-plane/capabilities" {
                    (
                        200,
                        json!({
                            "success": true,
                            "data": {
                                "status": 200,
                                "endpoint": "control_plane.capabilities",
                                "body": ControlPlaneResult::ok(capabilities.clone())
                            }
                        }),
                    )
                } else if path.starts_with("/v1/control-plane/runs/") {
                    (
                        200,
                        json!({
                            "success": true,
                            "data": {
                                "status": 200,
                                "endpoint": "control_plane.runs.show",
                                "body": ControlPlaneResult::ok(run.clone())
                            }
                        }),
                    )
                } else {
                    (
                        404,
                        json!({
                            "success": false,
                            "error": { "message": format!("unhandled {path}") }
                        }),
                    )
                };
                let body = serde_json::to_string(&envelope).expect("envelope");
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn connected_session_control_plane_reads_use_http_without_cli_or_ssh() {
        let capabilities = ControlPlaneCapabilities::this_build();
        let run = sample_run();
        let (url, server) = spawn_control_plane_http(capabilities.clone(), run.clone(), None, 2);
        let session = connected_session(&url);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");
        let client = ControlPlaneClient::from_connected_session(&session, http).expect("client");

        let fetched_capabilities = client.capabilities().expect("capabilities");
        assert_eq!(fetched_capabilities, capabilities);
        assert_eq!(
            fetched_capabilities.operations,
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun
            ]
        );

        let fetched_run = client
            .run(&RunId::new(AGENT_TASK_RUN).expect("run id"))
            .expect("run");
        assert_eq!(fetched_run, run);
        assert_eq!(fetched_run.schema, CONTROL_PLANE_RUN_SCHEMA);
        assert!(!fetched_run.reconciles);

        server.join().expect("server");
    }

    #[test]
    fn reverse_session_uses_the_broker_http_endpoint() {
        let capabilities = ControlPlaneCapabilities::this_build();
        let (url, server) = spawn_control_plane_http(capabilities.clone(), sample_run(), None, 1);
        let session = reverse_session(&url);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");
        let client = ControlPlaneClient::from_connected_session(&session, http).expect("client");

        assert_eq!(client.capabilities().expect("capabilities"), capabilities);
        server.join().expect("server");
    }

    #[test]
    fn control_plane_http_failures_preserve_class_retryability_and_next_action() {
        let (url, server) = spawn_control_plane_http(
            ControlPlaneCapabilities::this_build(),
            sample_run(),
            Some("/v1/control-plane/runs/missing"),
            1,
        );
        let session = connected_session(&url);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");
        let client = ControlPlaneClient::from_connected_session(&session, http).expect("client");
        let error = client
            .run(&RunId::new("missing").expect("id"))
            .expect_err("typed failure");
        assert_eq!(error.class, ControlPlaneErrorClass::NotFound);
        assert!(!error.retryable);
        assert_eq!(
            error.next_action.as_deref(),
            Some("homeboy agent-task active")
        );
        server.join().expect("server");
    }
}
