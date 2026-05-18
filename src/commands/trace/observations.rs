use std::path::{Path, PathBuf};

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::observation::ObservationStore;

pub(super) fn record_trace_artifacts(
    store: &ObservationStore,
    run_id: &str,
    run_dir: &RunDir,
    results: Option<&extension_trace::TraceResults>,
) {
    let trace_results_path =
        run_dir.step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS);
    record_artifact_if_file(store, run_id, "trace-results", &trace_results_path);
    if let Some(results) = results {
        for artifact in &results.artifacts {
            let path = PathBuf::from(&artifact.path);
            let resolved = if path.is_absolute() {
                path
            } else {
                run_dir.path().join(path)
            };
            record_artifact_if_file(store, run_id, "trace-artifact", &resolved);
        }
    }
}

fn record_artifact_if_file(store: &ObservationStore, run_id: &str, kind: &str, path: &Path) {
    if path.is_file() {
        let _ = store.record_artifact(run_id, kind, path);
    }
}
