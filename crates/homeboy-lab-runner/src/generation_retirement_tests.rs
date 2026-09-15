//! Real loopback daemons, separate durable stores, and separate controller
//! processes. Every child has a watchdog and a parent-owned kill/wait guard.
use super::*;
use base64::Engine;
use homeboy_core::observation::{ObservationStore, RunRecord};
use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use serde_json::{json, Value};

const SYNC: &str = "HOMEBOY_RETIREMENT_TEST_SYNC";
const BYTES: &[u8] = b"immutable A evidence\0\xff\n";

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Child {
    fn finish(&mut self) {
        self.wait_for_exit(false);
    }
    fn stopped(&mut self) {
        self.wait_for_exit(true);
    }
    fn wait_for_exit(&mut self, stopped: bool) {
        let deadline = Instant::now() + Duration::from_secs(if stopped { 10 } else { 45 });
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                use std::os::unix::process::ExitStatusExt;
                assert!(
                    status.success() || (stopped && status.signal() == Some(15)),
                    "child failed: {status}"
                );
                return;
            }
            assert!(Instant::now() < deadline, "child exceeded deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn spawn(context: &HermeticTestContext, test: &str, sync: &Path) -> Child {
    let token = uuid::Uuid::new_v4().to_string();
    Child(
        context
            .command(TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                &format!("generation_store::retirement_tests::{test}"),
                "--nocapture",
            ])
            // Non-Linux process identity uses the startup token in argv. After
            // `--`, libtest treats these as additional (nonmatching) exact filters.
            .args(["--", "--startup-token", &token])
            .env(SYNC, sync)
            .env(
                paths::DAEMON_STATE_DIR_ENV,
                context.config_dir().join("daemon"),
            )
            .env("HOMEBOY_DAEMON_STARTUP_TOKEN", token)
            .spawn()
            .unwrap(),
    )
}

fn sync() -> PathBuf {
    PathBuf::from(std::env::var_os(SYNC).expect("test-owned sync directory"))
}

fn watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(124);
    });
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn get(session: &RunnerSession, path: &str) -> Value {
    client()
        .get(format!("{}{path}", session.local_url.as_ref().unwrap()))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap()
}

fn post(session: &RunnerSession, path: &str, body: Value) -> Value {
    client()
        .post(format!("{}{path}", session.local_url.as_ref().unwrap()))
        .json(&body)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap()
}

#[derive(Clone)]
struct GatedAnalysis;
impl homeboy_core::http_api::AnalysisJobRunner for GatedAnalysis {
    fn run_analysis_job(
        &self,
        argv: Vec<String>,
    ) -> Result<homeboy_core::http_api::AnalysisJobRunOutput> {
        if argv.iter().any(|arg| arg == "new") {
            return Ok(homeboy_core::http_api::AnalysisJobRunOutput {
                exit_code: 0,
                output: json!({"generation": "B"}),
            });
        }
        wait(|| sync().join("complete").exists());
        if argv.iter().any(|arg| arg == "cancel") {
            return Ok(homeboy_core::http_api::AnalysisJobRunOutput {
                exit_code: 0,
                output: json!({}),
            });
        }
        let store = ObservationStore::open_initialized()?;
        let run = RunRecord {
            id: "retained-a".to_string(),
            kind: "audit".to_string(),
            started_at: "2026-09-15T00:00:00Z".to_string(),
            finished_at: Some("2026-09-15T00:00:01Z".to_string()),
            status: "pass".to_string(),
            metadata_json: json!({"lab": {"runner": {"id": "runner-a"}}}),
            ..Default::default()
        };
        store.import_run(&run)?;
        let source = sync().join("source.bin");
        std::fs::write(&source, BYTES).unwrap();
        let artifact =
            store.record_artifact_with_id(&run.id, "evidence", &source, "artifact-a", json!({}))?;
        std::fs::write(
            sync().join("evidence.json"),
            serde_json::to_vec(&(run, artifact)).unwrap(),
        )
        .unwrap();
        Ok(homeboy_core::http_api::AnalysisJobRunOutput {
            exit_code: 0,
            output: json!({"run_id": "retained-a"}),
        })
    }
}

#[test]
#[ignore = "bounded daemon subprocess for retained_evidence_survives_process_retirement_and_controller_restart"]
fn daemon_process() {
    watchdog();
    homeboy_core::daemon::serve_with_analysis_runner("127.0.0.1:0".parse().unwrap(), GatedAnalysis)
        .unwrap();
}

fn daemon_session(context: &HermeticTestContext, expected_pid: u32) -> RunnerSession {
    let state_path = context.config_dir().join("daemon/state.json");
    wait(|| state_path.exists());
    let state: homeboy_core::daemon::DaemonState =
        serde_json::from_slice(&std::fs::read(state_path).unwrap()).unwrap();
    assert_eq!(
        state.pid, expected_pid,
        "fixture lease must belong to the child we spawned"
    );
    let mut session = super::tests::session(&state.lease_id, "unused", None);
    session.remote_daemon_pid = Some(state.pid);
    session.remote_daemon_address = Some(state.address.clone());
    session.local_url = Some(format!("http://{}", state.address));
    wait(|| {
        client()
            .get(format!("{}/health", session.local_url.as_ref().unwrap()))
            .send()
            .is_ok()
    });
    session
}

#[test]
fn retained_evidence_survives_process_retirement_and_controller_restart() {
    let a = HermeticTestContext::new();
    let b = HermeticTestContext::new();
    let c = HermeticTestContext::new();
    let controller = HermeticTestContext::new();
    let mut daemon_a = spawn(&a, "daemon_process", a.root());
    let mut daemon_b = spawn(&b, "daemon_process", b.root());
    let mut daemon_c = spawn(&c, "daemon_process", c.root());
    let sessions = [
        daemon_session(&a, daemon_a.0.id()),
        daemon_session(&b, daemon_b.0.id()),
        daemon_session(&c, daemon_c.0.id()),
    ];
    std::fs::write(
        controller.root().join("sessions.json"),
        serde_json::to_vec(&sessions).unwrap(),
    )
    .unwrap();
    std::fs::write(controller.root().join("a-root"), a.root().to_str().unwrap()).unwrap();
    spawn(
        &controller,
        "controller_lifecycle_process",
        controller.root(),
    )
    .finish();
    // The actual A process must have exited; a removed registry entry is not proof.
    daemon_a.stopped();
    daemon_b.stopped();
    spawn(&controller, "controller_restart_process", controller.root()).finish();
    post(
        &sessions[2],
        "/lifecycle/stop",
        json!({"lease_id": sessions[2].remote_daemon_lease_id, "force": false}),
    );
    daemon_c.stopped();
}

fn sessions() -> [RunnerSession; 3] {
    serde_json::from_slice(&std::fs::read(sync().join("sessions.json")).unwrap()).unwrap()
}

#[test]
#[ignore = "controller lifecycle subprocess"]
fn controller_lifecycle_process() {
    watchdog();
    let [a, b, c] = sessions();
    let job = post(&a, "/audit", json!({}));
    let job_id = job
        .pointer("/data/body/job/id")
        .and_then(Value::as_str)
        .expect("A job ID");
    record_job("runner-a", &a, job_id).unwrap();
    record_job_run("runner-a", &a, job_id, "retained-a").unwrap();
    record_job_artifacts("runner-a", &a, job_id, ["artifact-a".to_string()]).unwrap();
    activate(
        "runner-a",
        &a,
        "B".to_string(),
        b.clone(),
        &[job_id.to_string()],
    )
    .unwrap();
    assert_eq!(
        admission_session("runner-a", None).unwrap(),
        Some(b.clone())
    );
    let new_job = post(&b, "/audit", json!({"component": "new"}));
    let new_id = new_job
        .pointer("/data/body/job/id")
        .and_then(Value::as_str)
        .unwrap();
    record_job("runner-a", &b, new_id).unwrap();
    assert_eq!(
        job_session("runner-a", new_id, None).unwrap(),
        Some(b.clone())
    );
    assert_eq!(
        job_session("runner-a", job_id, None).unwrap(),
        Some(a.clone())
    );
    assert!(get(&a, &format!("/jobs/{job_id}"))
        .pointer("/data/body/job")
        .is_some());
    let busy = reconcile("runner-a", None).unwrap();
    assert!(busy.retired_generation_ids.is_empty());
    assert!(!busy.retirement_blockers.is_empty());
    assert!(get(&a, "/health").is_object());
    let valid_registry = read("runner-a", None).unwrap().unwrap();
    let mut unknown = valid_registry.clone();
    unknown
        .run_owners
        .insert("unknown-run".to_string(), "missing-generation".to_string());
    write("runner-a", &unknown).unwrap();
    let unknown = reconcile("runner-a", None).unwrap();
    assert!(unknown.retired_generation_ids.is_empty());
    assert!(unknown.retirement_blockers["missing-generation"].contains("resolves to 0"));
    write("runner-a", &valid_registry).unwrap();

    // A second active job remains cancellable on A while B admits new work.
    let cancel = post(&a, "/audit", json!({"component": "cancel"}));
    let cancel_id = cancel
        .pointer("/data/body/job/id")
        .and_then(Value::as_str)
        .unwrap();
    record_job("runner-a", &a, cancel_id).unwrap();
    let owner = job_session("runner-a", cancel_id, None).unwrap().unwrap();
    post(&owner, &format!("/jobs/{cancel_id}/cancel"), json!({}));

    let a_root = PathBuf::from(std::fs::read_to_string(sync().join("a-root")).unwrap());
    std::fs::write(a_root.join("complete"), b"complete").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let health = get(&a, "/health");
        if health
            .pointer("/data/freshness/active_jobs")
            .and_then(Value::as_u64)
            == Some(0)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "A did not drain: health={health}; job={}",
            get(&a, &format!("/jobs/{job_id}"))
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let operations = HttpGenerationEndpointOperations { client: client() };
    let mut stale = a.clone();
    stale.remote_daemon_lease_id = Some("stale-lease".to_string());
    assert_eq!(operations.active_jobs(&stale), None);
    assert!(!operations.reconcile_terminal_jobs(&stale));
    assert!(!operations.stop(&stale));
    assert_eq!(operations.active_jobs(&a), Some(0));
    let blocked = reconcile("runner-a", None).unwrap();
    assert!(blocked.retired_generation_ids.is_empty());
    assert!(blocked
        .retirement_blockers
        .values()
        .any(|reason| reason.contains("controller run is missing")));

    // Use the same verified-copy primitive as terminal mirroring. The source
    // bytes come from A's real HTTP artifact route, not a shared artifact root.
    let (run, artifact): (RunRecord, homeboy_core::observation::ArtifactRecord) =
        serde_json::from_slice(&std::fs::read(a_root.join("evidence.json")).unwrap()).unwrap();
    let bytes = client()
        .get(format!(
            "{}/runs/retained-a/artifacts/artifact-a/content",
            a.local_url.as_ref().unwrap()
        ))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .unwrap();
    assert_eq!(bytes.as_ref(), BYTES);
    let store = ObservationStore::open_initialized().unwrap();
    store.import_run(&run).unwrap();
    std::fs::write(
        sync().join("expected-run.json"),
        serde_json::to_vec(&run).unwrap(),
    )
    .unwrap();
    let downloaded = sync().join("download.bin");
    std::fs::write(&downloaded, &bytes).unwrap();
    let retained = store
        .record_verified_artifact_with_id(
            &run.id,
            &artifact.kind,
            &downloaded,
            &artifact.id,
            artifact.size_bytes,
            artifact.sha256.as_deref(),
            artifact.metadata_json,
        )
        .unwrap();
    // Equal length is insufficient: a corrupted copy must not release A.
    std::fs::write(&retained.path, vec![b'x'; BYTES.len()]).unwrap();
    let corrupt = reconcile("runner-a", None).unwrap();
    assert!(corrupt.retired_generation_ids.is_empty());
    assert!(corrupt
        .retirement_blockers
        .values()
        .any(|reason| reason.contains("checksum")));
    std::fs::write(&retained.path, BYTES).unwrap();
    let retired = reconcile("runner-a", None).unwrap();
    assert_eq!(
        retired.retired_generation_ids,
        [a.remote_daemon_lease_id.clone().unwrap()],
        "{retired:?}"
    );
    assert!(retired.retirement_blockers.is_empty());
    assert_eq!(live_sessions("runner-a", None).unwrap(), [b.clone()]);
    assert!(reconcile("runner-a", None)
        .unwrap()
        .retired_generation_ids
        .is_empty());
    activate("runner-a", &b, "C".to_string(), c.clone(), &[]).unwrap();
    let second = reconcile("runner-a", None).unwrap();
    assert_eq!(second.retired_generation_ids, ["B"]);
    assert_eq!(live_sessions("runner-a", None).unwrap(), [c]);
    check_reads();
}

fn check_reads() {
    let registry = read("runner-a", None).unwrap().unwrap();
    assert_eq!(registry.retired_evidence.len(), 1);
    assert_eq!(
        registry.run_owners["retained-a"],
        registry.retired_evidence.keys().next().unwrap().as_str()
    );
    let run = crate::execution::daemon_api_get("runner-a", "/runs/retained-a").unwrap();
    assert_eq!(run.pointer("/body/run/status"), Some(&json!("pass")));
    let artifact =
        crate::connection::runner_artifact_content("runner-a", "historical-job", "artifact-a")
            .unwrap();
    let encoded = artifact
        .get("content_base64")
        .and_then(Value::as_str)
        .expect("artifact bytes");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        BYTES
    );
    let store = ObservationStore::open_initialized().unwrap();
    let expected: RunRecord =
        serde_json::from_slice(&std::fs::read(sync().join("expected-run.json")).unwrap()).unwrap();
    assert_eq!(store.get_run("retained-a").unwrap(), Some(expected));
    assert_eq!(
        std::fs::read(store.get_artifact("artifact-a").unwrap().unwrap().path).unwrap(),
        BYTES
    );
    let token = homeboy_core::execution_contract::EXECUTION_CONTRACT
        .artifacts
        .runner_artifact_ref("runner-a", "retained-a", "artifact-a");
    let download =
        crate::evidence::download_remote_artifact(&token, Some(sync().join("review.bin"))).unwrap();
    assert_eq!(std::fs::read(download.output_path).unwrap(), BYTES);
}

#[test]
#[ignore = "fresh controller subprocess with no live A endpoint"]
fn controller_restart_process() {
    watchdog();
    check_reads();
}
