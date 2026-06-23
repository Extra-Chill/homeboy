use std::fs;
use std::path::PathBuf;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::bench::BenchRunWorkflowResult;

use super::lifecycle::BenchObservation;
use crate::commands::bench::BenchRunArgs;

pub(super) trait BenchStatusTarget {
    fn status_file(&self) -> Option<PathBuf>;
}

impl BenchStatusTarget for BenchRunArgs {
    fn status_file(&self) -> Option<PathBuf> {
        self.status_file.clone()
    }
}

impl BenchStatusTarget for serde_json::Value {
    fn status_file(&self) -> Option<PathBuf> {
        self.get("status_file")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
    }
}

pub(super) fn write_status_file<T: BenchStatusTarget>(
    target: &T,
    run_dir: &RunDir,
    observation: &BenchObservation,
    status: &str,
    workflow: Option<&BenchRunWorkflowResult>,
    error: Option<&homeboy::core::Error>,
) {
    let Some(path) = target.status_file() else {
        return;
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let _ = fs::create_dir_all(parent);
    }
    let artifacts = observation
        .0
        .store()
        .list_artifacts(observation.run_id())
        .unwrap_or_default();
    let payload = serde_json::json!({
        "schema": "homeboy/bench-status/v1",
        "command": "bench.status",
        "run_id": observation.run_id(),
        "kind": observation.0.run().kind,
        "status": status,
        "updated_at": chrono::Utc::now().to_rfc3339(),
        "started_at": observation.0.run().started_at,
        "finished": workflow.is_some() || error.is_some(),
        "component_id": observation.0.component_id(),
        "rig_id": observation.0.rig_id(),
        "homeboy_version": observation.0.run().homeboy_version,
        "git_sha": observation.0.run().git_sha,
        "cwd": observation.0.run().cwd,
        "run_dir": run_dir.path().to_string_lossy().to_string(),
        "observation_store": observation.0.store_path(),
        "artifact_count": artifacts.len(),
        "artifacts": artifacts,
        "failure": workflow.and_then(|workflow| workflow.failure.as_ref()),
        "failure_classification": workflow.and_then(|workflow| workflow.results.as_ref()?.failure_classification.as_ref()),
        "phase_summaries": workflow.and_then(|workflow| workflow.results.as_ref().map(|results| results.phase_summaries.clone())).unwrap_or_default(),
        "error": error.map(|error| error.to_string()),
        "exit_code": workflow.map(|workflow| workflow.exit_code),
        "gate_failures": workflow.map(|workflow| workflow.gate_failures.clone()).unwrap_or_default(),
        "hints": workflow.and_then(|workflow| workflow.hints.clone()).unwrap_or_default(),
    });
    if let Ok(json) = serde_json::to_vec_pretty(&payload) {
        let _ = fs::write(path, json);
    }
}
