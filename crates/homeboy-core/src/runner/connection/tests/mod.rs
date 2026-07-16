#![cfg(test)]

mod recovery;
mod session;

use clap::Parser;
use std::collections::HashMap;
use std::io::{Read, Write};

use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::super::session::RunnerStaleRuntimePath;
use super::connection_daemon::{
    daemon_identity_from_body, daemon_runtime_loaded_paths_from_body,
    daemon_runtime_stale_paths_from_body, daemon_version_from_body, versions_match,
};
use crate::test_support;

pub(super) fn command_output(
    success: bool,
    stdout: impl Into<String>,
    timed_out: bool,
) -> crate::server::CommandOutput {
    crate::server::CommandOutput {
        stdout: stdout.into(),
        stderr: String::new(),
        success,
        exit_code: if success { 0 } else { 1 },
        timed_out,
        child_resource: None,
    }
}

pub(super) fn sample_leaseless_recovery() -> DaemonLeaselessRecoveryResult {
    serde_json::from_value(serde_json::json!({
        "affected_job_ids": [],
        "affected_job_count": 0,
        "evidence_snapshot_path": "/evidence/jobs.snapshot",
        "ownership_proof": ["owner lock acquired"],
        "retry_guidance": "retry",
        "replacement": {
            "pid": 42,
            "address": "127.0.0.1:7421",
            "state_path": "/state.json",
            "lease_id": "lease-new"
        }
    }))
    .expect("sample recovery")
}

pub(super) fn reverse_controller_session() -> RunnerSession {
    RunnerSession {
        runner_id: "homeboy-lab".to_string(),
        mode: RunnerTunnelMode::Reverse,
        role: RunnerSessionRole::Controller,
        server_id: None,
        controller_id: Some("extra-chill".to_string()),
        broker_url: Some("http://127.0.0.1:9876".to_string()),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        remote_daemon_lease_id: None,
        homeboy_version: "test".to_string(),
        homeboy_build_identity: Some("homeboy test+abc123".to_string()),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: Some("worker-1".to_string()),
        worker_pid: Some(1234),
        last_seen_at: Some(Utc::now().to_rfc3339()),
        leaseless_recovery_evidence: None,
    }
}

fn direct_controller_session() -> RunnerSession {
    let mut session = reverse_controller_session();
    session.mode = RunnerTunnelMode::DirectSsh;
    session.broker_url = None;
    session.local_url = Some("http://127.0.0.1:9877".to_string());
    session
}

#[test]
fn artifact_content_transport_routes_direct_sessions_to_the_daemon() {
    assert_eq!(
        artifact_content_transport(&direct_controller_session()).expect("direct transport"),
        RunnerArtifactContentTransport::DirectDaemon
    );
}

#[test]
fn artifact_content_transport_routes_reverse_sessions_to_the_broker() {
    assert_eq!(
        artifact_content_transport(&reverse_controller_session()).expect("reverse transport"),
        RunnerArtifactContentTransport::ReverseBroker
    );
}

#[test]
fn artifact_content_transport_rejects_sessions_without_a_managed_endpoint() {
    let mut session = direct_controller_session();
    session.local_url = None;
    let error = artifact_content_transport(&session).expect_err("missing endpoint");
    assert!(error.message.contains("no managed daemon endpoint"));
}

pub(super) fn sample_run_summary(id: &str) -> RunSummary {
    RunSummary {
        id: id.to_string(),
        kind: "test".to_string(),
        status: "running".to_string(),
        started_at: "2026-07-03T13:00:00Z".to_string(),
        finished_at: None,
        component_id: Some("wpcom".to_string()),
        rig_id: None,
        git_sha: None,
        command: Some("homeboy test wpcom".to_string()),
        cwd: Some("/workspace/wpcom".to_string()),
        status_note: None,
    }
}

pub(super) fn sample_active_job(
    durable_run_id: Option<&str>,
    command: &str,
) -> ActiveRunnerJobSummary {
    ActiveRunnerJobSummary {
        runner_id: "homeboy-lab".to_string(),
        job_id: "job-1".to_string(),
        operation: "runner.exec".to_string(),
        source: "direct-daemon".to_string(),
        kind: "test".to_string(),
        status: JobStatus::Running,
        command: command.to_string(),
        cwd: Some("/workspace/wpcom".to_string()),
        started_at_ms: 0,
        updated_at_ms: 0,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: JobClaimMetadata {
            claim_id: None,
            claimed_by_runner_id: Some("homeboy-lab".to_string()),
            claimed_at_ms: None,
            claim_expires_at_ms: None,
        },
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: durable_run_id.map(str::to_string),
        stale_reason: None,
        lifecycle_state: Some("running".to_string()),
        retryable: Some(false),
        active_child_count: None,
        active_cell_count: None,
    }
}

pub(super) fn direct_ssh_session(lease_id: &str) -> RunnerSession {
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

pub(super) fn remote_daemon_status_for_test(
    fresh: bool,
    reachable: bool,
    active_jobs: usize,
    lease_id: &str,
    pid: u32,
) -> RemoteDaemonStatus {
    remote_daemon_status_for_test_with_reason(fresh, reachable, active_jobs, lease_id, pid, None)
}

pub(super) fn remote_daemon_status_for_test_with_reason(
    fresh: bool,
    reachable: bool,
    active_jobs: usize,
    lease_id: &str,
    pid: u32,
    stale_reason_code: Option<DaemonStaleReasonCode>,
) -> RemoteDaemonStatus {
    RemoteDaemonStatus {
        daemon: Some(RemoteDaemon {
            address: "127.0.0.1:49152".to_string(),
            pid: Some(pid),
            lease_id: Some(lease_id.to_string()),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        }),
        stale_reason: (!fresh).then(|| "daemon is stale".to_string()),
        stale_reason_code,
        fresh,
        reachable,
        active_jobs,
        endpoint_probe_error: None,
        termination_evidence: None,
    }
}
