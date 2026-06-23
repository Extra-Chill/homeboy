//! Public-preview session lifecycle for trace runs.

use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};

use super::super::parsing::{TraceAssertion, TraceAssertionStatus, TraceResults, TraceStatus};
use super::super::preview::{TracePreviewMetadata, TracePublicPreviewSession};
use super::types::TraceRunWorkflowArgs;

pub(super) fn start_trace_public_preview(
    args: &mut TraceRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<Option<TracePublicPreviewSession>> {
    let Some(spec) = args.runner_inputs.public_preview.clone() else {
        return Ok(None);
    };
    let artifact_dir = run_dir.path().join("artifacts");
    let session = TracePublicPreviewSession::start_with_artifact_dir(&spec, Some(&artifact_dir))?;
    args.runner_inputs.env.extend(session.env_vars()?);
    Ok(Some(session))
}

pub(super) fn finish_trace_public_preview(
    session: Option<TracePublicPreviewSession>,
    run_dir: &RunDir,
) -> Result<Option<TracePreviewMetadata>> {
    let Some(session) = session else {
        return Ok(None);
    };
    let metadata = session.finish();
    let artifact_dir = run_dir.path().join("artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace preview artifact dir {}: {}",
                artifact_dir.display(),
                e
            ),
            Some("trace.preview.artifact_dir".to_string()),
        )
    })?;
    let path = artifact_dir.join("preview.json");
    let content = serde_json::to_string_pretty(&metadata).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize trace preview artifact: {e}"),
            Some("trace.preview.artifact_serialize".to_string()),
        )
    })?;
    std::fs::write(&path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace preview artifact {}: {e}",
                path.display()
            ),
            Some("trace.preview.artifact_write".to_string()),
        )
    })?;
    Ok(Some(metadata))
}

pub(super) fn apply_trace_preview_metadata(
    results: &mut TraceResults,
    preview: Option<&TracePreviewMetadata>,
) {
    let Some(preview) = preview else {
        return;
    };
    results.preview = Some(preview.clone());
    if !results
        .artifacts
        .iter()
        .any(|artifact| artifact.path == "artifacts/preview.json")
    {
        results
            .artifacts
            .push(super::super::parsing::TraceArtifact {
                label: "Public preview metadata".to_string(),
                path: "artifacts/preview.json".to_string(),
                kind: None,
            });
    }
    if preview.require_https {
        let status = if preview.window_is_secure_context {
            TraceAssertionStatus::Pass
        } else {
            results.status = TraceStatus::Error;
            TraceAssertionStatus::Error
        };
        results.assertions.push(TraceAssertion {
            id: "public_preview.secure_context".to_string(),
            status,
            message: Some(format!(
                "Browser effective origin `{}` secure_context={}",
                preview.browser_effective_origin, preview.window_is_secure_context
            )),
            details: Some(serde_json::json!({
                "requested_mode": preview.requested_mode,
                "local_origin": preview.local_origin,
                "public_origin": preview.public_origin,
                "browser_effective_origin": preview.browser_effective_origin,
                "window_location_origin": preview.window_location_origin,
                "window_is_secure_context": preview.window_is_secure_context
            })),
        });
    }
}
