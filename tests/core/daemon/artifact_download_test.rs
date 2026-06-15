use super::*;
use crate::core::observation::ArtifactRecord;
use crate::core::observation::NewRunRecord;
use crate::test_support::HomeGuard;

#[test]
fn test_route() {
    let _home = HomeGuard::new();
    let home_path = std::path::PathBuf::from(std::env::var("HOME").expect("home"));
    let store = ObservationStore::open_initialized().expect("store");
    let run = store
        .start_run(
            NewRunRecord::builder("runner-exec")
                .command("homeboy runner exec")
                .homeboy_version("test-version")
                .build(),
        )
        .expect("run");
    let artifact_path = home_path.join("artifact.txt");
    fs::write(&artifact_path, "artifact body").expect("artifact file");
    let artifact = store
        .record_artifact(&run.id, "lab_fix_patch", &artifact_path)
        .expect("artifact");

    let response =
        route(&format!("/runs/{}/artifacts/{}", run.id, artifact.id)).expect("artifact route");

    assert_eq!(response.status_code, 200);
    assert!(response.artifact.is_some());
    assert_eq!(response.body["artifact"]["id"], artifact.id);
}

#[test]
fn route_serves_run_scoped_artifact_store_tokens() {
    let _home = HomeGuard::new();
    let home_path = std::path::PathBuf::from(std::env::var("HOME").expect("home"));
    let store = ObservationStore::open_initialized().expect("store");
    let run = store
        .start_run(
            NewRunRecord::builder("bench")
                .command("homeboy bench")
                .homeboy_version("test-version")
                .build(),
        )
        .expect("run");
    let locator =
        "homeboy/workflow-bench/runs/run-1/artifacts/scenario/adapter/attempt-1/summary.json";
    let artifact_root = home_path.join(".local/share/homeboy/artifacts");
    let path = artifact_root.join(locator);
    fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact parent");
    fs::write(&path, br#"{"ok":true}"#).expect("artifact-store file");
    let token = crate::core::runner::runner_artifact_store_token("lab", &run.id, locator)
        .rsplit('/')
        .next()
        .expect("artifact token")
        .to_string();

    let response =
        route(&format!("/runs/{}/artifacts/{}", run.id, token)).expect("artifact-store route");

    assert_eq!(response.status_code, 200);
    let artifact = response.artifact.expect("artifact download");
    assert_eq!(artifact.filename, "summary.json");
    assert_eq!(artifact.size_bytes, 11);
    assert_eq!(response.body["artifact"]["run_id"], run.id);
}

#[test]
fn route_serves_percent_encoded_run_scoped_artifact_ids() {
    let _home = HomeGuard::new();
    let home_path = std::path::PathBuf::from(std::env::var("HOME").expect("home"));
    let store = ObservationStore::open_initialized().expect("store");
    let run = store
        .start_run(
            NewRunRecord::builder("bench")
                .command("homeboy bench")
                .homeboy_version("test-version")
                .build(),
        )
        .expect("run");
    let artifact_path = home_path.join("summary.json");
    fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact file");
    let artifact = ArtifactRecord {
        id: "report/summary".to_string(),
        run_id: run.id.clone(),
        kind: "summary".to_string(),
        artifact_type: "file".to_string(),
        path: artifact_path.to_string_lossy().to_string(),
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
    store.import_artifact(&artifact).expect("import artifact");

    let response = route(&format!("/runs/{}/artifacts/report%2Fsummary", run.id))
        .expect("encoded artifact route");

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body["artifact"]["id"], artifact.id);
    assert_eq!(
        response.artifact.expect("artifact download").filename,
        "summary.json"
    );
}
