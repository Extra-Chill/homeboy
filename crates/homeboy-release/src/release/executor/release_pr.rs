use super::{step_failed, step_success};
use crate::release::types::ReleaseStepResult;
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::plan::PlanStep;
use serde::Deserialize;
use std::process::Command;

/// Open the review boundary for a protected-default-branch release. GitHub owns
/// checks and merge policy; Homeboy only supplies the prepared release branch.
pub(crate) fn run_release_pr(step: &PlanStep, component: &Component) -> Result<ReleaseStepResult> {
    let value = |name: &str| {
        step.inputs
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
    };
    let base = value("base");
    let head = value("head");
    let title = value("title");
    let output = Command::new("gh")
        .args([
            "pr", "create", "--base", base, "--head", head, "--title", title,
        ])
        .args([
            "--body",
            "Release commit prepared by Homeboy; merge after required checks pass.",
        ])
        .current_dir(&component.local_path)
        .output()
        .map_err(|error| {
            homeboy_core::error::Error::git_command_failed(format!("run gh pr create: {error}"))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let data = serde_json::json!({ "base": base, "head": head, "url": stdout });
    if output.status.success() {
        return Ok(step_success(
            "github.release_pr",
            "github.release_pr",
            Some(data),
            Vec::new(),
        ));
    }
    Ok(step_failed(
        "github.release_pr",
        "github.release_pr",
        Some(data),
        Some(if stderr.is_empty() {
            "gh pr create failed".to_string()
        } else {
            stderr
        }),
        Vec::new(),
    ))
}

/// The tag is immutable, so protected-branch finalization must prove that the
/// reviewed release PR reached its declared default branch before creating it.
pub(crate) fn require_merged_release_pr(
    component: &Component,
    base: &str,
    head: &str,
) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            head,
            "--json",
            "state,baseRefName,headRefName",
        ])
        .current_dir(&component.local_path)
        .output()
        .map_err(|error| Error::git_command_failed(format!("inspect release PR: {error}")))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "protected-branch",
            format!(
                "cannot verify that release PR '{}' was merged: {}",
                head,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(head.to_string()),
            Some(vec![
                "Merge the release PR normally, then retry --protected-branch --head.".to_string(),
            ]),
        ));
    }
    let pr: ReleasePr = serde_json::from_slice(&output.stdout).map_err(|error| {
        Error::validation_invalid_argument(
            "protected-branch",
            format!("cannot read release PR state: {error}"),
            Some(head.to_string()),
            None,
        )
    })?;
    if pr.state == "MERGED" && pr.base_ref_name == base && pr.head_ref_name == head {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "protected-branch",
        format!(
            "release PR must be merged from '{}' into '{}'; found state '{}' from '{}' into '{}'",
            head, base, pr.state, pr.head_ref_name, pr.base_ref_name
        ),
        Some(head.to_string()),
        Some(vec![
            "Merge the release PR normally, then retry --protected-branch --head.".to_string(),
        ]),
    ))
}

#[derive(Deserialize)]
struct ReleasePr {
    #[serde(rename = "state")]
    state: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

#[cfg(test)]
mod tests {
    use super::ReleasePr;

    #[test]
    fn release_pr_state_carries_the_exact_merged_boundary() {
        let pr: ReleasePr = serde_json::from_str(
            r#"{"state":"MERGED","baseRefName":"main","headRefName":"release/v1.0.0"}"#,
        )
        .expect("parse gh PR response");

        assert_eq!(pr.state, "MERGED");
        assert_eq!(pr.base_ref_name, "main");
        assert_eq!(pr.head_ref_name, "release/v1.0.0");
    }
}
