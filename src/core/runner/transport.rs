use serde_json::json;

use crate::core::engine::shell;
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::server::{self, SshClient};

use super::session::{RunnerSession, RunnerStatusReport, RunnerTunnelMode};
use super::{Runner, RunnerKind};

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
    client: SshClient,
}

impl RunnerFileTransferCapability {
    pub(crate) fn for_runner(
        runner: &Runner,
        status: Option<&RunnerStatusReport>,
    ) -> RunnerFileTransferCapability {
        match select_runner_transport(runner, status, false) {
            RunnerTransport::ReverseBroker(handle) => RunnerFileTransferCapability::Unsupported {
                transport: "reverse_broker",
                reason: "Lab runner file transfer over the reverse broker is not implemented yet",
                broker_url: Some(handle.endpoint_url().to_string()),
            },
            RunnerTransport::Local => RunnerFileTransferCapability::Unsupported {
                transport: "local",
                reason: "Lab runner file transfer requires a remote runner transport",
                broker_url: None,
            },
            RunnerTransport::DirectDaemon(_)
            | RunnerTransport::DiagnosticSsh
            | RunnerTransport::Unavailable => match (runner.kind.clone(), runner.server_id.clone()) {
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
            },
        }
    }

    pub(crate) fn ensure_supported(&self, runner_id: &str) -> Result<&str> {
        match self {
            RunnerFileTransferCapability::DirectSsh { server_id } => Ok(server_id.as_str()),
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
        let server_id = capability.ensure_supported(&runner.id)?;
        let server = server::load(server_id)?;
        let mut client = SshClient::from_server(&server, server_id)?;
        client.env.extend(runner.env.clone());
        Ok(RunnerFileTransfer {
            runner_id: runner.id.clone(),
            client,
        })
    }

    pub(crate) fn ensure_directory(&self, remote_dir: &str) -> Result<()> {
        let mkdir = self
            .client
            .execute(&format!("mkdir -p {}", shell::quote_arg(remote_dir)));
        if !mkdir.success {
            return Err(file_transfer_operation_error(
                &self.runner_id,
                "mkdir",
                remote_dir,
                mkdir.stderr,
            ));
        }
        Ok(())
    }

    pub(crate) fn upload_file(&self, local_path: &str, remote_path: &str) -> Result<()> {
        let upload = self.client.upload_file(local_path, remote_path);
        if !upload.success {
            return Err(file_transfer_operation_error(
                &self.runner_id,
                "upload",
                remote_path,
                upload.stderr,
            ));
        }
        Ok(())
    }

    pub(crate) fn download_file(&self, remote_path: &str, local_path: &str) -> Result<()> {
        let download = self.client.download_file(remote_path, local_path);
        if !download.success {
            return Err(file_transfer_operation_error(
                &self.runner_id,
                "download",
                remote_path,
                download.stderr,
            ));
        }
        Ok(())
    }
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
    let mut error = Error::new(
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
    );
    error.retryable = Some(false);
    error
        .with_hint("Use a direct SSH runner for Lab @file arguments and structured-output download until the reverse broker exposes a file-transfer API.".to_string())
        .with_hint("Next transport implementation should provide mkdir/upload/download through the selected runner transport instead of calling SSH directly.".to_string())
}

fn file_transfer_operation_error(
    runner_id: &str,
    operation: &str,
    remote_path: &str,
    stderr: String,
) -> Error {
    let mut error = Error::new(
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
            "transport": "direct_ssh",
        }),
    );
    error.retryable = Some(true);
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::session::{
        RunnerActiveJobState, RunnerSessionRole, RunnerSessionState,
    };

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
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "now".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
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
    fn file_transfer_errors_for_reverse_broker_with_actionable_metadata() {
        let runner = runner(RunnerKind::Ssh);
        let mut session = session(RunnerTunnelMode::Reverse);
        session.broker_url = Some("https://broker.example".to_string());
        let capability = RunnerFileTransferCapability::for_runner(&runner, Some(&status(session)));
        let err = capability
            .ensure_supported("test-runner")
            .expect_err("reverse broker file transfer should be unsupported");

        assert_eq!(err.code, ErrorCode::RunnerLabTransportFailure);
        assert_eq!(err.details["transport"], "reverse_broker");
        assert_eq!(err.details["broker_url"], "https://broker.example");
        assert_eq!(err.details["missing_capability"], "runner_file_transfer");
        assert!(err
            .message
            .contains("does not currently support controller file transfer"));
    }
}
