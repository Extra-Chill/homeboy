//! Daemon `/exec` end-to-end tests that require the real runner exec driver.
//!
//! The daemon `/exec` endpoint routes command execution through the
//! `RunnerExecDriver` hook (Extra-Chill/homeboy#8632), whose real implementation
//! lives in this runner crate (extracted from core in #8698). These tests were
//! moved out of `homeboy-core`'s daemon tests because they assert on the result
//! of actually running the requested command — which a core-only stub driver
//! cannot produce. Here the real driver is registered, so the command runs for
//! real and the result events, exit codes, captured stdout/stderr, and patch
//! artifacts are exercised end to end.

use homeboy_core::api_jobs::{Job, JobEventKind, JobStatus, JobStore};
use homeboy_core::daemon::route_with_body;
use homeboy_core::observation::ObservationStore;
use homeboy_core::test_support::HomeGuard;

/// Register the runner-side exec driver the daemon `/exec` route drives.
/// Production wires this at CLI startup; the registration is an idempotent
/// process-global slot, so registering per test is safe.
fn register_driver() {
    crate::register_runner_daemon_exec_driver();
}

fn write_runner_config(id: &str, value: &serde_json::Value) {
    let dir = homeboy_core::paths::homeboy()
        .expect("homeboy dir")
        .join("runners");
    std::fs::create_dir_all(&dir).expect("create runners dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(value).expect("serialize runner"),
    )
    .expect("write runner config");
}

fn create_lab_local_runner() -> HomeGuard {
    let home = HomeGuard::new();
    write_runner_config(
        "lab-local",
        &serde_json::json!({"id": "lab-local", "kind": "local"}),
    );
    home
}

fn wait_for_job(store: &JobStore, job_id: &str) -> Job {
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
fn daemon_exec_does_not_require_runner_config_on_daemon_host() {
    register_driver();
    let _home = HomeGuard::new();
    let store = JobStore::default();
    let response = route_with_body(
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
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_body(
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
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_body(
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
    register_driver();
    let _home = create_lab_local_runner();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("file.txt"), "before\n").expect("seed file");
    let source_snapshot = homeboy_core::source_snapshot::existing_remote(
        "lab-local",
        &workspace.path().display().to_string(),
        Some(workspace.path().display().to_string().as_str()),
    );
    let store = JobStore::default();
    let response = route_with_body(
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
