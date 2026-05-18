use super::*;
use crate::api_jobs::JobStore;
use crate::observation::{NewRunRecord, ObservationStore};
use crate::test_support::HomeGuard;

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
    assert!(!read_status().expect("status").running);
}

#[test]
fn read_status_reports_stale_state_as_not_running() {
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

    let status = read_status().expect("status");

    assert!(!status.running);
    assert_eq!(status.state.expect("state").pid, u32::MAX);
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
fn test_serve_writes_state_and_routes_health_requests() {
    let _home = HomeGuard::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    std::thread::spawn(move || {
        let _ = serve(addr);
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
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    let status = read_status().expect("status");

    assert!(response.contains("200 OK"));
    assert!(response.contains("\"status\": \"ok\""));
    assert!(status.running);
    assert_eq!(status.state.expect("state").address, addr.to_string());
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

    let download = route(
        "GET",
        &format!("/runs/{}/artifacts/{}", run.id, artifact.id),
    );
    assert_eq!(download.status_code, 200);
    assert!(download.artifact.is_some());
    assert_eq!(download.body["artifact"]["id"], artifact.id);
    assert_eq!(download.body["size_bytes"], 11);

    let sync = route("GET", &format!("/runs/{}/artifacts/sync", run.id));
    assert_eq!(sync.status_code, 200);
    assert!(sync.artifact.is_none());
    assert_eq!(sync.body["command"], "api.runs.artifacts.sync");
    assert_eq!(sync.body["artifacts"][0]["id"], artifact.id);
    assert_eq!(
        sync.body["artifacts"][0]["download_path"],
        format!("/runs/{}/artifacts/{}", run.id, artifact.id)
    );

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

#[test]
fn routes_exec_body_to_daemon_job() {
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
fn exec_capture_patch_records_remote_delta_artifact() {
    let _home = HomeGuard::new();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("file.txt"), "before\n").expect("seed file");
    let source_snapshot = SourceSnapshot::existing_remote(
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
            crate::api_jobs::JobStatus::Succeeded
                | crate::api_jobs::JobStatus::Failed
                | crate::api_jobs::JobStatus::Cancelled
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
