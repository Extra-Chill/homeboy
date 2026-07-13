use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use super::{
    artifact_content_url, ensure_running_with_operations, fetch_artifact_to_path,
    reconcile_dead_lease_and_ensure_running_with_operations,
};
use crate::core::api_jobs::{JobEventKind, JobStatus, JobStore};
use crate::core::build_identity::BuildIdentity;
use crate::core::daemon::{
    DaemonFreshnessReport, DaemonRuntimeSnapshot, DaemonStaleReasonCode, DaemonState, DaemonStatus,
};
use crate::test_support::with_isolated_home;

#[derive(Default)]
struct FakeEnsureState {
    daemon: Option<super::DaemonStartResult>,
    starts: usize,
}

#[test]
fn artifact_content_url_builds_encoded_daemon_byte_alias() {
    let url = artifact_content_url(
        "http://127.0.0.1:7421/base?ignored=true",
        "run 1",
        "report/summary.json",
    )
    .expect("url");

    assert_eq!(
        url,
        "http://127.0.0.1:7421/runs/run%201/artifacts/report%2Fsummary.json/content"
    );
}

#[test]
fn fetch_artifact_to_path_downloads_daemon_byte_alias() {
    with_isolated_home(|home| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 1024];
            let bytes = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request
                .starts_with("GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                )
                .expect("response");
        });
        let output = home.path().join("summary.json");

        let outcome = fetch_artifact_to_path(
            "run-1",
            "report/summary.json",
            Some(format!("http://{addr}")),
            Some(output.clone()),
        )
        .expect("artifact get");

        server.join().expect("server");
        assert_eq!(outcome.content_type.as_deref(), Some("application/json"));
        assert_eq!(outcome.size_bytes, 11);
        assert_eq!(outcome.sha256.as_deref(), Some("abc123"));
        assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
    });
}

#[test]
fn ensure_running_times_out_when_lifecycle_lock_remains_held() {
    with_isolated_home(|_| {
        let _lock = super::super::acquire_daemon_operation_lock().expect("hold lifecycle lock");

        let err = ensure_running_with_operations(
            Duration::from_millis(10),
            super::super::acquire_daemon_operation_lock_for_ensure,
            || unreachable!("lock acquisition should time out first"),
            |_| unreachable!("lock acquisition should time out first"),
            || unreachable!("lock acquisition should time out first"),
        )
        .expect_err("ensure should time out behind held lock");

        assert!(err.message.contains("timed out"));
        assert!(err.message.contains("ensure-running lifecycle lock"));
    });
}

#[test]
fn ensure_running_returns_stale_live_daemon_without_starting_duplicate() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-stale");
        let state = Arc::new(Mutex::new(FakeEnsureState {
            daemon: Some(daemon.clone()),
            starts: 0,
        }));
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);

        let attached = ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    false,
                ))
            },
            |pid| pid == daemon.pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                Ok(fake_daemon(4343, "unexpected-replacement"))
            },
        )
        .expect("attach stale live daemon");

        assert_eq!(attached, daemon);
        assert_eq!(state.lock().expect("state").starts, 0);
    });
}

#[test]
fn ensure_running_concurrent_callers_converge_on_same_daemon() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-shared");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first =
            ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon.clone());
        let second = ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();

        let first = first.join().expect("first thread").expect("first ensure");
        let second = second
            .join()
            .expect("second thread")
            .expect("second ensure");
        assert_eq!(first.pid, second.pid);
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(first.address, second.address);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

#[test]
fn dead_matching_lease_reconciles_jobs_before_starting_replacement() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("daemon jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("create durable store")
            .with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("job starts");
        let daemon = fake_daemon(4343, "lease-replacement");

        let started = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
            |_| false,
            || super::reconcile_dead_daemon_lease_jobs("lease-dead"),
            || {
                let recovered = JobStore::open_without_reconciliation(&path)
                    .expect("read reconciled jobs before start");
                assert_eq!(
                    recovered.get(job.id).expect("reconciled job").status,
                    JobStatus::Failed,
                    "scoped reconciliation completes before replacement startup"
                );
                Ok(daemon.clone())
            },
        )
        .expect("dead lease is reconciled and replaced");

        assert_eq!(started.lease_id, "lease-replacement");
        let reconciled =
            JobStore::open_without_reconciliation(&path).expect("read reconciled durable store");
        let orphaned = reconciled.get(job.id).expect("durable job persists");
        assert_eq!(orphaned.status, JobStatus::Failed);
        assert_eq!(
            orphaned.stale_reason.as_deref(),
            Some("daemon lease owner process was not running")
        );
        let events = reconciled
            .events(job.id)
            .expect("durable evidence persists");
        let classification = events
            .iter()
            .find_map(|event| {
                (event.kind == JobEventKind::Error)
                    .then(|| event.data.as_ref()?.get("classification"))?
            })
            .expect("control-plane-loss classification");
        assert_eq!(classification["kind"], "orphaned_after_control_plane_loss");
    });
}

#[test]
fn legacy_unowned_job_blocks_replacement_start() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("daemon jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("create durable store");
        let job = store.create("runner.exec");
        store.start(job.id).expect("job starts");

        let error = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
            |_| false,
            || super::reconcile_dead_daemon_lease_jobs("lease-dead"),
            || unreachable!("legacy job must prevent replacement startup"),
        )
        .expect_err("legacy job blocks recovery");

        assert!(error.message.contains("legacy unowned active job"));
        assert!(error.message.contains(&job.id.to_string()));
    });
}

#[test]
fn dead_lease_reconciliation_reattaches_live_or_refuses_mismatched_daemon() {
    let live_daemon = fake_daemon(4242, "lease-dead");
    let live = reconcile_dead_lease_and_ensure_running_with_operations(
        Duration::from_millis(50),
        super::super::acquire_daemon_operation_lock_for_ensure,
        "lease-dead",
        || Ok(fake_dead_status(live_daemon.clone())),
        |_| true,
        || unreachable!("live daemon must not reconcile"),
        || unreachable!("live daemon must not start replacement"),
    )
    .expect("live replacement from a concurrent reconnect is reattached");
    assert_eq!(live, live_daemon);

    let mismatched = reconcile_dead_lease_and_ensure_running_with_operations(
        Duration::from_millis(50),
        super::super::acquire_daemon_operation_lock_for_ensure,
        "lease-expected",
        || Ok(fake_dead_status(fake_daemon(4242, "lease-other"))),
        |_| false,
        || unreachable!("mismatched lease must not reconcile"),
        || unreachable!("mismatched lease must not start replacement"),
    )
    .expect_err("mismatched lease must fail closed");
    assert!(mismatched
        .message
        .contains("does not match expected stale lease"));
}

fn ensure_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::core::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    true,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                state.daemon = Some(daemon.clone());
                Ok(daemon)
            },
        )
    })
}

fn fake_daemon(pid: u32, lease_id: &str) -> super::DaemonStartResult {
    super::DaemonStartResult {
        pid,
        address: "127.0.0.1:49152".to_string(),
        state_path: "/fake/daemon-state.json".to_string(),
        lease_id: lease_id.to_string(),
    }
}

fn fake_status(daemon: Option<super::DaemonStartResult>, fresh: bool) -> DaemonStatus {
    let stale_reason_code = (!fresh).then_some(DaemonStaleReasonCode::VersionMismatch);
    DaemonStatus {
        running: fresh,
        fresh,
        reachable: true,
        freshness: DaemonFreshnessReport {
            fresh,
            stale_reason_code,
            restartable: !fresh,
            lease_id: daemon.as_ref().map(|daemon| daemon.lease_id.clone()),
            pid: daemon.as_ref().map(|daemon| daemon.pid),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            runtime_paths: None,
            active_jobs: 0,
            repair_plan: Vec::new(),
        },
        stale_reason: (!fresh).then(|| "simulated stale daemon".to_string()),
        state: daemon.map(fake_daemon_state),
        state_path: "/fake/daemon-state.json".to_string(),
    }
}

fn fake_dead_status(daemon: super::DaemonStartResult) -> DaemonStatus {
    DaemonStatus {
        running: false,
        fresh: false,
        reachable: false,
        freshness: DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(DaemonStaleReasonCode::PidDead),
            restartable: false,
            lease_id: Some(daemon.lease_id.clone()),
            binary_hash: None,
            runtime_paths: None,
            active_jobs: 1,
            repair_plan: Vec::new(),
        },
        stale_reason: Some("daemon lease pid is not running".to_string()),
        state: Some(fake_daemon_state(daemon)),
        state_path: "/fake/daemon-state.json".to_string(),
    }
}

fn fake_daemon_state(daemon: super::DaemonStartResult) -> DaemonState {
    DaemonState {
        schema: "homeboy.daemon.session_lease.v1".to_string(),
        lease_id: daemon.lease_id,
        startup_token: "fake-startup-token".to_string(),
        address: daemon.address,
        pid: daemon.pid,
        state_path: daemon.state_path,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        last_seen_at: "2026-01-01T00:00:00Z".to_string(),
        build_identity: BuildIdentity {
            version: "test".to_string(),
            git_commit: None,
            git_dirty: None,
            display: "homeboy test".to_string(),
        },
        binary_sha256: None,
        runtime_paths: DaemonRuntimeSnapshot {
            loaded_at: "2026-01-01T00:00:00Z".to_string(),
            paths: Vec::new(),
        },
    }
}
