use super::*;
use crate::api_jobs::{JobEventKind, JobStatus, JobStore};
use crate::observation::{ArtifactRecord, NewRunRecord, ObservationStore};
use crate::test_support::HomeGuard;
use base64::Engine;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

const DAEMON_TEST_RESPONSE_LIMIT_BYTES: u64 = 64 * 1024;

fn serialized_contains(value: &serde_json::Value, needle: &str) -> bool {
    serde_json::to_string(value)
        .expect("serialize test value")
        .contains(needle)
}

fn daemon_state_for_test(pid: u32, address: &str) -> DaemonState {
    let path = state_path().expect("state path");
    let now = chrono::Utc::now().to_rfc3339();
    DaemonState {
        schema: DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: "test-lease".to_string(),
        startup_token: "test-token".to_string(),
        address: address.to_string(),
        pid,
        state_path: path.display().to_string(),
        started_at: now.clone(),
        last_seen_at: now,
        build_identity: build_identity::current(),
        binary_sha256: current_binary_sha256().expect("binary hash"),
        runtime_paths: capture_daemon_runtime_snapshot(),
    }
}

fn write_daemon_state_for_test(state: &DaemonState) {
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, serde_json::to_string(state).expect("state json")).expect("write state");
}

#[test]
fn status_reports_active_job_recovery_evidence_without_mutating_the_store() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(u32::MAX, "127.0.0.1:49152");
    state.lease_id = "evidence-lease".to_string();
    write_daemon_state_for_test(&state);
    let path = crate::paths::daemon_jobs_file().expect("jobs path");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("store")
        .with_daemon_lease(state.lease_id.clone());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");
    let before = std::fs::read(&path).expect("store bytes");

    let status = read_status().expect("status");

    assert_eq!(std::fs::read(&path).expect("store bytes"), before);
    assert_eq!(status.active_job_recovery_evidence.len(), 1);
    let evidence = &status.active_job_recovery_evidence[0];
    assert_eq!(evidence.job_id, job.id);
    assert_eq!(evidence.operation, "runner.exec");
    assert_eq!(
        evidence.disposition,
        crate::api_jobs::DaemonActiveJobRecoveryDisposition::MissingChildIdentityRecoverable
    );
}

#[test]
fn status_marks_pidless_jobs_non_recoverable_while_the_lease_is_live() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.lease_id = "live-evidence-lease".to_string();
    write_daemon_state_for_test(&state);
    let path = crate::paths::daemon_jobs_file().expect("jobs path");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("store")
        .with_daemon_lease(state.lease_id);
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");

    let status = read_status().expect("status");

    assert_eq!(status.active_job_recovery_evidence.len(), 1);
    assert_eq!(
        status.active_job_recovery_evidence[0].disposition,
        crate::api_jobs::DaemonActiveJobRecoveryDisposition::BlockingAmbiguous
    );
}

fn write_legacy_daemon_state_for_test(pid: u32, address: &str) -> (std::path::PathBuf, String) {
    let path = state_path().expect("state path");
    let content = serde_json::json!({
        "address": address,
        "pid": pid,
        "state_path": path.display().to_string(),
    })
    .to_string();
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, &content).expect("write legacy state");
    (path, content)
}

fn create_local_runner_for_file_api(id: &str, workspace_root: &std::path::Path) {
    // Write the runner config directly (the runner subsystem's `create` lives in
    // the homeboy-runner crate, which core tests cannot depend on). A runner is a
    // config entity stored at `<homeboy>/runners/<id>.json`.
    write_runner_config(
        id,
        &serde_json::json!({
            "id": id,
            "kind": "local",
            "workspace_root": workspace_root.display().to_string(),
        }),
    );
}

fn write_runner_config(id: &str, value: &serde_json::Value) {
    let dir = crate::paths::homeboy()
        .expect("homeboy dir")
        .join("runners");
    std::fs::create_dir_all(&dir).expect("create runners dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(value).expect("serialize runner"),
    )
    .expect("write runner config");
}

fn file_api_workspace(configure_runner: bool) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    if configure_runner {
        create_local_runner_for_file_api("file-lab", &workspace);
    }
    (temp, workspace)
}

#[test]
fn parse_bind_addr_defaults_to_loopback_shape() {
    let addr = parse_bind_addr(DEFAULT_ADDR).expect("parse default");

    assert!(addr.ip().is_loopback());
    assert_eq!(addr.port(), 0);
}

#[test]
fn parse_bind_addr_rejects_public_bind() {
    let err = parse_bind_addr("0.0.0.0:8080").expect_err("reject public bind");

    assert!(err.message.contains("loopback"));
}

#[test]
fn state_path_uses_daemon_state_location() {
    let _home = HomeGuard::new();

    let path = state_path().expect("state path");

    assert!(path.ends_with("daemon/state.json"));
}

#[test]
fn write_state_establishes_daemon_session_lease_identity() {
    let _home = HomeGuard::new();

    let state = write_state("127.0.0.1:49152".parse().expect("addr")).expect("write lease");
    let status = read_status().expect("status");

    assert_eq!(state.schema, DAEMON_LEASE_SCHEMA);
    assert!(!state.lease_id.is_empty());
    assert_eq!(state.startup_token, "");
    assert_eq!(
        state.build_identity.display,
        build_identity::current().display
    );
    assert!(state
        .binary_sha256
        .as_deref()
        .is_some_and(|hash| hash.len() == 64));
    assert!(status.running);
    assert!(status.fresh);
    assert!(status.reachable);
    assert_eq!(status.state.expect("state").lease_id, state.lease_id);
}

#[test]
fn lease_writer_rejects_an_unsupported_schema() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.schema = "homeboy.daemon.session_lease.v0".to_string();

    let error = write_lease(&state_path().expect("state path"), &state)
        .expect_err("unsupported schema is not persisted");

    assert!(error.message.contains("unsupported schema"));
}

#[test]
fn lease_writer_rejects_a_missing_lease_id() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.lease_id.clear();

    let error = write_lease(&state_path().expect("state path"), &state)
        .expect_err("leases without an identity are not persisted");

    assert!(error.message.contains("without a lease ID"));
}

#[test]
fn health_route_refreshes_daemon_lease_heartbeat() {
    let _home = HomeGuard::new();
    let mut state = write_state("127.0.0.1:49152".parse().expect("addr")).expect("write lease");
    state.last_seen_at = "2000-01-01T00:00:00Z".to_string();
    write_daemon_state_for_test(&state);

    let response = route("GET", "/health");
    let refreshed = read_status().expect("status").state.expect("state");

    assert_eq!(response.status_code, 200);
    assert_ne!(refreshed.last_seen_at, state.last_seen_at);
}

#[test]
fn file_route_rejects_paths_outside_runner_workspace_root() {
    let _home = HomeGuard::new();
    let (_temp, workspace) = file_api_workspace(true);

    let response = route_with_job_store_and_body(
        "POST",
        "/files/download",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": workspace.join("..").join("secret.txt").display().to_string(),
        })),
        &JobStore::default(),
    );

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["details"]["field"], "path");
    assert!(response.body["message"]
        .as_str()
        .expect("message")
        .contains("workspace_root"));
}

#[test]
fn file_route_requires_broker_submit_auth_for_untrusted_requests() {
    let _home = HomeGuard::new();
    let (_temp, _workspace) = file_api_workspace(true);

    let response = route_with_job_store_and_body_and_runner_and_auth(
        "POST",
        "/files/download",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": "report.json",
        })),
        &JobStore::default(),
        UnsupportedAnalysisJobRunner,
        remote_runner::BrokerAuthContext {
            token: None,
            loopback_bind: false,
            trusted_local: false,
        },
    );

    assert_eq!(response.status_code, 401);
    assert_eq!(response.body["error"], "broker.auth_denied");
}

#[test]
fn file_route_accepts_request_workspace_root_without_runner_config() {
    let _home = HomeGuard::new();
    let (_temp, workspace) = file_api_workspace(false);

    let response = route_with_job_store_and_body(
        "POST",
        "/files/mkdir",
        Some(serde_json::json!({
            "runner_id": "file-lab-without-local-config",
            "workspace_root": workspace.display().to_string(),
            "path": workspace.join("artifacts").display().to_string(),
        })),
        &JobStore::default(),
    );

    assert_eq!(response.status_code, 200, "mkdir body: {}", response.body);
    assert!(workspace.join("artifacts").is_dir());
}

#[test]
fn file_routes_upload_and_download_inside_runner_workspace_root() {
    let _home = HomeGuard::new();
    let (_temp, workspace) = file_api_workspace(true);

    let upload = route_with_job_store_and_body(
        "POST",
        "/files/upload",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": "nested/report.json",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(br#"{"ok":true}"#),
        })),
        &JobStore::default(),
    );
    assert_eq!(upload.status_code, 200, "upload body: {}", upload.body);
    assert_eq!(
        std::fs::read_to_string(workspace.join("nested/report.json")).expect("uploaded file"),
        r#"{"ok":true}"#
    );

    let download = route_with_job_store_and_body(
        "POST",
        "/files/download",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": workspace.join("nested/report.json").display().to_string(),
        })),
        &JobStore::default(),
    );
    assert_eq!(
        download.status_code, 200,
        "download body: {}",
        download.body
    );
    let encoded = download.body["body"]["content_base64"]
        .as_str()
        .expect("content_base64");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("decode content");
    assert_eq!(decoded, br#"{"ok":true}"#);
}

#[test]
fn read_status_classifies_unknown_binary_freshness_as_stale() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.binary_sha256 = Some("unknown".to_string());
    write_daemon_state_for_test(&state);

    let status = read_status().expect("status");

    assert!(!status.running);
    assert!(!status.fresh);
    assert!(status.reachable);
    assert!(status
        .stale_reason
        .as_deref()
        .unwrap_or_default()
        .contains("binary hash"));
    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::BinaryHashMismatch)
    );
}

#[test]
fn freshness_report_classifies_missing_lease() {
    let _home = HomeGuard::new();

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseMissing)
    );
    assert!(status.freshness.restartable);
}

#[test]
fn freshness_report_classifies_corrupt_lease() {
    let _home = HomeGuard::new();
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, "not-json").expect("write corrupt lease");

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseCorrupt)
    );
    assert!(!status.freshness.restartable);
}

#[test]
fn freshness_report_classifies_schema_mismatch() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.schema = "homeboy.daemon.session_lease.v0".to_string();
    write_daemon_state_for_test(&state);

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseSchemaMismatch)
    );
}

#[test]
fn freshness_report_classifies_version_mismatch() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.build_identity.version = "0.0.0".to_string();
    state.build_identity.display = "homeboy 0.0.0".to_string();
    write_daemon_state_for_test(&state);

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::VersionMismatch)
    );
}

#[test]
fn freshness_report_classifies_runtime_paths_drift() {
    let _home = HomeGuard::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime_path = temp.path().join("runtime");
    std::fs::write(&runtime_path, "loaded").expect("runtime file");
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.runtime_paths.paths.push(DaemonRuntimePathSnapshot {
        env: "HOMEBOY_TEST_RUNTIME_PATH".to_string(),
        path: runtime_path.display().to_string(),
        fingerprint: "old".to_string(),
    });
    write_daemon_state_for_test(&state);

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::RuntimePathsDrift)
    );
}

#[test]
fn test_pid_is_running_rejects_impossible_pid_and_drives_status() {
    let _home = HomeGuard::new();
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(
        &path,
        serde_json::json!({
            "address": "127.0.0.1:49152",
            "pid": u32::MAX,
            "state_path": path.display().to_string(),
        })
        .to_string(),
    )
    .expect("write state");

    assert!(pid_is_running(std::process::id()));
    assert!(!pid_is_running(u32::MAX));
    let status = read_status().expect("status");
    assert!(!status.running);
    assert!(!status.fresh);
    // A state file that exists but omits the required `lease_id` is now
    // classified as a corrupt lease (missing field), reported through the
    // structured code with a descriptive parse reason — not a missing lease.
    assert!(status
        .stale_reason
        .as_deref()
        .unwrap_or_default()
        .contains("invalid daemon lease"));
    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseCorrupt)
    );
}

#[test]
fn status_retains_dead_missing_schema_lease_identity_for_explicit_recovery() {
    let _home = HomeGuard::new();
    let state = daemon_state_for_test(u32::MAX, "127.0.0.1:49152");
    let path = state_path().expect("state path");
    let mut lease = serde_json::to_value(state).expect("serialize lease");
    lease
        .as_object_mut()
        .expect("lease object")
        .remove("schema");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, serde_json::to_string(&lease).expect("lease json")).expect("write state");

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::PidDead)
    );
    assert_eq!(
        status.state.as_ref().map(|state| state.lease_id.as_str()),
        Some("test-lease")
    );
    assert_eq!(status.freshness.lease_id.as_deref(), Some("test-lease"));
    assert!(path.exists());
}

#[test]
fn status_keeps_corrupt_lease_fail_closed_without_identity() {
    let _home = HomeGuard::new();
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, "{invalid json").expect("write corrupt state");

    let status = read_status().expect("status");

    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseCorrupt)
    );
    assert!(status.state.is_none());
    assert!(status.freshness.lease_id.is_none());
}

#[test]
fn status_treats_dead_known_legacy_lease_as_missing_metadata_for_recovery() {
    let _home = HomeGuard::new();
    let (path, _) = write_legacy_daemon_state_for_test(u32::MAX, "127.0.0.1:1");

    let status = read_status().expect("status");

    // A legacy state that predates the lease schema lacks the required
    // `lease_id`, so it is now surfaced as a corrupt lease (missing field)
    // with a descriptive parse reason rather than the old "no schema" text.
    assert!(status
        .stale_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("invalid daemon lease")));
    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::LeaseCorrupt)
    );
    assert!(status.state.is_none());
    assert!(status.freshness.lease_id.is_none());
    assert!(path.exists());
}

#[test]
fn start_repair_archives_dead_known_legacy_lease() {
    let _home = HomeGuard::new();
    let (path, content) = write_legacy_daemon_state_for_test(u32::MAX, "127.0.0.1:1");

    assert!(repair_legacy_lease_for_start().expect("repair legacy lease"));
    assert!(
        !path.exists(),
        "legacy lease should be replaced by daemon startup"
    );
    let evidence: Vec<_> = std::fs::read_dir(path.parent().expect("state parent"))
        .expect("state dir")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("state.json.legacy-lease-"))
        })
        .collect();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&evidence[0]).expect("evidence"),
        content
    );
}

#[test]
fn start_repair_refuses_live_legacy_owner_pid_or_endpoint() {
    let _home = HomeGuard::new();
    let (path, _) = write_legacy_daemon_state_for_test(std::process::id(), "127.0.0.1:1");

    let pid_error = repair_legacy_lease_for_start().expect_err("live pid is refused");
    assert_eq!(pid_error.code.as_str(), "validation.invalid_argument");
    assert!(pid_error.message.contains("still running"));
    assert!(path.exists());

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    std::fs::write(
        &path,
        serde_json::json!({
            "address": listener.local_addr().expect("address").to_string(),
            "pid": u32::MAX,
            "state_path": path.display().to_string(),
        })
        .to_string(),
    )
    .expect("write legacy state");

    let endpoint_error = repair_legacy_lease_for_start().expect_err("live endpoint is refused");
    assert_eq!(endpoint_error.code.as_str(), "validation.invalid_argument");
    assert!(endpoint_error.message.contains("still reachable"));
    assert!(path.exists());
}

#[test]
fn start_repair_refuses_unknown_corrupt_lease_with_actionable_diagnostic() {
    let _home = HomeGuard::new();
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(&path, r#"{"pid":4294967295,"broken":true}"#).expect("write corrupt lease");

    let error = repair_legacy_lease_for_start().expect_err("unknown corrupt lease is refused");

    assert_eq!(error.code.as_str(), "validation.invalid_argument");
    assert!(error
        .message
        .contains("neither the current schema nor the known pre-schema shape"));
    assert!(serialized_contains(&error.details, "Inspect"));
    assert!(
        path.exists(),
        "unknown state must remain for operator inspection"
    );
}

#[test]
fn start_repair_leaves_current_schema_lease_for_normal_lifecycle() {
    let _home = HomeGuard::new();
    let state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    write_daemon_state_for_test(&state);

    assert!(!repair_legacy_lease_for_start().expect("current lease is not repaired"));
    assert_eq!(read_status().expect("status").state.expect("state"), state);
}

#[test]
fn read_status_reports_stale_state_as_not_running() {
    let _home = HomeGuard::new();
    let state = daemon_state_for_test(u32::MAX, "127.0.0.1:49152");
    write_daemon_state_for_test(&state);

    let status = read_status().expect("status");

    assert!(!status.running);
    assert!(!status.fresh);
    assert_eq!(status.state.expect("state").pid, u32::MAX);
    assert!(status
        .stale_reason
        .as_deref()
        .unwrap_or_default()
        .contains("pid is not running"));
    assert_eq!(
        status.freshness.stale_reason_code,
        Some(DaemonStaleReasonCode::PidDead)
    );
}

#[test]
fn stop_without_state_reports_noop() {
    let _home = HomeGuard::new();

    let result = stop().expect("stop");

    assert!(!result.stopped);
    assert_eq!(result.pid, None);
    assert!(result.state_path.ends_with("daemon/state.json"));
}

#[test]
fn daemon_operation_lock_rejects_concurrent_start_or_stop() {
    let _home = HomeGuard::new();

    let _held = acquire_daemon_operation_lock().expect("first lock");
    let err = acquire_daemon_operation_lock().expect_err("second lock fails");

    assert!(err
        .message
        .contains("daemon lifecycle operation already in progress"));
    assert!(err.message.contains("operation.lock"));
}

#[cfg(unix)]
#[test]
fn daemon_operation_lock_recovers_after_owner_exits_without_drop() {
    const HOLDER_ENV: &str = "HOMEBOY_TEST_DAEMON_OPERATION_LOCK_HOLDER";

    if std::env::var_os(HOLDER_ENV).is_some() {
        let ready = state_path()
            .expect("state path")
            .with_file_name("operation-lock-holder.ready");
        let release = state_path()
            .expect("state path")
            .with_file_name("operation-lock-holder.release");
        let _lock = acquire_daemon_operation_lock().expect("child acquires operation lock");
        std::fs::write(&ready, "ready").expect("signal operation lock holder");
        while !release.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Simulate an interrupted lifecycle process: no Rust destructors run.
        std::process::exit(0);
    }

    let _home = HomeGuard::new();
    let ready = state_path()
        .expect("state path")
        .with_file_name("operation-lock-holder.ready");
    let release = state_path()
        .expect("state path")
        .with_file_name("operation-lock-holder.release");
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("core::daemon::daemon_test::daemon_operation_lock_recovers_after_owner_exits_without_drop")
        .arg("--nocapture")
        .env(HOLDER_ENV, "1")
        .spawn()
        .expect("start operation lock holder");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("operation lock holder did not become ready");
    }

    let err = acquire_daemon_operation_lock().expect_err("live child excludes lifecycle operation");
    assert!(err
        .message
        .contains("daemon lifecycle operation already in progress"));

    std::fs::write(&release, "release").expect("release operation lock holder");
    assert!(child.wait().expect("wait for lock holder").success());
    acquire_daemon_operation_lock().expect("recover after interrupted lock holder");
}

#[test]
fn stop_refuses_stale_lease_that_points_at_reused_pid() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);

    let result = stop().expect("stop stale lease");
    let status = read_status().expect("status after stop");

    assert!(!result.stopped);
    assert_eq!(result.pid, Some(std::process::id()));
    assert!(pid_is_running(std::process::id()));
    assert!(
        status.state.is_some(),
        "stale lease should remain for operator inspection"
    );
}

#[cfg(unix)]
fn spawn_force_stop_test_process(startup_token: Option<&str>) -> std::process::Child {
    let mut command = Command::new("sleep");
    command.arg("30").env_clear();
    if let Some(startup_token) = startup_token {
        command.env(DAEMON_STARTUP_TOKEN_ENV, startup_token);
    }
    command.spawn().expect("start force-stop test process")
}

#[cfg(unix)]
#[test]
fn force_stop_lease_mismatch_cannot_signal_a_process() {
    let _home = HomeGuard::new();
    let mut child = spawn_force_stop_test_process(None);
    let mut state = daemon_state_for_test(child.id(), "127.0.0.1:1");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);

    let error = force_stop_for_lease("other-lease").expect_err("mismatched lease is rejected");

    assert!(error.message.contains("refusing to signal"));
    assert!(pid_is_running(child.id()));
    child.kill().expect("cleanup test process");
    child.wait().expect("reap test process");
}

#[cfg(unix)]
#[test]
fn force_stop_active_jobs_block_signaling() {
    let _home = HomeGuard::new();
    let mut child = spawn_force_stop_test_process(None);
    let mut state = daemon_state_for_test(child.id(), "127.0.0.1:1");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);
    let store = JobStore::open_without_reconciliation(
        &crate::paths::daemon_jobs_file().expect("jobs path"),
    )
    .expect("store")
    .with_daemon_lease(state.lease_id.clone());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");

    let error = force_stop_for_lease(&state.lease_id).expect_err("active job blocks force stop");

    assert_eq!(error.details["active_job_ids"], serde_json::json!([job.id]));
    assert!(pid_is_running(child.id()));
    child.kill().expect("cleanup test process");
    child.wait().expect("reap test process");
}

#[cfg(target_os = "linux")]
#[test]
fn force_stop_matching_zero_job_stale_daemon_is_terminated_and_verified() {
    let _home = HomeGuard::new();
    let startup_token = "force-stop-owned-process";
    let child = spawn_force_stop_test_process(Some(startup_token));
    let pid = child.id();
    let mut state = daemon_state_for_test(pid, "127.0.0.1:1");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    state.startup_token = startup_token.to_string();
    write_daemon_state_for_test(&state);

    let result = force_stop_for_lease(&state.lease_id).expect("force stop stale daemon");

    assert!(result.stopped);
    assert_eq!(result.pid, Some(pid));
    let evidence = result.termination_evidence.expect("termination evidence");
    assert_eq!(evidence.lease_id.as_deref(), Some(state.lease_id.as_str()));
    assert_eq!(evidence.pid, Some(pid));
    assert_eq!(evidence.signal, Some(libc::SIGTERM));
    assert!(evidence.os_evidence.contains("process death verified"));
    assert!(!pid_is_running(pid));
    assert!(!state_path().expect("state path").exists());
}

#[cfg(unix)]
#[test]
fn force_stop_matching_lease_cannot_signal_a_reused_pid() {
    let _home = HomeGuard::new();
    let mut child = spawn_force_stop_test_process(None);
    let mut state = daemon_state_for_test(child.id(), "127.0.0.1:1");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);

    force_stop_for_lease(&state.lease_id)
        .expect_err("unowned matching lease cannot signal reused pid");

    assert!(pid_is_running(child.id()));
    child.kill().expect("cleanup test process");
    child.wait().expect("reap test process");
}

#[test]
fn force_stop_default_non_force_behavior_remains_unchanged() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);

    let result = stop().expect("routine stop stale lease");

    assert!(!result.stopped);
    assert_eq!(result.pid, Some(std::process::id()));
    assert!(result.termination_evidence.is_none());
    assert!(state_path().expect("state path").exists());
}

#[test]
fn lease_bound_stop_does_not_report_live_stale_owner_as_already_absent() {
    let _home = HomeGuard::new();
    let mut state = daemon_state_for_test(std::process::id(), "127.0.0.1:49152");
    state.binary_sha256 = Some("stale-binary-hash".to_string());
    write_daemon_state_for_test(&state);

    let result = stop_for_lease(&state.lease_id).expect("lease-bound stale stop");

    assert!(!result.stopped);
    assert!(!result.already_absent);
    assert!(state_path().expect("state path").exists());
}

#[test]
fn lease_bound_stop_reconciles_an_absent_lease_idempotently_without_jobs() {
    let _home = HomeGuard::new();

    let normal = stop_for_lease("stale-lease").expect("normal stop reconciles missing lease");
    let forced = force_stop_for_lease("stale-lease").expect("forced stop replays safely");

    assert!(!normal.stopped);
    assert!(!forced.stopped);
    assert!(normal.already_absent);
    assert!(forced.already_absent);
    assert_eq!(normal.pid, None);
    assert_eq!(forced.pid, None);
}

#[test]
fn lease_bound_stop_keeps_active_jobs_protected_when_lease_is_absent() {
    let _home = HomeGuard::new();
    let store = JobStore::open_without_reconciliation(
        &crate::paths::daemon_jobs_file().expect("jobs path"),
    )
    .expect("store")
    .with_daemon_lease("stale-lease".to_string());
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");

    for result in [
        stop_for_lease("stale-lease"),
        force_stop_for_lease("stale-lease"),
    ] {
        let error = result.expect_err("active job blocks missing-lease reconciliation");
        assert_eq!(error.details["active_job_ids"], serde_json::json!([job.id]));
    }
}

#[test]
fn stop_reports_corrupt_lease_without_signalling_pid() {
    let _home = HomeGuard::new();
    let path = state_path().expect("state path");
    std::fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    std::fs::write(
        &path,
        format!(r#"{{"pid":{},"broken":"#, std::process::id()),
    )
    .expect("corrupt state");

    let err = stop().expect_err("corrupt lease is rejected");

    assert!(pid_is_running(std::process::id()));
    assert!(path.exists(), "corrupt lease should remain for remediation");
    assert!(err.message.contains("refusing to signal any pid"));
    assert!(err.message.contains(&path.display().to_string()));
    assert!(serialized_contains(&err.details, "remove"));
}

#[test]
fn test_serve_writes_state_and_routes_health_requests() {
    let _home = HomeGuard::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");

    std::thread::spawn(move || {
        let _ = serve_listener(listener);
    });

    let mut stream = None;
    for _ in 0..100 {
        match std::net::TcpStream::connect(addr) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let mut stream = stream.expect("daemon connection");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("write request");
    let mut response_bytes = Vec::new();
    stream
        .take(DAEMON_TEST_RESPONSE_LIMIT_BYTES + 1)
        .read_to_end(&mut response_bytes)
        .expect("read response");
    assert!(response_bytes.len() <= DAEMON_TEST_RESPONSE_LIMIT_BYTES as usize);
    let response = String::from_utf8_lossy(&response_bytes);

    let status = read_status().expect("status");

    assert!(response.contains("200 OK"));
    assert!(response.contains("\"status\": \"ok\""));
    assert!(status.running);
    assert_eq!(status.state.expect("state").address, addr.to_string());
}

#[test]
fn daemon_reads_full_content_length_body_before_json_parse() {
    let _home = HomeGuard::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");

    std::thread::spawn(move || {
        let _ = serve_listener(listener);
    });

    let mut stream = None;
    for _ in 0..100 {
        match std::net::TcpStream::connect(addr) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let mut stream = stream.expect("daemon connection");
    let body = serde_json::json!({
        "payload": "x".repeat(96 * 1024),
    })
    .to_string();
    let headers = format!(
        "POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let split = 16 * 1024;
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream
        .write_all(&body.as_bytes()[..split])
        .expect("write partial body");
    std::thread::sleep(std::time::Duration::from_millis(20));
    stream
        .write_all(&body.as_bytes()[split..])
        .expect("write remaining body");

    let mut response_bytes = Vec::new();
    stream
        .take(DAEMON_TEST_RESPONSE_LIMIT_BYTES + 1)
        .read_to_end(&mut response_bytes)
        .expect("read response");
    let response = String::from_utf8_lossy(&response_bytes);

    assert!(response.contains("405 Method Not Allowed"));
    assert!(
        !response.contains("invalid JSON request body"),
        "daemon parsed a partial JSON body: {response}"
    );
}

#[test]
fn routes_health_version_and_config_paths() {
    let _home = HomeGuard::new();

    let health = route("GET", "/health");
    assert_eq!(health.status_code, 200);
    assert_eq!(health.body["status"], "ok");

    let version = route("GET", "/version");
    assert_eq!(version.status_code, 200);
    assert_eq!(version.body["version"], env!("CARGO_PKG_VERSION"));

    let paths = route("GET", "/config/paths");
    assert_eq!(paths.status_code, 200);
    assert!(paths.body["homeboy"]
        .as_str()
        .unwrap()
        .ends_with(".config/homeboy"));
    assert!(paths.body["daemon_state"]
        .as_str()
        .unwrap()
        .ends_with("daemon/state.json"));
    assert!(paths.body["daemon_jobs"]
        .as_str()
        .unwrap()
        .ends_with("daemon/jobs.json"));
}

#[test]
fn routes_read_only_http_api_contract() {
    let _home = HomeGuard::new();

    let components = route("GET", "/components");
    assert_eq!(components.status_code, 200);
    assert_eq!(components.body["endpoint"], "components.list");
    assert!(components.body["body"]["components"].is_array());

    let job_ready = route("POST", "/audit");
    assert_eq!(job_ready.status_code, 200);
    assert_eq!(job_ready.body["endpoint"], "jobs.required");
    assert_eq!(job_ready.body["body"]["command"], "api.audit.enqueue");
    assert!(job_ready.body["body"]["poll"]["job"]
        .as_str()
        .unwrap()
        .starts_with("/jobs/"));

    let runs = route("GET", "/runs?kind=bench&limit=1");
    assert_eq!(runs.status_code, 200);
    assert_eq!(runs.body["endpoint"], "runs.list");
    assert!(runs.body["body"]["runs"].is_array());

    let bench_runs = route("GET", "/bench/runs?component=homeboy");
    assert_eq!(bench_runs.status_code, 200);
    assert_eq!(bench_runs.body["endpoint"], "bench.runs");
    assert!(bench_runs.body["body"]["runs"].is_array());

    let findings = route("GET", "/runs/run-missing/findings");
    assert_eq!(findings.status_code, 404);
    assert_eq!(findings.body["error"], "validation.invalid_argument");
}

#[test]
fn cancelling_daemon_exec_job_terminates_process_tree() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let cwd = std::env::temp_dir().join(format!("homeboy-daemon-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("test cwd");
    let marker = cwd.join("orphan-marker");

    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": cwd.display().to_string(),
            "command": [
                "sh",
                "-c",
                format!("sleep 1; touch {}", marker.display()),
            ],
        })),
        &store,
    );
    assert_eq!(response.status_code, 200);
    let job_id =
        uuid::Uuid::parse_str(response.body["body"]["job"]["id"].as_str().expect("job id"))
            .expect("parse job id");

    for _ in 0..50 {
        if store.get(job_id).expect("job").status == JobStatus::Running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    store
        .cancel(job_id, "test cancellation")
        .expect("cancel job");
    std::thread::sleep(std::time::Duration::from_millis(1600));

    assert_eq!(store.get(job_id).expect("job").status, JobStatus::Cancelled);
    assert!(
        !marker.exists(),
        "cancelled daemon runner exec left a child process running"
    );
}

#[test]
fn routes_registered_artifact_downloads_and_sync_manifest() {
    let _home = HomeGuard::new();
    let home_path = std::path::PathBuf::from(std::env::var("HOME").expect("home"));
    let store = ObservationStore::open_initialized().expect("store");
    let run = store
        .start_run(
            NewRunRecord::builder("bench")
                .component_id("homeboy")
                .command("homeboy bench")
                .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
                .homeboy_version("test-version")
                .git_sha(Some("abc123".to_string()))
                .rig_id("studio")
                .build(),
        )
        .expect("run");
    let artifact_path = home_path.join("bench-results.json");
    std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact");
    let artifact = store
        .record_artifact(&run.id, "bench_results", &artifact_path)
        .expect("record artifact");
    let metadata_only = ArtifactRecord {
        id: "exported-summary".to_string(),
        run_id: run.id.clone(),
        kind: "summary".to_string(),
        artifact_type: "metadata-only".to_string(),
        path: "metadata-only:exported-summary".to_string(),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: Some(11),
        mime: Some("application/json".to_string()),
        metadata_json: serde_json::json!({}),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    store
        .import_artifact(&metadata_only)
        .expect("metadata-only artifact");

    let download = route(
        "GET",
        &format!("/runs/{}/artifacts/{}", run.id, artifact.id),
    );
    assert_eq!(download.status_code, 200);
    assert!(download.artifact.is_some());
    assert_eq!(download.body["artifact"]["id"], artifact.id);
    assert_eq!(download.body["content_available"], true);
    assert_eq!(download.body["retrieval"]["mode"], "direct_download");
    assert_eq!(
        download.body["content_url"],
        format!("/runs/{}/artifacts/{}/content", run.id, artifact.id)
    );
    assert_eq!(download.body["size_bytes"], 11);

    let content_alias = route(
        "GET",
        &format!("/runs/{}/artifacts/{}/content", run.id, artifact.id),
    );
    assert_eq!(content_alias.status_code, 200);
    assert!(content_alias.artifact.is_some());

    let sync = route("GET", &format!("/runs/{}/artifacts/sync", run.id));
    assert_eq!(sync.status_code, 200);
    assert!(sync.artifact.is_none());
    assert_eq!(sync.body["command"], "api.runs.artifacts.sync");
    assert_eq!(sync.body["artifacts"][0]["id"], artifact.id);
    assert_eq!(sync.body["artifacts"][0]["content_available"], true);
    assert_eq!(
        sync.body["artifacts"][0]["retrieval"]["mode"],
        "direct_download"
    );
    assert_eq!(
        sync.body["artifacts"][0]["download_path"],
        format!("/runs/{}/artifacts/{}", run.id, artifact.id)
    );
    let metadata_artifact = sync.body["artifacts"]
        .as_array()
        .expect("artifact array")
        .iter()
        .find(|artifact| artifact["id"] == "exported-summary")
        .expect("metadata-only artifact");
    assert_eq!(metadata_artifact["content_available"], false);
    assert_eq!(metadata_artifact["content_url"], serde_json::Value::Null);
    assert_eq!(metadata_artifact["retrieval"]["mode"], "metadata_only");

    let raw_path = route(
        "GET",
        &format!("/runs/{}/artifacts/{}", run.id, artifact_path.display()),
    );
    assert_eq!(raw_path.status_code, 404);
}

#[test]
fn routes_job_inspection_against_daemon_job_store() {
    let store = JobStore::default();
    let job = store.create("lint");

    let list = route_with_job_store("GET", "/jobs", &store);
    assert_eq!(list.status_code, 200);
    assert_eq!(list.body["endpoint"], "jobs.list");
    assert_eq!(list.body["body"]["jobs"].as_array().unwrap().len(), 1);

    let show = route_with_job_store("GET", &format!("/jobs/{}", job.id), &store);
    assert_eq!(show.status_code, 200);
    assert_eq!(show.body["endpoint"], "jobs.show");
    assert_eq!(show.body["body"]["job"]["operation"], "lint");

    let events = route_with_job_store("GET", &format!("/jobs/{}/events", job.id), &store);
    assert_eq!(events.status_code, 200);
    assert_eq!(events.body["endpoint"], "jobs.events");
    assert_eq!(events.body["body"]["events"].as_array().unwrap().len(), 1);

    let cancel = route_with_job_store("POST", &format!("/jobs/{}/cancel", job.id), &store);
    assert_eq!(cancel.status_code, 200);
    assert_eq!(cancel.body["endpoint"], "jobs.cancel");
    assert_eq!(cancel.body["body"]["job"]["status"], "cancelled");
}

#[test]
fn routes_remote_runner_job_broker_lifecycle() {
    let store = JobStore::default();
    let submit = route_with_job_store_and_body(
        "POST",
        "/runner/jobs",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "project_id": "sample-project",
            "command": ["homeboy", "test", "sample-component"],
            "cwd": "/runner/workspaces/sample-component"
        })),
        &store,
    );

    assert_eq!(submit.status_code, 200);
    assert_eq!(submit.body["endpoint"], "runner.jobs.submit");
    assert_eq!(submit.body["body"]["command"], "api.runner.jobs.submit");
    let job_id = submit.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    let claim = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/claim",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "project_id": "sample-project",
            "lease_ms": 30000
        })),
        &store,
    );

    assert_eq!(claim.status_code, 200);
    assert_eq!(claim.body["endpoint"], "runner.jobs.claim");
    assert_eq!(claim.body["body"]["claim"]["job"]["id"], job_id);
    let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    let original_expiry = claim.body["body"]["claim"]["job"]["claim_expires_at_ms"]
        .as_u64()
        .expect("claim expiry");
    assert_eq!(
        claim.body["body"]["claim"]["request"]["command"],
        serde_json::json!(["homeboy", "test", "sample-component"])
    );

    let heartbeat = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/heartbeat"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": claim_id.clone(),
            "lease_ms": 60000
        })),
        &store,
    );

    assert_eq!(heartbeat.status_code, 200);
    assert_eq!(heartbeat.body["endpoint"], "runner.jobs.heartbeat");
    assert_eq!(heartbeat.body["body"]["job"]["id"], job_id);
    assert!(
        heartbeat.body["body"]["job"]["claim_expires_at_ms"]
            .as_u64()
            .expect("renewed expiry")
            > original_expiry
    );

    let event = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/events"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": claim_id.clone(),
            "kind": "progress",
            "data": { "phase": "running" }
        })),
        &store,
    );

    assert_eq!(event.status_code, 200);
    assert_eq!(event.body["endpoint"], "runner.jobs.events.append");
    assert_eq!(event.body["body"]["event"]["kind"], "progress");

    let finish = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/finish"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": claim_id,
            "result": {
                "exit_code": 0,
                "stdout": "ok",
                "stderr": ""
            }
        })),
        &store,
    );

    assert_eq!(finish.status_code, 200);
    assert_eq!(finish.body["endpoint"], "runner.jobs.finish");
    assert_eq!(finish.body["body"]["job"]["status"], "succeeded");
}

#[test]
fn remote_runner_broker_redacts_secret_env_from_public_surfaces() {
    let store = JobStore::default();
    let sentinel = "homeboy-secret-sentinel-do-not-echo";
    let submit = route_with_job_store_and_body(
        "POST",
        "/runner/jobs",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "project_id": "sample-project",
            "command": ["homeboy", "test", "sample-component"],
            "cwd": "/runner/workspaces/sample-component",
            "env": {
                "PUBLIC_FLAG": "1",
                "RUNNER_SECRET_TOKEN": sentinel
            },
            "secret_env_names": ["RUNNER_SECRET_TOKEN"]
        })),
        &store,
    );

    assert_eq!(submit.status_code, 200, "submit body: {}", submit.body);
    assert!(!serialized_contains(&submit.body, sentinel));
    assert_eq!(
        submit.body["body"]["request"]["env"]["RUNNER_SECRET_TOKEN"],
        "<redacted>"
    );
    let job_id = submit.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    let list = route_with_job_store("GET", "/jobs", &store);
    assert_eq!(list.status_code, 200, "list body: {}", list.body);
    assert!(!serialized_contains(&list.body, sentinel));

    let show = route_with_job_store("GET", &format!("/jobs/{job_id}"), &store);
    assert_eq!(show.status_code, 200, "show body: {}", show.body);
    assert!(!serialized_contains(&show.body, sentinel));

    let events = route_with_job_store("GET", &format!("/jobs/{job_id}/events"), &store);
    assert_eq!(events.status_code, 200, "events body: {}", events.body);
    assert!(!serialized_contains(&events.body, sentinel));

    let claim = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/claim",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "project_id": "sample-project",
            "lease_ms": 30000
        })),
        &store,
    );
    assert_eq!(claim.status_code, 200, "claim body: {}", claim.body);
    assert_eq!(
        claim.body["body"]["claim"]["request"]["env"]["RUNNER_SECRET_TOKEN"],
        sentinel
    );
}

#[test]
fn routes_remote_runner_job_updates_require_live_matching_claim_id() {
    let store = JobStore::default();
    let submit = route_with_job_store_and_body(
        "POST",
        "/runner/jobs",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "command": ["homeboy", "test", "sample-component"]
        })),
        &store,
    );
    let job_id = submit.body["body"]["job"]["id"].as_str().expect("job id");
    let claim = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/claim",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "lease_ms": 30000
        })),
        &store,
    );
    let claim_id = claim.body["body"]["claim"]["job"]["claim_id"]
        .as_str()
        .expect("claim id");

    let missing_claim = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/events"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "kind": "progress"
        })),
        &store,
    );
    assert_eq!(missing_claim.status_code, 400);

    let wrong_claim = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/finish"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": "wrong-claim",
            "result": { "exit_code": 0 }
        })),
        &store,
    );
    assert_eq!(wrong_claim.status_code, 400);

    let expired_job = route_with_job_store_and_body(
        "POST",
        "/runner/jobs",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "command": ["homeboy", "test", "sample-component"]
        })),
        &store,
    );
    let expired_job_id = expired_job.body["body"]["job"]["id"]
        .as_str()
        .expect("job id");
    let expired_claim = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/claim",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "lease_ms": 1
        })),
        &store,
    );
    let expired_claim_id = expired_claim.body["body"]["claim"]["job"]["claim_id"]
        .as_str()
        .expect("claim id");
    std::thread::sleep(std::time::Duration::from_millis(5));

    let expired_event = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{expired_job_id}/events"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": expired_claim_id,
            "kind": "progress"
        })),
        &store,
    );
    assert_eq!(expired_event.status_code, 400);

    let valid_event = route_with_job_store_and_body(
        "POST",
        &format!("/runner/jobs/{job_id}/events"),
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "claim_id": claim_id,
            "kind": "progress"
        })),
        &store,
    );
    assert_eq!(valid_event.status_code, 200);
}

#[test]
fn broker_reconcile_route_owns_expired_reverse_runner_claims() {
    let store = JobStore::default();
    let submit = route_with_job_store_and_body(
        "POST",
        "/runner/jobs",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "command": ["homeboy", "test", "sample-component"]
        })),
        &store,
    );
    let job_id = submit.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    let claim = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/claim",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "lease_ms": 1
        })),
        &store,
    );
    assert_eq!(claim.status_code, 200);
    std::thread::sleep(std::time::Duration::from_millis(5));

    let reconcile = route_with_job_store_and_body(
        "POST",
        "/runner/jobs/reconcile",
        Some(serde_json::json!({})),
        &store,
    );

    assert_eq!(reconcile.status_code, 200);
    assert_eq!(reconcile.body["endpoint"], "runner.jobs.reconcile");
    assert_eq!(
        reconcile.body["body"]["command"],
        "api.runner.jobs.reconcile"
    );
    assert_eq!(reconcile.body["body"]["reconciled_count"], 1);
    assert_eq!(reconcile.body["body"]["reconciled"][0]["id"], job_id);
    assert_eq!(reconcile.body["body"]["reconciled"][0]["status"], "failed");
    assert_eq!(reconcile.body["body"]["policy"]["owner"], "broker");

    let events = store
        .events(uuid::Uuid::parse_str(&job_id).expect("job uuid"))
        .expect("events");
    assert!(events
        .iter()
        .any(|event| event.message.as_deref() == Some("remote runner claim expired")));
}

#[test]
fn daemon_http_error_envelope_includes_error_payload() {
    let _home = HomeGuard::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let _ = serve_listener(listener);
    });

    let response: serde_json::Value = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("client")
        .post(format!("http://{addr}/exec"))
        .json(&serde_json::json!({}))
        .send()
        .expect("response")
        .json()
        .expect("json");

    assert_eq!(response["success"], false);
    assert_eq!(response["data"]["error"], "validation.invalid_argument");
    assert_eq!(response["error"]["error"], "validation.invalid_argument");
    assert!(response["error"]["message"]
        .as_str()
        .expect("message")
        .contains("invalid exec request body"));
}

#[test]
fn routes_json_body_to_analysis_enqueue() {
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/lint",
        Some(serde_json::json!({
            "component": "missing-component",
            "path": "/tmp/homeboy-missing-component",
            "changed_since": "origin/main",
            "json_summary": true
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body["endpoint"], "jobs.required");
    assert_eq!(response.body["body"]["command"], "api.lint.enqueue");
    assert_eq!(store.list().len(), 1);
}

#[test]
fn route_with_body_validates_exec_requests() {
    let _home = HomeGuard::new();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "cwd": "relative",
            "command": []
        })),
        daemon_job_store(),
    );

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["error"], "validation.invalid_argument");
}

fn create_lab_local_runner() -> HomeGuard {
    let home = HomeGuard::new();
    write_runner_config(
        "lab-local",
        &serde_json::json!({"id": "lab-local", "kind": "local"}),
    );
    home
}

#[test]
fn routes_exec_body_to_daemon_job() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf ok"]
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body["endpoint"], "jobs.exec");
    assert_eq!(response.body["body"]["command"], "api.runner.exec.enqueue");
    assert_eq!(
        response.body["body"]["job"]["source_snapshot"]["runner_id"],
        "lab-local"
    );
    assert_eq!(
        response.body["body"]["job"]["source_snapshot"]["remote_path"],
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(
        response.body["body"]["job"]["source_snapshot"]["sync_mode"],
        "existing_remote"
    );
    assert_eq!(store.list().len(), 1);
    assert_eq!(
        store.list()[0]
            .source_snapshot
            .as_ref()
            .expect("stored source snapshot")
            .runner_id,
        "lab-local"
    );
}

#[test]
fn routes_exec_preserves_path_materialization_plan_on_job_metadata() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let cwd = std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .to_string();
    let path_materialization_plan = serde_json::json!({
        "schema": "homeboy/path-materialization-plan/v1",
        "entries": [
            {
                "role": "workspace",
                "owner": "lab-local",
                "local_path": cwd,
                "remote_path": "/tmp/homeboy-lab/workspace",
                "materialization_mode": "mirror",
                "validation_status": "validated"
            }
        ]
    });
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf ok"],
            "path_materialization_plan": path_materialization_plan
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    assert_eq!(
        response.body["body"]["job"]["path_materialization_plan"],
        path_materialization_plan
    );
    assert_eq!(
        response.body["body"]["request"]["path_materialization_plan"],
        path_materialization_plan
    );
    assert_eq!(
        store.list()[0].path_materialization_plan,
        Some(serde_json::from_value(path_materialization_plan.clone()).expect("plan"))
    );

    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);
    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(
        result["path_materialization_plan"],
        path_materialization_plan
    );
}

#[test]
fn daemon_exec_does_not_require_runner_config_on_daemon_host() {
    let _home = HomeGuard::new();
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf lab"]
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["runner_id"], "homeboy-lab");
    assert_eq!(result["stdout"], "lab");
    assert_eq!(result["source_snapshot"]["runner_id"], "homeboy-lab");
    assert_eq!(result["source_snapshot"]["sync_mode"], "existing_remote");
}

#[test]
fn exec_applies_request_env_to_daemon_command() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf '%s' \"$HOMEBOY_TEST_DAEMON_ENV\""],
            "env": {
                "HOMEBOY_TEST_DAEMON_ENV": "ok"
            }
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["runner_id"], "lab-local");
    assert_eq!(
        result["cwd"],
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(
        result["command"],
        serde_json::json!(["sh", "-c", "printf '%s' \"$HOMEBOY_TEST_DAEMON_ENV\""])
    );
    assert_eq!(result["stdout"], "ok");
    assert_eq!(result["source_snapshot"]["runner_id"], "lab-local");
    assert_eq!(result["source_snapshot"]["sync_mode"], "existing_remote");
    assert!(result["metrics"]["duration_ms"].as_u64().is_some());
    if cfg!(target_os = "linux") {
        assert_eq!(result["metrics"]["source"], "linux_procfs_process_tree");
        if result["metrics"]["sample_count"].as_u64().unwrap_or(0) > 0 {
            assert!(result["metrics"].get("peak_rss_bytes").is_some());
        }
    }
}

#[test]
fn exec_failed_command_marks_job_failed_after_result_event() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf out; printf err >&2; exit 7"]
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Failed);

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["stdout"], "out");
    assert_eq!(result["stderr"], "err");

    let status_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == JobEventKind::Status)
        .collect();
    assert!(status_events.iter().all(|event| {
        event.data.as_ref().and_then(|data| data["status"].as_str()) != Some("succeeded")
    }));
    let final_status = status_events.last().expect("final status event");
    assert_eq!(final_status.data.as_ref().unwrap()["status"], "failed");
    assert_ne!(final_status.message.as_deref(), Some("job succeeded"));
}

#[test]
fn exec_capture_patch_records_remote_delta_artifact() {
    let _home = create_lab_local_runner();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("file.txt"), "before\n").expect("seed file");
    let source_snapshot = crate::source_snapshot::existing_remote(
        "lab-local",
        &workspace.path().display().to_string(),
        Some(workspace.path().display().to_string().as_str()),
    );
    let store = JobStore::default();
    let response = route_with_job_store_and_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": workspace.path(),
            "command": ["sh", "-c", "printf 'after\n' > file.txt"],
            "capture_patch": true,
            "source_snapshot": source_snapshot,
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(format!("{:?}", job.status), "Succeeded");

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .rev()
        .filter_map(|event| event.data.as_ref())
        .find(|data| data.get("patch").is_some())
        .expect("patch result");
    let patch = &result["patch"];
    assert_eq!(patch["runner_id"], "lab-local");
    assert_eq!(patch["remote_path"], workspace.path().display().to_string());
    assert_eq!(patch["modified_files"], serde_json::json!(["file.txt"]));
    assert_eq!(patch["dirty_snapshot"], false);
    assert_eq!(patch["baseline_missing"], false);
    assert!(patch["patch_artifact_id"].as_str().is_some());

    let observation_store = ObservationStore::open_initialized().expect("observation store");
    let run_id = format!("runner-exec-{job_id}");
    let artifacts = observation_store
        .list_artifacts(&run_id)
        .expect("patch artifacts");
    assert_eq!(artifacts.len(), 1);
    let patch_body = std::fs::read_to_string(&artifacts[0].path).expect("patch file");
    assert!(patch_body.contains("-before"));
    assert!(patch_body.contains("+after"));
}

fn wait_for_job(store: &JobStore, job_id: &str) -> crate::api_jobs::Job {
    let id = uuid::Uuid::parse_str(job_id).expect("uuid");
    for _ in 0..100 {
        let job = store.get(id).expect("job");
        if matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ) {
            return job;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    store.get(id).expect("job")
}

#[test]
fn route_rejects_unknown_paths_and_methods() {
    assert_eq!(route("GET", "/missing").status_code, 404);
    assert_eq!(route("POST", "/health").status_code, 405);
    assert_eq!(route("POST", "/release").status_code, 404);
}

#[test]
fn status_is_not_running_without_state_file() {
    let _home = HomeGuard::new();

    let status = read_status().expect("status");
    assert!(!status.running);
    assert!(status.state.is_none());
    assert!(status.state_path.ends_with("daemon/state.json"));
}
