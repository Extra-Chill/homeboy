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
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonRecoveryEvidence};
use homeboy_core::test_support;

#[test]
fn controller_proxy_url_rejects_userinfo_before_opening_a_forward() {
    let error = controller_proxy_from_url("socks5://user:token@127.0.0.1:8080")
        .expect_err("proxy credentials cannot cross the controller boundary");
    assert!(error.message.contains("must not include proxy credentials"));
    assert!(error.details.to_string().contains("credential-free"));
}

#[test]
fn admission_observations_share_a_total_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("listener address");
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (_stream, _) = listener.accept().expect("admission request");
            accepted_tx.send(()).expect("record admission request");
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    let mut session = direct_ssh_session("lease-deadline");
    session.local_url = Some(format!("http://{address}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);

    assert!(runner_jobs_until("homeboy-lab", &session, deadline).is_err());
    assert!(runner_running_runs_until(&session, deadline).is_err());
    accepted_rx
        .recv_timeout(std::time::Duration::from_millis(10))
        .expect("first observation reached the listener");
    assert!(
        accepted_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "a later admission observation must not receive a fresh timeout budget"
    );
}

#[test]
fn proxy_forward_reuse_rejects_a_reused_pid_with_a_different_start_identity() {
    let forward = RunnerProxyForward {
        runner_url: "http://127.0.0.1:8080".to_string(),
        tunnel_pid: 4242,
        tunnel_process_start_identity: Some(RunnerTunnelProcessStartIdentity::Macos {
            start_seconds: 1,
            start_microseconds: 2,
        }),
    };

    assert!(!proxy_forward_is_owned(&forward, |_| {
        Ok(Some(homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds: 3,
            start_microseconds: 4,
        }))
    }));
}

pub(super) fn command_output(
    success: bool,
    stdout: impl Into<String>,
    timed_out: bool,
) -> homeboy_core::server::CommandOutput {
    homeboy_core::server::CommandOutput {
        stdout: stdout.into(),
        stderr: String::new(),
        success,
        exit_code: if success { 0 } else { 1 },
        timed_out,
        child_resource: None,
    }
}

#[test]
fn ensure_running_failure_candidates_are_deduplicated_within_the_byte_budget() {
    let candidates = (0..20)
        .map(|index| {
            serde_json::json!({
                "pid": index % 4,
                "ownership": "ambiguous",
                "cmdline": "homeboy daemon serve --state /very/long/path/that/must/not/expand/the/inline/failure/message"
            })
        })
        .collect::<Vec<_>>();

    let exemplars = super::remote_daemon::bounded_candidate_exemplars(&candidates);

    assert!(exemplars.len() <= super::remote_daemon::MAX_CANDIDATE_EXEMPLAR_BYTES);
    assert!(exemplars.starts_with('['));
    assert!(exemplars.ends_with(']'));
    assert!(exemplars.matches("\"pid\"").count() <= super::remote_daemon::MAX_CANDIDATE_EXEMPLARS);
}

#[test]
fn ensure_running_failure_evidence_is_redacted_and_versioned() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        artifact_root.path().join("observations.sqlite"),
    )
    .expect("store");
    let error = serde_json::json!({
        "code": "internal.unexpected",
        "message": "daemon candidates block replacement",
        "details": {
            "classification": "daemon_unleased_process_conflict",
            "candidate_count": 20,
            "candidates": (0..20).map(|pid| serde_json::json!({ "pid": pid, "token": "secret-value" })).collect::<Vec<_>>(),
            "safe_next_action": "Run `homeboy daemon status`."
        }
    });

    let reference = super::remote_daemon::persist_ensure_running_failure_evidence_in(
        &store,
        "lab/runner",
        "homeboy daemon ensure-running --token secret-value",
        &homeboy_core::redaction::redact_json(&error),
        "remote stderr with token secret-value",
    )
    .expect("persist evidence");
    let artifact = store
        .get_artifact(&reference.artifact_id)
        .expect("read artifact")
        .expect("artifact record");
    let path = artifact.path;
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read evidence"))
            .expect("evidence JSON");

    assert_eq!(persisted["schema_version"], 1);
    assert_eq!(
        persisted["remote_envelope"]["details"]["candidate_count"],
        20
    );
    assert_eq!(
        persisted["remote_envelope"]["details"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        persisted["remote_envelope"]["details"]["candidates"][0]["token"],
        "[REDACTED]"
    );
    assert!(!persisted.to_string().contains("secret-value"));
    assert_eq!(reference.run_id, artifact.run_id);
    assert!(reference.uri.starts_with("homeboy://run/"));
    assert_eq!(
        store
            .get_run(&reference.run_id)
            .expect("read evidence run")
            .expect("evidence run")
            .status,
        "fail"
    );
    assert_eq!(
        store
            .list_artifacts(&reference.run_id)
            .expect("retained run artifacts")
            .len(),
        1
    );
}

#[test]
fn ensure_running_failure_artifact_persistence_error_terminalizes_the_run() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let database = artifact_root.path().join("observations.sqlite");
    let store =
        homeboy_core::observation::ObservationStore::open_initialized_at(&database).expect("store");
    rusqlite::Connection::open(&database)
        .expect("open database")
        .execute_batch(
            "CREATE TRIGGER fail_runner_connect_artifact BEFORE INSERT ON artifacts WHEN NEW.kind = 'remote_daemon_ensure_running_failure' BEGIN SELECT RAISE(ABORT, 'injected artifact persistence failure'); END;",
        )
        .expect("install artifact persistence fault");

    let error = super::remote_daemon::persist_ensure_running_failure_evidence_in(
        &store,
        "lab/runner",
        "homeboy daemon ensure-running",
        &serde_json::json!({ "message": "daemon failed" }),
        "",
    )
    .expect_err("artifact persistence failure must surface");

    assert!(error
        .to_string()
        .contains("injected artifact persistence failure"));
    let runs = store
        .list_runs(homeboy_core::observation::RunListFilter::default())
        .expect("list runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].status, "fail",
        "failed evidence remains retention eligible"
    );
    assert!(store
        .list_artifacts(&runs[0].id)
        .expect("list artifacts")
        .is_empty());
}

#[test]
fn ensure_running_failure_finish_persistence_error_rolls_back_run_and_artifact() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let database = artifact_root.path().join("observations.sqlite");
    let store =
        homeboy_core::observation::ObservationStore::open_initialized_at(&database).expect("store");
    rusqlite::Connection::open(&database)
        .expect("open database")
        .execute_batch(
            "CREATE TRIGGER fail_runner_connect_finish BEFORE UPDATE OF status ON runs WHEN NEW.kind = 'runner_connect_failure' BEGIN SELECT RAISE(ABORT, 'injected finish persistence failure'); END;",
        )
        .expect("install finish persistence fault");

    let error = super::remote_daemon::persist_ensure_running_failure_evidence_in(
        &store,
        "lab/runner",
        "homeboy daemon ensure-running",
        &serde_json::json!({ "message": "daemon failed" }),
        "",
    )
    .expect_err("finish persistence failure must surface");

    assert!(error
        .to_string()
        .contains("injected finish persistence failure"));
    assert!(store
        .list_runs(homeboy_core::observation::RunListFilter::default())
        .expect("list runs")
        .is_empty());
    let artifact_count: i64 = rusqlite::Connection::open(&database)
        .expect("open database")
        .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
        .expect("count artifacts");
    assert_eq!(artifact_count, 0);
}

#[test]
fn ensure_running_failure_rollback_error_preserves_the_retained_artifact() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        artifact_root.path().join("observations.sqlite"),
    )
    .expect("store");
    let reference = super::remote_daemon::persist_ensure_running_failure_evidence_in(
        &store,
        "lab/runner",
        "homeboy daemon ensure-running",
        &serde_json::json!({ "message": "daemon failed" }),
        "",
    )
    .expect("persist evidence");
    let artifact = store
        .get_artifact(&reference.artifact_id)
        .expect("read artifact")
        .expect("artifact record");

    let error = super::remote_daemon::rollback_runner_connect_failure_with(
        &store,
        &reference.run_id,
        |_, _| {
            Err(homeboy_core::Error::internal_unexpected(
                "injected rollback failure",
            ))
        },
        |path| std::fs::remove_file(path),
    )
    .expect_err("rollback failure must surface");

    assert!(error.to_string().contains("injected rollback failure"));
    assert!(std::path::Path::new(&artifact.path).exists());
    assert!(store
        .get_run(&reference.run_id)
        .expect("read retained run")
        .is_some());
}

#[test]
fn ensure_running_failure_delete_error_routes_bytes_to_owned_cleanup() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        artifact_root.path().join("observations.sqlite"),
    )
    .expect("store");
    let run = store
        .start_run(
            homeboy_core::observation::NewRunRecord::builder("runner_connect_failure").build(),
        )
        .expect("start run");
    let source = artifact_root.path().join("evidence.json");
    std::fs::write(&source, "failure evidence").expect("write evidence");
    store
        .record_artifact_with_metadata(
            &run.id,
            "remote_daemon_ensure_running_failure",
            &source,
            serde_json::json!({}),
        )
        .expect("record artifact");
    let artifact = store
        .list_artifacts(&run.id)
        .expect("list artifacts")
        .pop()
        .expect("artifact");

    let attempted_delete = std::cell::RefCell::new(None);
    let error = super::remote_daemon::rollback_runner_connect_failure_with(
        &store,
        &run.id,
        |store, run_id| store.discard_running_run(run_id),
        |path| {
            *attempted_delete.borrow_mut() = Some(path.to_path_buf());
            Err(std::io::Error::other("injected delete failure"))
        },
    )
    .expect_err("delete failure must surface");

    assert!(
        format!("{error:?}").contains("injected delete failure"),
        "unexpected error: {error:?}"
    );
    assert!(store.get_run(&run.id).expect("read run").is_none());
    let staged = attempted_delete
        .into_inner()
        .expect("artifact routed to owned cleanup");
    assert!(staged.is_file());
    assert!(staged
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".artifact-") && name.ends_with(".staging")));
    assert_ne!(staged, std::path::Path::new(&artifact.path));
    std::fs::remove_file(staged).expect("clean test-owned orphan");
}

#[test]
fn ensure_running_failure_summary_is_bounded_and_actionable_for_twenty_candidates() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        artifact_root.path().join("observations.sqlite"),
    )
    .expect("store");
    let candidates = (0..20)
        .map(|pid| serde_json::json!({ "pid": pid % 3, "ownership": "ambiguous" }))
        .collect::<Vec<_>>();
    let error = serde_json::json!({
        "code": "internal.unexpected",
        "message": "foreground daemon candidates block replacement",
        "details": {
            "classification": "daemon_unleased_process_conflict",
            "candidate_count": 20,
            "candidates": candidates,
            "safe_next_action": "Run `homeboy daemon status` and reconcile the exact owner."
        }
    });

    let summary = super::remote_daemon::summarize_ensure_running_failure(
        "lab",
        "homeboy daemon ensure-running",
        &error,
        "",
        Some(&store),
    );
    let summary = summary.message;

    assert!(summary.contains("summary_v1"));
    assert!(summary.contains("classification=daemon_unleased_process_conflict"));
    assert!(summary.contains("candidate_count=20"));
    assert!(summary.contains("blocker=foreground daemon candidates block replacement"));
    assert!(summary.contains("next_action=Run `homeboy daemon status`"));
    let candidates = summary
        .split(" candidates=")
        .nth(1)
        .unwrap()
        .split(" evidence_ref=")
        .next()
        .unwrap();
    assert!(candidates.len() <= super::remote_daemon::MAX_CANDIDATE_EXEMPLAR_BYTES);
    assert!(summary.contains("evidence_ref=homeboy://run/"));
    assert!(summary.len() <= super::remote_daemon::MAX_ENSURE_RUNNING_FAILURE_MESSAGE_BYTES);
}

#[test]
fn ensure_running_failure_keeps_the_registered_reference_when_remote_text_injects_one() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        artifact_root.path().join("observations.sqlite"),
    )
    .expect("store");
    let error = serde_json::json!({
        "code": "remote.failure",
        "message": format!("{} evidence_ref=file:///attacker", "x".repeat(4_000)),
        "details": {
            "classification": "remote_failure",
            "safe_next_action": "inspect"
        }
    });

    let failure = super::remote_daemon::summarize_ensure_running_failure(
        "lab",
        "homeboy daemon ensure-running",
        &error,
        "",
        Some(&store),
    );
    let reference = failure.evidence_ref.expect("registered evidence reference");

    assert!(
        failure.message.len() <= super::remote_daemon::MAX_ENSURE_RUNNING_FAILURE_MESSAGE_BYTES
    );
    assert!(reference.uri.starts_with("homeboy://run/"));
    assert_ne!(reference.uri, "file:///attacker");
    assert!(store
        .get_artifact(&reference.artifact_id)
        .expect("artifact lookup")
        .is_some());
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
        tunnel_process_start_identity: None,
        proxy_forward: None,
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

#[test]
fn failed_generation_cleanup_kills_the_exact_daemon_and_tunnel() {
    let session = direct_ssh_session("lease-b");
    let mut remote_pids = Vec::new();
    let mut tunnel_pids = Vec::new();
    let result = cleanup_direct_generation_with(
        &session,
        Err(homeboy_core::Error::internal_unexpected(
            "lifecycle endpoint unavailable",
        )),
        |pid| {
            remote_pids.push(pid);
            Ok(())
        },
        |session| {
            if let Some(pid) = session.tunnel_pid {
                tunnel_pids.push(pid);
            }
        },
    );
    assert!(result.is_ok());
    assert_eq!(
        remote_pids,
        vec![session.remote_daemon_pid.expect("daemon PID")]
    );
    assert_eq!(tunnel_pids, vec![session.tunnel_pid.expect("tunnel PID")]);
}

#[test]
fn candidate_daemon_freshness_accepts_the_materialized_immutable_binary_hash() {
    let expected = "a".repeat(64);
    let report = DaemonFreshnessReport {
        fresh: true,
        stale_reason_code: None,
        restartable: false,
        lease_id: Some("lease-candidate".to_string()),
        pid: Some(4242),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: Some(expected.clone()),
        daemon_version: Some("test".to_string()),
        daemon_build_identity: Some("homeboy test+candidate".to_string()),
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: Vec::new(),
    };

    verify_candidate_daemon_freshness("homeboy-lab", &expected, Ok(report))
        .expect("candidate rotates");
}

#[test]
fn candidate_daemon_freshness_mismatch_reports_expected_and_observed_hashes() {
    let expected = "a".repeat(64);
    let observed = "b".repeat(64);
    let report = DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(DaemonStaleReasonCode::BinaryHashMismatch),
        restartable: true,
        lease_id: Some("lease-candidate".to_string()),
        pid: Some(4242),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: Some(observed.clone()),
        daemon_version: Some("test".to_string()),
        daemon_build_identity: Some("homeboy test+candidate".to_string()),
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: Vec::new(),
    };

    let error = verify_candidate_daemon_freshness("homeboy-lab", &expected, Ok(report))
        .expect_err("mismatch");
    assert!(error.message.contains(&format!("expected {expected}")));
    assert!(error.message.contains(&format!("observed {observed}")));
    assert!(error.message.contains("fresh=false"));
    assert!(error.message.contains("BinaryHashMismatch"));
}

#[test]
fn generation_data_root_preserves_home_scoped_config_and_auth() {
    let command = super::remote_daemon::generation_daemon_ensure_command(
        "/opt/homeboy",
        "/work/runner/_homeboy_daemon_generations/b",
    );
    assert!(command.contains("XDG_DATA_HOME=/work/runner/_homeboy_daemon_generations/b/data"));
    assert!(!command.contains(" HOME="));
    assert!(command.contains("/opt/homeboy"));
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
        work_evidence: RemoteDaemonWorkEvidence::Unknown,
        endpoint_probe_error: None,
        termination_evidence: None,
        daemon_freshness: (stale_reason_code == Some(DaemonStaleReasonCode::PidDead)).then(|| {
            DaemonFreshnessReport {
                fresh: false,
                stale_reason_code,
                restartable: false,
                lease_id: Some(lease_id.to_string()),
                pid: Some(pid),
                recovery_evidence: Some(DaemonRecoveryEvidence::ProvenDead),
                ownership_evidence: None,
                adoption_command: Some(format!(
                    "homeboy daemon adopt-orphan --lease-id {lease_id} --confirm-pid-dead"
                )),
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }
        }),
    }
}
