use super::*;
use crate::core::api_jobs::{JobEventKind, JobStatus, JobStore};
use crate::core::observation::{ArtifactRecord, NewRunRecord, ObservationStore};
use crate::test_support::HomeGuard;

const DAEMON_TEST_RESPONSE_LIMIT_BYTES: u64 = 64 * 1024;

fn serialized_contains(value: &serde_json::Value, needle: &str) -> bool {
    serde_json::to_string(value)
        .expect("serialize test value")
        .contains(needle)
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
            "project_id": "extrachill",
            "command": ["homeboy", "test", "sample-plugin"],
            "cwd": "/home/user/Developer/sample-plugin"
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
            "project_id": "extrachill",
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
        serde_json::json!(["homeboy", "test", "sample-plugin"])
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
            "project_id": "extrachill",
            "command": ["homeboy", "test", "sample-plugin"],
            "cwd": "/home/user/Developer/sample-plugin",
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
            "project_id": "extrachill",
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
            "command": ["homeboy", "test", "sample-plugin"]
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
            "command": ["homeboy", "test", "sample-plugin"]
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
            "command": ["homeboy", "test", "sample-plugin"]
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
        .timeout(std::time::Duration::from_secs(10))
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
fn routes_remote_runner_session_registration() {
    let _home = HomeGuard::new();
    crate::core::runner::create(
        r#"{"id":"homeboy-lab","kind":"local","workspace_root":"/home/user/Developer"}"#,
        false,
    )
    .expect("create runner");
    let store = JobStore::default();

    let response = route_with_job_store_and_body(
        "POST",
        "/runner/sessions",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "controller_id": "extra-chill",
            "broker_url": "http://127.0.0.1:49152",
            "homeboy_version": "test-version"
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body["endpoint"], "runner.sessions.register");
    assert_eq!(
        response.body["body"]["session"]["role"],
        serde_json::json!("controller")
    );

    let status = crate::core::runner::status("homeboy-lab").expect("runner status");
    assert!(status.connected);
    assert_eq!(
        status.state,
        crate::core::runner::RunnerSessionState::Connected
    );
    let session = status.session.expect("session");
    assert_eq!(session.controller_id.as_deref(), Some("extra-chill"));
    assert_eq!(
        session.broker_url.as_deref(),
        Some("http://127.0.0.1:49152")
    );
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
    crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
        .expect("create lab local runner");
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

#[test]
fn runner_exec_rejects_requests_that_violate_runner_policy_before_daemon_dispatch() {
    let _home = HomeGuard::new();
    crate::core::server::create(
        r#"{"id":"lab-server","host":"192.0.2.10","user":"user"}"#,
        false,
    )
    .expect("create server");
    crate::core::runner::create(
        r#"{"id":"lab-server","kind":"ssh","server_id":"lab-server","workspace_root":"/srv/homeboy"}"#,
        false,
    )
    .expect("create ssh runner");

    let err = crate::core::runner::exec(
        "lab-server",
        crate::core::runner::RunnerExecOptions {
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf denied".to_string(),
            ],
            env: Default::default(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            detach_after_handoff: false,
        },
    )
    .expect_err("policy denied");

    assert_eq!(err.code.as_str(), "runner.policy_denied");
    assert_eq!(err.details["runner_id"], "lab-server");
    assert_eq!(err.details["field"], "raw_exec");
}

fn wait_for_job(store: &JobStore, job_id: &str) -> crate::core::api_jobs::Job {
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
