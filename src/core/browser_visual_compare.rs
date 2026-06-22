//! Visual-compare provider orchestration.
//!
//! Owns the side-effecting parts of the browser-evidence visual compare flow:
//! creating the artifacts directory, writing the provider request file, invoking
//! the external visual-compare provider process, and parsing its JSON response.
//! The report command layer stays a thin adapter that maps the parsed value into
//! its presentation types.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::core::{Error, Result};

/// Inputs required to run the external visual-compare provider for one variant.
pub struct VisualCompareProviderRequest<'a> {
    pub artifacts_dir: &'a Path,
    pub source_screenshot: &'a str,
    pub candidate_screenshot: &'a str,
    pub baseline_label: &'a str,
    pub candidate_label: &'a str,
    pub threshold: Option<f64>,
    pub provider_command: &'a str,
    pub provider_args: &'a [String],
}

/// Create the artifacts directory, write the request file, run the provider
/// process, and return the parsed JSON response value.
pub fn run_visual_compare_provider(request: &VisualCompareProviderRequest<'_>) -> Result<Value> {
    std::fs::create_dir_all(request.artifacts_dir).map_err(|err| {
        Error::internal_io(
            format!(
                "Failed to create visual compare artifact directory {}: {}",
                request.artifacts_dir.display(),
                err
            ),
            Some("report.browser_evidence_compare.visual_artifacts".to_string()),
        )
    })?;
    let input_path = request
        .artifacts_dir
        .join("homeboy-visual-compare-input.json");
    let mut input = serde_json::json!({
        "schema": "homeboy/visual-compare-request/v1",
        "source_screenshot": request.source_screenshot,
        "candidate_screenshot": request.candidate_screenshot,
        "source_label": request.baseline_label,
        "candidate_label": request.candidate_label,
        "artifacts_directory": request.artifacts_dir,
    });
    if let Some(threshold) = request.threshold {
        input["threshold"] = serde_json::json!(threshold);
    }
    std::fs::write(
        &input_path,
        serde_json::to_string_pretty(&input).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("report.browser_evidence_compare.visual_input".to_string()),
            )
        })?,
    )
    .map_err(|err| {
        Error::internal_io(
            format!(
                "Failed to write visual compare input {}: {}",
                input_path.display(),
                err
            ),
            Some("report.browser_evidence_compare.visual_input".to_string()),
        )
    })?;

    let output = Command::new(request.provider_command)
        .args(request.provider_args)
        .arg(&input_path)
        .output()
        .map_err(|err| {
            Error::internal_unexpected(format!(
                "Failed to invoke visual compare provider `{}`: {}",
                request.provider_command, err
            ))
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "Visual compare provider `{}` failed with status {:?}: {}{}",
            request.provider_command,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )));
    }
    serde_json::from_slice::<Value>(&output.stdout).map_err(|err| {
        Error::internal_json(
            format!("Failed to parse visual compare provider output: {}", err),
            Some("report.browser_evidence_compare.visual_output".to_string()),
        )
    })
}
