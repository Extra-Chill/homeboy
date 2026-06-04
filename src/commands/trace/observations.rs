use std::path::Path;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::observation::ObservationStore;

use extension_trace::resolve_declared_trace_artifact_path;

pub(super) fn record_trace_artifacts(
    store: &ObservationStore,
    run_id: &str,
    run_dir: &RunDir,
    results: Option<&extension_trace::TraceResults>,
) {
    let trace_results_path =
        run_dir.step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS);
    record_artifact_if_file(store, run_id, "trace-results", &trace_results_path);
    let artifact_dir = run_dir.path().join("artifacts");
    record_artifact_dir_if_non_empty(store, run_id, "trace-artifacts", &artifact_dir);
    if let Some(results) = results {
        for artifact in &results.artifacts {
            if let Some(resolved) =
                resolve_declared_trace_artifact_path(&artifact.path, run_dir, &artifact_dir)
            {
                record_artifact_path(store, run_id, "trace-artifact", &resolved);
            }
        }
    }
}

fn record_artifact_if_file(store: &ObservationStore, run_id: &str, kind: &str, path: &Path) {
    if path.is_file() {
        let _ = store.record_artifact(run_id, kind, path);
    }
}

fn record_artifact_dir_if_non_empty(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    path: &Path,
) {
    if path.is_dir()
        && std::fs::read_dir(path)
            .ok()
            .and_then(|mut entries| entries.next())
            .is_some()
    {
        let _ = store.record_directory_artifact(run_id, kind, path);
    }
}

fn record_artifact_path(store: &ObservationStore, run_id: &str, kind: &str, path: &Path) {
    if path.is_dir() {
        let _ = store.record_directory_artifact(run_id, kind, path);
    } else if path.is_file() {
        let _ = store.record_artifact(run_id, kind, path);
    }
}
