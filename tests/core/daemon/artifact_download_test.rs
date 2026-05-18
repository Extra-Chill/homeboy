use super::*;
use crate::observation::NewRunRecord;
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
