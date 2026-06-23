//! Trace artifact path resolution, validation, and results persistence.

use std::path::{Path, PathBuf};

use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};

use super::super::parsing::{TraceAssertion, TraceAssertionStatus, TraceResults, TraceStatus};

pub(super) fn validate_declared_trace_artifacts(
    results: &mut TraceResults,
    run_dir: &RunDir,
    artifact_dir: &Path,
) {
    let missing = results
        .artifacts
        .iter()
        .filter(|artifact| {
            resolve_declared_trace_artifact_path(&artifact.path, run_dir, artifact_dir).is_none()
        })
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }

    results.status = TraceStatus::Error;
    results.failure = Some(format!(
        "missing declared trace artifact{}: {}",
        if missing.len() == 1 { "" } else { "s" },
        missing.join(", ")
    ));
    for path in missing {
        results.assertions.push(TraceAssertion {
            id: format!("trace_artifact_exists:{}", path),
            status: TraceAssertionStatus::Error,
            message: Some(format!("Declared trace artifact is missing: {path}")),
            details: Some(serde_json::json!({ "path": path })),
        });
    }
}

pub fn resolve_declared_trace_artifact_path(
    path: &str,
    run_dir: &RunDir,
    artifact_dir: &Path,
) -> Option<PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        return relative.exists().then(|| relative.to_path_buf());
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }

    [run_dir.path().join(relative), artifact_dir.join(relative)]
        .into_iter()
        .find(|candidate| candidate.exists())
}

pub(super) fn persist_trace_results(path: &Path, results: &TraceResults) -> Result<()> {
    let content = serde_json::to_string_pretty(results).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize trace results JSON: {}", e),
            Some("trace.results.serialize".to_string()),
        )
    })?;
    std::fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace results file {}: {}",
                path.display(),
                e
            ),
            Some("trace.results.write".to_string()),
        )
    })
}
