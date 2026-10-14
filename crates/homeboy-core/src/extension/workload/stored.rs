//! Read a persisted workload run back: the run row, its envelope and the
//! artifacts the envelope listed.

use std::path::PathBuf;

use homeboy_extension_contract::{WorkloadCompareConfig, WorkloadResult};

use crate::error::{Error, Result};
use crate::observation::{runs_service, ArtifactRecord, ObservationStore, RunRecord};

use super::{WORKLOAD_ARTIFACT_KIND, WORKLOAD_COMPARE_METADATA_KEY, WORKLOAD_RESULT_ARTIFACT_KIND};

/// A persisted workload run with its parsed envelope.
#[derive(Debug, Clone)]
pub struct StoredWorkloadRun {
    pub run: RunRecord,
    pub result: WorkloadResult,
    pub result_file: PathBuf,
    pub result_artifact: ArtifactRecord,
    pub artifacts: Vec<ArtifactRecord>,
}

impl StoredWorkloadRun {
    /// The compare semantics the kind declared when this run executed.
    pub fn compare_config(&self) -> WorkloadCompareConfig {
        self.run
            .metadata_json
            .get(WORKLOAD_COMPARE_METADATA_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    pub fn extension_id(&self) -> Option<&str> {
        self.run
            .metadata_json
            .get("extension_id")
            .and_then(serde_json::Value::as_str)
    }

    pub fn source_path(&self) -> Option<&str> {
        self.run
            .metadata_json
            .get("source_path")
            .and_then(serde_json::Value::as_str)
    }

    /// The persisted file for an envelope artifact name, when its bytes are
    /// local.
    pub fn artifact_file(&self, name: &str) -> Option<PathBuf> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.kind == WORKLOAD_ARTIFACT_KIND)
            .find(|artifact| {
                artifact
                    .metadata_json
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    == Some(name)
            })
            .map(|artifact| PathBuf::from(&artifact.path))
            .filter(|path| path.is_file())
    }
}

/// Load run `run_id` and its envelope. `kind`, when given, must match.
pub fn load(
    store: &ObservationStore,
    run_id: &str,
    kind: Option<&str>,
) -> Result<StoredWorkloadRun> {
    let run = runs_service::require_run(store, run_id)?;
    if let Some(kind) = kind {
        if run.kind != kind {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run `{}` is a `{}` run, not `{kind}`", run.id, run.kind),
                Some(run_id.to_string()),
                None,
            ));
        }
    }
    let artifacts = runs_service::list_artifacts_for_run(store, &run.id)?;
    let result_artifact = artifacts
        .iter()
        .filter(|artifact| artifact.kind == WORKLOAD_RESULT_ARTIFACT_KIND)
        .filter(|artifact| {
            artifact
                .metadata_json
                .get("valid")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .last()
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!(
                    "run `{}` has no valid {WORKLOAD_RESULT_ARTIFACT_KIND} artifact; it was not produced by `homeboy workload run` or its runner wrote no valid envelope",
                    run.id
                ),
                Some(run_id.to_string()),
                None,
            )
        })?;
    let mut result_file = PathBuf::from(&result_artifact.path);
    if !result_file.is_file() {
        result_file =
            runs_service::download_remote_artifact(result_artifact.clone(), None)?.output_path;
    }
    let bytes = std::fs::read(&result_file).map_err(|error| {
        Error::internal_io(error.to_string(), Some(result_file.display().to_string()))
    })?;
    let result = WorkloadResult::parse(&bytes, &run.kind, None).map_err(|problem| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run `{}` envelope is invalid: {problem}", run.id),
            Some(run_id.to_string()),
            None,
        )
    })?;
    Ok(StoredWorkloadRun {
        run,
        result,
        result_file,
        result_artifact,
        artifacts,
    })
}
