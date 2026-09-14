use super::{step_failed, step_success};
use crate::release::types::ReleaseStepResult;
use homeboy_core::component::Component;
use homeboy_core::error::Result;
use homeboy_core::plan::PlanStep;
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
