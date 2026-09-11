//! Shared fixtures for lab-runner tests.

use crate::{
    Runner, RunnerKind, RunnerPolicy, RunnerSession, RunnerSessionRole, RunnerSettings,
    RunnerTunnelMode,
};
use chrono::Utc;

pub(crate) fn ssh_runner() -> Runner {
    Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("srv".to_string()),
        workspace_root: Some("/srv/homeboy".to_string()),
        settings: RunnerSettings {
            daemon: true,
            ..Default::default()
        },
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: RunnerPolicy::default(),
    }
}

pub(crate) fn direct_ssh_session(lease_id: &str) -> RunnerSession {
    RunnerSession {
        runner_id: "homeboy-lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("homeboy-lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:49152".to_string()),
        local_port: Some(49153),
        local_url: Some("http://127.0.0.1:49153".to_string()),
        tunnel_pid: Some(1234),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(4242),
        remote_daemon_lease_id: Some(lease_id.to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: Some("homeboy test+abc123".to_string()),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    }
}
