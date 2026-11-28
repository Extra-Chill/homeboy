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
    let commit = homeboy_core::git::get_head_commit(&component.local_path)?;
    // Listing by head ref also works when the release branch has been deleted.
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--base",
            base,
            "--head",
            head,
            "--json",
            "state,baseRefName,headRefName,isCrossRepository,mergeCommit",
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
            Some(vec![MERGE_HINT.to_string()]),
        ));
    }
    verify_merged_release_pr(&output.stdout, base, head, &commit)
}

const MERGE_HINT: &str =
    "Merge the release PR, check out its merge or squash commit, then retry --head --apply.";

fn verify_merged_release_pr(output: &[u8], base: &str, head: &str, commit: &str) -> Result<()> {
    let prs: Vec<ReleasePr> = serde_json::from_slice(output).map_err(|error| {
        Error::validation_invalid_argument(
            "protected-branch",
            format!("cannot read release PR state: {error}"),
            Some(head.to_string()),
            None,
        )
    })?;
    if prs.iter().any(|pr| {
        pr.state == "MERGED"
            && !pr.is_cross_repository
            && pr.base_ref_name == base
            && pr.head_ref_name == head
            && pr
                .merge_commit
                .as_ref()
                .is_some_and(|merge| merge.oid == commit)
    }) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "protected-branch",
        format!(
            "HEAD {commit} is not the merged commit of a release PR from '{head}' into '{base}'"
        ),
        Some(head.to_string()),
        Some(vec![MERGE_HINT.to_string()]),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleasePr {
    state: String,
    base_ref_name: String,
    head_ref_name: String,
    is_cross_repository: bool,
    merge_commit: Option<MergeCommit>,
}

#[derive(Deserialize)]
struct MergeCommit {
    oid: String,
}

#[cfg(test)]
mod tests {
    use super::verify_merged_release_pr;

    #[test]
    fn head_release_requires_a_merged_pr_for_the_version_and_default_branch() {
        for (state, base, head, fork, accepted) in [
            ("MERGED", "trunk", "release/v1.2.3", false, true),
            ("OPEN", "trunk", "release/v1.2.3", false, false),
            ("MERGED", "develop", "release/v1.2.3", false, false),
            ("MERGED", "trunk", "release/v1.2.2", false, false),
            ("MERGED", "trunk", "release/v1.2.3", true, false),
        ] {
            let output = serde_json::json!([{
                "state": state,
                "baseRefName": base,
                "headRefName": head,
                "isCrossRepository": fork,
                "mergeCommit": { "oid": "merge-sha" },
            }]);
            assert_eq!(
                verify_merged_release_pr(
                    output.to_string().as_bytes(),
                    "trunk",
                    "release/v1.2.3",
                    "merge-sha",
                )
                .is_ok(),
                accepted,
                "{output}"
            );
        }
    }
}
