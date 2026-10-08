use super::*;
use homeboy::core::daemon;
use homeboy_control_plane_contract::{
    ControlPlaneReference, ControlPlaneResult, ControlPlaneRun, CONTROL_PLANE_REFERENCE_SCHEMA,
    CONTROL_PLANE_RESULT_SCHEMA,
};
use serde_json::json;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// The existing daemon owns routing, envelopes, exact byte authorization, and
/// streaming. Teardown wakes its listener and joins its owned helpers, also on
/// assertion failure; this fixture does not implement an artifact reader.
struct OwnedDaemon {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<homeboy::core::Result<daemon::DaemonState>>>,
}

impl OwnedDaemon {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("owned loopback listener");
        let addr = listener.local_addr().expect("loopback address");
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown);
        let thread = std::thread::spawn(move || {
            daemon::serve_listener_until_shutdown(listener, server_shutdown)
        });
        Self {
            addr,
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, std::time::Duration::from_secs(2));
        let result = self.thread.take().expect("owned daemon thread").join();
        if !std::thread::panicking() {
            result
                .expect("join owned daemon")
                .expect("daemon drained its helpers");
        }
    }
}

#[test]
fn runner_reference_byte_selection_downloads_through_real_daemon_http_after_producer_removal() {
    homeboy::test_support::with_isolated_home(|home| {
        const RUN: &str = "reference-http-proof";
        let lifecycle =
            homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                .expect("rooted lifecycle");
        let uri = format!("homeboy://agent-task/run/{RUN}/artifacts#task=producer&artifact=patch");
        let record: homeboy::agents::agent_task_lifecycle::AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1", "run_id": RUN, "plan_id": "reference-http-proof",
            "state": "succeeded", "submitted_at": "2026-10-08T00:00:00Z",
            "plan_path": home.path().join("plan.json"),
            "artifact_refs": [{"task_id": "producer", "kind": "patch", "uri": uri}]
        })).expect("durable lifecycle fixture");
        lifecycle
            .write_record(&record)
            .expect("persist lifecycle through owning API");
        homeboy::agents::orchestration::register();

        let store = homeboy::core::observation::ObservationStore::open_initialized()
            .expect("real observation store");
        let producer = home.path().join("producer.patch");
        let bytes = b"diff --git a/source b/source\n+retained HTTP proof\n";
        std::fs::write(&producer, bytes).expect("producer bytes");
        for index in 0..60 {
            store
                .record_artifact_with_id(
                    RUN,
                    "unrelated",
                    &producer,
                    &format!("unrelated-{index:02}"),
                    json!({}),
                )
                .expect("real inventory row");
        }
        let canonical = store
            .record_artifact_with_id(
                RUN,
                "patch",
                &producer,
                "controller-retained-patch",
                json!({
                    "agent_task": {"task_id": "producer", "logical_artifact_id": "patch"}
                }),
            )
            .expect("retain actual bytes under a non-derived canonical ID");
        std::fs::remove_file(&producer).expect("remove original producer before HTTP retrieval");

        let server = OwnedDaemon::start();
        let base = format!("http://{}", server.addr);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("HTTP client");
        let requested = std::cell::RefCell::new(Vec::new());
        // Consume the real daemon wire response exactly as daemon_api_get does:
        // outer success/data -> HttpApiResponse -> body -> ControlPlaneResult.
        let get = |path: &str| -> homeboy::core::Result<serde_json::Value> {
            requested.borrow_mut().push(path.to_string());
            let response = client
                .get(format!("{base}{path}"))
                .send()
                .expect("real daemon HTTP request");
            assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
            let wire: serde_json::Value = response.json().expect("actual daemon JSON envelope");
            assert_eq!(wire["success"], true, "{wire}");
            let data = wire["data"].clone();
            assert_eq!(data["status"], 200);
            if path.ends_with("?full=1") {
                let records: Vec<ArtifactRecord> =
                    serde_json::from_value(data["body"]["artifacts"].clone())
                        .expect("actual exhaustive inventory");
                assert!(records.len() >= 61);
                assert!(
                    records
                        .iter()
                        .position(|record| record.id == canonical.id)
                        .expect("target in inventory")
                        >= 50
                );
                assert!(data["body"]["page"].is_null());
            } else if path.contains("/artifacts/") {
                let envelope: ControlPlaneResult<ControlPlaneReference> =
                    serde_json::from_value(data["body"].clone())
                        .expect("actual reference resource envelope");
                assert_eq!(envelope.schema, CONTROL_PLANE_RESULT_SCHEMA);
                assert!(envelope.ok);
                let reference = envelope.resource.expect("reference resource");
                assert_eq!(reference.schema, CONTROL_PLANE_REFERENCE_SCHEMA);
                assert_eq!(reference.run.as_str(), RUN);
                assert_eq!(reference.uri, uri);
            }
            Ok(data)
        };
        let status = get(&format!("/v1/control-plane/runs/{RUN}")).expect("real status HTTP");
        let status: ControlPlaneResult<ControlPlaneRun> =
            serde_json::from_value(status["body"].clone()).expect("actual status envelope");
        let pointer = status
            .resource
            .expect("run resource")
            .artifacts
            .into_iter()
            .find(|reference| reference.uri == uri)
            .expect("actual projected status pointer");
        assert_ne!(pointer.id, canonical.id);

        for token in [pointer.id.clone(), format!("artifact/{}", pointer.id)] {
            requested.borrow_mut().clear();
            let selected = resolve_runner_artifact_record(RUN, &token, &get)
                .expect("HTTP status pointer selects canonical record");
            assert_eq!(
                requested.borrow().as_slice(),
                [
                    format!("/runs/{RUN}/artifacts?full=1"),
                    format!("/v1/control-plane/runs/{RUN}/artifacts/{}", pointer.id),
                ]
            );
            assert_eq!(selected.id, canonical.id);
            let destination = home.path().join(format!(
                "downloaded-{}.patch",
                token.starts_with("artifact/")
            ));
            let fetched = daemon::fetch_artifact_to_path(
                RUN,
                &selected.id,
                Some(base.clone()),
                Some(destination.clone()),
            )
            .expect("existing HTTP byte downloader");
            assert_eq!(
                fetched.content_url,
                format!("{base}/runs/{RUN}/artifacts/{}/content", canonical.id)
            );
            assert_eq!(fetched.size_bytes, bytes.len() as u64);
            assert_eq!(fetched.sha256, canonical.sha256);
            assert_eq!(
                std::fs::read(&destination).expect("HTTP-downloaded destination"),
                bytes
            );
            assert_eq!(
                homeboy::core::artifact_metadata::sha256_file(&destination)
                    .expect("independent destination digest"),
                canonical.sha256.clone().expect("retained digest")
            );
        }

        // Actual direct byte routes must stay exact, including after metadata
        // reference selection was introduced in the runner acquisition layer.
        for unauthorized in [
            pointer.id,
            "patch".to_string(),
            producer.display().to_string(),
            format!("file://{}", producer.display()),
        ] {
            let url = daemon::artifact_content_url(&base, RUN, &unauthorized)
                .expect("encoded exact route");
            let response = client
                .get(url)
                .send()
                .expect("direct negative HTTP request");
            assert_eq!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND,
                "{unauthorized}"
            );
        }
        for raw in [
            producer.display().to_string(),
            format!("file://{}", producer.display()),
        ] {
            assert!(resolve_runner_artifact_record(RUN, &raw, &get).is_err());
        }
    });
}
