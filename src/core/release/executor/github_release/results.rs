//! `ReleaseStepResult` builders for each GitHub Release outcome.

use crate::core::deploy::release_download::GitHubRepo;
use crate::core::release::types::ReleaseStepResult;

use super::super::{step_failed, step_success};
use super::notes::GitHubReleaseBody;
use super::repair::{repair_data, repair_hints, GitHubReleaseRepairCommands};

/// A successful but no-op result for an idempotent retry where the GitHub
/// Release object already exists. The release exists, so this is `Success`.
pub(super) fn skipped_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    repair: Option<GitHubReleaseRepairCommands>,
) -> ReleaseStepResult {
    let mut data = serde_json::json!({
        "skipped": true,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
    });
    if let Some(repair) = repair {
        data["fallback_command"] = serde_json::json!(repair.create_command.clone());
        data["repair"] = repair_data(&repair);
    }

    step_success("github.release", "github.release", Some(data), Vec::new())
}

/// The GitHub Release object was NOT created and cannot be recovered in this
/// run (no `gh` binary / not authenticated). This must be `Failed`, not a
/// success-with-`skipped`, so the release pipeline halts before publish/upload
/// steps run against a release that does not exist (issue #3541).
pub(crate) fn not_created_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    error: &str,
    repair: GitHubReleaseRepairCommands,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": false,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "fallback_command": repair.create_command.clone(),
        "repair": repair_data(&repair),
    });

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error.to_string()),
        repair_hints(&repair),
    )
}

/// `gh release create` failed, so no GitHub Release object exists. `Failed`,
/// carrying the recovery commands so the operator can finish the release from
/// the already-pushed tag + built artifacts without making a second tag.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_failed_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    stdout: String,
    stderr: String,
    repair: GitHubReleaseRepairCommands,
    body: &GitHubReleaseBody,
    persisted_notes_path: Option<&str>,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": false,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "stdout": stdout,
        "stderr": stderr.clone(),
        "fallback_command": repair.create_command.clone(),
        "repair": repair_data(&repair),
        // Expose the EXACT body Homeboy attempted to post + its persisted copy
        // so manual recovery reproduces the identical release body (issue #3508).
        "release_body": body.body,
        "release_body_source": body.source_label(),
        "release_body_file": persisted_notes_path,
    });

    let detail = stderr.trim();
    let error = if detail.is_empty() {
        format!("`gh release create` failed for {}", tag)
    } else {
        format!("`gh release create` failed for {}: {}", tag, detail)
    };

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error),
        repair_hints(&repair),
    )
}

/// The GitHub Release exists but attaching the build artifacts failed. The
/// step is responsible for the full release lifecycle (entry + assets), so a
/// failed asset upload is a `Failed` step: downstream consumers would
/// otherwise assume the assets are present.
pub(crate) fn upload_failed_result(
    tag: &str,
    github: &GitHubRepo,
    stdout: String,
    stderr: String,
    artifact_count: usize,
    repair: GitHubReleaseRepairCommands,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": true,
        "reason": "gh-upload-failed",
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "stdout": stdout,
        "stderr": stderr.clone(),
        "artifact_count": artifact_count,
        "repair": repair_data(&repair),
    });

    let detail = stderr.trim();
    let error = if detail.is_empty() {
        format!("`gh release upload` failed for {}", tag)
    } else {
        format!("`gh release upload` failed for {}: {}", tag, detail)
    };

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error),
        repair_hints(&repair),
    )
}

pub(super) fn upload_success_result(
    tag: &str,
    github: &GitHubRepo,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release.upload",
            "tag": tag,
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
    )
}
