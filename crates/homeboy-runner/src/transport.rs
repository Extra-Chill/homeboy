use std::fs;
use std::time::Duration;

use base64::Engine;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::server::{self, SshClient};

use super::session::{RunnerSession, RunnerStatusReport, RunnerTunnelMode};
use super::{broker_http, Runner, RunnerKind};
use homeboy_core::broker_auth;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnerSessionHandle {
    pub(crate) session: RunnerSession,
    endpoint_url: String,
}

impl RunnerSessionHandle {
    fn new(session: RunnerSession, endpoint_url: String) -> Self {
        Self {
            session,
            endpoint_url,
        }
    }

    pub(crate) fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerTransport {
    DirectDaemon(RunnerSessionHandle),
    ReverseBroker(RunnerSessionHandle),
    Local,
    DiagnosticSsh,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerFileTransferCapability {
    DaemonHttp {
        endpoint_url: String,
        broker: bool,
    },
    DirectSsh {
        server_id: String,
    },
    Unsupported {
        transport: &'static str,
        reason: &'static str,
        broker_url: Option<String>,
    },
}

pub(crate) struct RunnerFileTransfer {
    runner_id: String,
    workspace_root: Option<String>,
    channel: RunnerFileChannel,
}

enum RunnerFileChannel {
    DirectSsh(SshClient),
    DaemonHttp {
        client: Client,
        endpoint_url: String,
        broker_token: Option<String>,
    },
    BrokerHttp {
        client: Client,
        endpoint_url: String,
        broker_token: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct HttpFileEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

impl RunnerFileTransferCapability {
    pub(crate) fn for_runner(
        runner: &Runner,
        status: Option<&RunnerStatusReport>,
    ) -> RunnerFileTransferCapability {
        match select_runner_transport(runner, status, false) {
            RunnerTransport::ReverseBroker(handle) => RunnerFileTransferCapability::DaemonHttp {
                endpoint_url: handle.endpoint_url().to_string(),
                broker: true,
            },
            RunnerTransport::Local => RunnerFileTransferCapability::Unsupported {
                transport: "local",
                reason: "Lab runner file transfer requires a remote runner transport",
                broker_url: None,
            },
            RunnerTransport::DirectDaemon(handle) => RunnerFileTransferCapability::DaemonHttp {
                endpoint_url: handle.endpoint_url().to_string(),
                broker: false,
            },
            RunnerTransport::DiagnosticSsh | RunnerTransport::Unavailable => {
                match (runner.kind.clone(), runner.server_id.clone()) {
                (RunnerKind::Ssh, Some(server_id)) => {
                    RunnerFileTransferCapability::DirectSsh { server_id }
                }
                (RunnerKind::Ssh, None) => RunnerFileTransferCapability::Unsupported {
                    transport: "direct_daemon",
                    reason: "Lab runner file transfer over the direct daemon API is not implemented yet and no SSH server_id is configured",
                    broker_url: None,
                },
                (RunnerKind::Local, _) => RunnerFileTransferCapability::Unsupported {
                    transport: "local",
                    reason: "Lab runner file transfer requires a remote runner transport",
                    broker_url: None,
                },
                }
            }
        }
    }

    pub(crate) fn ensure_supported(&self, runner_id: &str) -> Result<()> {
        match self {
            RunnerFileTransferCapability::DaemonHttp { .. }
            | RunnerFileTransferCapability::DirectSsh { .. } => Ok(()),
            RunnerFileTransferCapability::Unsupported {
                transport,
                reason,
                broker_url,
            } => Err(unsupported_file_transfer_error(
                runner_id,
                transport,
                reason,
                broker_url.as_deref(),
            )),
        }
    }
}

impl RunnerFileTransfer {
    pub(crate) fn for_runner(
        runner: &Runner,
        status: Option<&RunnerStatusReport>,
    ) -> Result<RunnerFileTransfer> {
        let capability = RunnerFileTransferCapability::for_runner(runner, status);
        capability.ensure_supported(&runner.id)?;
        let channel = match capability {
            RunnerFileTransferCapability::DaemonHttp {
                endpoint_url,
                broker,
            } => {
                let mut client_builder = Client::builder().timeout(Duration::from_secs(30));
                if !broker {
                    client_builder = client_builder.no_proxy();
                }
                let client = client_builder.build().map_err(|err| {
                    Error::internal_unexpected(format!("build runner file HTTP client: {err}"))
                })?;
                let broker_token = broker_auth::broker_submit_token_for_runner(&runner.id)?;
                if broker {
                    RunnerFileChannel::BrokerHttp {
                        client,
                        endpoint_url,
                        broker_token,
                    }
                } else {
                    RunnerFileChannel::DaemonHttp {
                        client,
                        endpoint_url,
                        broker_token,
                    }
                }
            }
            RunnerFileTransferCapability::DirectSsh { server_id } => {
                let server = server::load(&server_id)?;
                let mut client = SshClient::from_server(&server, &server_id)?;
                client.env.extend(runner.env.clone());
                RunnerFileChannel::DirectSsh(client)
            }
            RunnerFileTransferCapability::Unsupported { .. } => unreachable!("checked above"),
        };
        Ok(RunnerFileTransfer {
            runner_id: runner.id.clone(),
            workspace_root: runner.workspace_root.clone(),
            channel,
        })
    }

    pub(crate) fn ensure_directory(&self, remote_dir: &str) -> Result<()> {
        match &self.channel {
            RunnerFileChannel::DirectSsh(client) => {
                let mkdir = client.execute(&format!("mkdir -p {}", shell::quote_arg(remote_dir)));
                if !mkdir.success {
                    return Err(file_transfer_operation_error(
                        &self.runner_id,
                        "mkdir",
                        remote_dir,
                        mkdir.stderr,
                        "direct_ssh",
                    ));
                }
            }
            RunnerFileChannel::DaemonHttp { .. } | RunnerFileChannel::BrokerHttp { .. } => {
                self.http_post_json(
                    "/files/mkdir",
                    self.file_path_body(remote_dir),
                    "mkdir",
                    remote_dir,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn upload_file(&self, local_path: &str, remote_path: &str) -> Result<()> {
        match &self.channel {
            RunnerFileChannel::DirectSsh(client) => {
                let upload = client.upload_file(local_path, remote_path);
                if !upload.success {
                    return Err(file_transfer_operation_error(
                        &self.runner_id,
                        "upload",
                        remote_path,
                        upload.stderr,
                        "direct_ssh",
                    ));
                }
            }
            RunnerFileChannel::DaemonHttp { .. } | RunnerFileChannel::BrokerHttp { .. } => {
                let content = fs::read(local_path).map_err(|err| {
                    Error::internal_io(err.to_string(), Some(format!("read {local_path}")))
                })?;
                self.http_post_json(
                    "/files/upload",
                    self.file_upload_body(
                        remote_path,
                        base64::engine::general_purpose::STANDARD.encode(content),
                    ),
                    "upload",
                    remote_path,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn download_file(&self, remote_path: &str, local_path: &str) -> Result<()> {
        match &self.channel {
            RunnerFileChannel::DirectSsh(client) => {
                let download = client.download_file(remote_path, local_path);
                if !download.success {
                    return Err(file_transfer_operation_error(
                        &self.runner_id,
                        "download",
                        remote_path,
                        download.stderr,
                        "direct_ssh",
                    ));
                }
            }
            RunnerFileChannel::DaemonHttp { .. } | RunnerFileChannel::BrokerHttp { .. } => {
                let body = self.http_post_json(
                    "/files/download",
                    self.file_path_body(remote_path),
                    "download",
                    remote_path,
                )?;
                let encoded = body
                    .get("content_base64")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| {
                        Error::internal_unexpected("runner file download missing content_base64")
                    })?;
                let content = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|err| {
                        Error::internal_json(
                            err.to_string(),
                            Some("decode runner file download".to_string()),
                        )
                    })?;
                fs::write(local_path, content).map_err(|err| {
                    Error::internal_io(err.to_string(), Some(format!("write {local_path}")))
                })?;
            }
        }
        Ok(())
    }

    fn http_post_json(
        &self,
        path: &str,
        body: Value,
        operation: &str,
        remote_path: &str,
    ) -> Result<Value> {
        match &self.channel {
            RunnerFileChannel::DirectSsh(_) => unreachable!("direct SSH does not use HTTP"),
            RunnerFileChannel::BrokerHttp {
                client,
                endpoint_url,
                broker_token,
            } => broker_http::post_json(
                client,
                endpoint_url,
                path,
                body,
                "runner file broker request",
                broker_token.as_deref(),
            )
            .map_err(|err| {
                http_file_transfer_error(
                    &self.runner_id,
                    operation,
                    remote_path,
                    err,
                    "reverse_broker",
                )
            }),
            RunnerFileChannel::DaemonHttp {
                client,
                endpoint_url,
                broker_token,
            } => daemon_file_post_json(client, endpoint_url, path, body, broker_token.as_deref())
                .map_err(|err| {
                    http_file_transfer_error(
                        &self.runner_id,
                        operation,
                        remote_path,
                        err,
                        "daemon_http",
                    )
                }),
        }
    }

    fn file_path_body(&self, path: &str) -> Value {
        json!({
            "runner_id": &self.runner_id,
            "path": path,
            "workspace_root": &self.workspace_root,
        })
    }

    fn file_upload_body(&self, path: &str, content_base64: String) -> Value {
        json!({
            "runner_id": &self.runner_id,
            "path": path,
            "workspace_root": &self.workspace_root,
            "content_base64": content_base64,
        })
    }
}

fn daemon_file_post_json(
    client: &Client,
    base_url: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> Result<Value> {
    let mut request = client
        .post(format!("{}{}", base_url.trim_end_matches('/'), path))
        .json(&body);
    if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
        request = request
            .header(broker_auth::BROKER_TOKEN_HEADER, token)
            .bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|err| Error::internal_unexpected(format!("runner file daemon request: {err}")))?;
    let status_code = response.status().as_u16();
    let envelope: HttpFileEnvelope = response.json().map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner file daemon response".to_string()),
        )
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "runner file daemon request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("runner file daemon response missing data"))?;
    data.get("body")
        .cloned()
        .ok_or_else(|| Error::internal_unexpected("runner file daemon response missing data.body"))
}

pub(crate) fn select_runner_transport(
    runner: &Runner,
    status: Option<&RunnerStatusReport>,
    allow_diagnostic_ssh: bool,
) -> RunnerTransport {
    if runner.kind == RunnerKind::Ssh && allow_diagnostic_ssh {
        return RunnerTransport::DiagnosticSsh;
    }

    if let Some(status) = status {
        if status.connected {
            if let Some(session) = status.session.as_ref() {
                if let Some(local_url) = session.local_url.as_ref() {
                    return RunnerTransport::DirectDaemon(RunnerSessionHandle::new(
                        session.clone(),
                        local_url.clone(),
                    ));
                }
                if session.mode == RunnerTunnelMode::Reverse {
                    if let Some(broker_url) = session.broker_url.as_ref() {
                        return RunnerTransport::ReverseBroker(RunnerSessionHandle::new(
                            session.clone(),
                            broker_url.clone(),
                        ));
                    }
                }
            }
        }
    }

    if runner.kind == RunnerKind::Local {
        RunnerTransport::Local
    } else {
        RunnerTransport::Unavailable
    }
}

fn unsupported_file_transfer_error(
    runner_id: &str,
    transport: &str,
    reason: &str,
    broker_url: Option<&str>,
) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "Lab offload runner `{runner_id}` does not currently support controller file transfer over `{transport}`: {reason}"
        ),
        json!({
            "runner_id": runner_id,
            "transport": transport,
            "broker_url": broker_url,
            "missing_capability": "runner_file_transfer",
            "supported_transports": ["direct_ssh"],
        }),
    )
    .with_retryable(false)
        .with_hint("Use a direct SSH runner for Lab @file arguments and structured-output download until the reverse broker exposes a file-transfer API.".to_string())
        .with_hint("Next transport implementation should provide mkdir/upload/download through the selected runner transport instead of calling SSH directly.".to_string())
}

fn file_transfer_operation_error(
    runner_id: &str,
    operation: &str,
    remote_path: &str,
    stderr: String,
    transport: &str,
) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "Lab runner file transfer `{operation}` failed on runner `{runner_id}` for `{remote_path}`: {}",
            stderr.trim()
        ),
        json!({
            "runner_id": runner_id,
            "operation": operation,
            "remote_path": remote_path,
            "stderr": stderr,
            "transport": transport,
        }),
    )
    .with_retryable(true)
}

fn http_file_transfer_error(
    runner_id: &str,
    operation: &str,
    remote_path: &str,
    source: Error,
    transport: &str,
) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "Lab runner file transfer `{operation}` failed on runner `{runner_id}` for `{remote_path}`: {}",
            source.message
        ),
        json!({
            "runner_id": runner_id,
            "operation": operation,
            "remote_path": remote_path,
            "transport": transport,
            "source": source.details,
        }),
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{RunnerActiveJobState, RunnerSessionRole, RunnerSessionState};
    use std::io::{Read, Write};

    fn runner(kind: RunnerKind) -> Runner {
        Runner {
            id: "test-runner".to_string(),
            kind,
            server_id: None,
            workspace_root: None,
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        }
    }

    fn status(session: RunnerSession) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "test-runner".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(session),
            stale_daemon: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_jobs: Vec::new(),
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            session_path: "/tmp/session.json".to_string(),
        }
    }

    fn session(mode: RunnerTunnelMode) -> RunnerSession {
        RunnerSession {
            runner_id: "test-runner".to_string(),
            mode,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "now".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn selects_diagnostic_ssh_before_session_transport() {
        assert_eq!(
            select_runner_transport(&runner(RunnerKind::Ssh), None, true),
            RunnerTransport::DiagnosticSsh
        );
    }

    #[test]
    fn selects_direct_daemon_from_connected_local_url() {
        let mut session = session(RunnerTunnelMode::DirectSsh);
        session.local_url = Some("http://127.0.0.1:1234".to_string());

        match select_runner_transport(&runner(RunnerKind::Ssh), Some(&status(session)), false) {
            RunnerTransport::DirectDaemon(handle) => {
                assert_eq!(handle.endpoint_url(), "http://127.0.0.1:1234");
            }
            transport => panic!("expected direct daemon, got {transport:?}"),
        }
    }

    #[test]
    fn selects_reverse_broker_from_connected_reverse_session() {
        let mut session = session(RunnerTunnelMode::Reverse);
        session.broker_url = Some("https://broker.example".to_string());

        match select_runner_transport(&runner(RunnerKind::Ssh), Some(&status(session)), false) {
            RunnerTransport::ReverseBroker(handle) => {
                assert_eq!(handle.endpoint_url(), "https://broker.example");
            }
            transport => panic!("expected reverse broker, got {transport:?}"),
        }
    }

    #[test]
    fn file_transfer_selects_direct_ssh_when_server_id_exists() {
        let mut runner = runner(RunnerKind::Ssh);
        runner.server_id = Some("srv".to_string());

        assert_eq!(
            RunnerFileTransferCapability::for_runner(&runner, None),
            RunnerFileTransferCapability::DirectSsh {
                server_id: "srv".to_string(),
            }
        );
    }

    #[test]
    fn file_transfer_selects_reverse_broker_http_when_connected() {
        let runner = runner(RunnerKind::Ssh);
        let mut session = session(RunnerTunnelMode::Reverse);
        session.broker_url = Some("https://broker.example".to_string());
        let capability = RunnerFileTransferCapability::for_runner(&runner, Some(&status(session)));

        assert_eq!(
            capability,
            RunnerFileTransferCapability::DaemonHttp {
                endpoint_url: "https://broker.example".to_string(),
                broker: true,
            }
        );
    }

    #[test]
    fn file_transfer_selects_direct_daemon_http_when_connected() {
        let runner = runner(RunnerKind::Ssh);
        let mut session = session(RunnerTunnelMode::DirectSsh);
        session.local_url = Some("http://127.0.0.1:1234".to_string());
        let capability = RunnerFileTransferCapability::for_runner(&runner, Some(&status(session)));

        assert_eq!(
            capability,
            RunnerFileTransferCapability::DaemonHttp {
                endpoint_url: "http://127.0.0.1:1234".to_string(),
                broker: false,
            }
        );
    }

    #[test]
    fn daemon_file_post_json_attaches_broker_token_headers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = String::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).expect("read");
                if read == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/json\r\n",
                "Connection: close\r\n",
                "\r\n",
                "{\"success\":true,\"data\":{\"body\":{\"ok\":true}}}"
            );
            stream.write_all(response.as_bytes()).expect("write");
            request
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");

        let body = daemon_file_post_json(
            &client,
            &format!("http://{address}"),
            "/files/mkdir",
            json!({ "runner_id": "test-runner", "path": "/tmp/x" }),
            Some("secret-token"),
        )
        .expect("daemon response");

        assert_eq!(body["ok"], true);
        let request = server.join().expect("server");
        assert!(request.contains("x-homeboy-broker-token: secret-token"));
        assert!(request.contains("authorization: Bearer secret-token"));
    }
}
