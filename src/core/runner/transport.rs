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
}
