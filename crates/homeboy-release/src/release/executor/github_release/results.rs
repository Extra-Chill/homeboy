//! `ReleaseStepResult` builders for each GitHub Release outcome.

use crate::release::types::ReleaseStepResult;
use homeboy_core::git::release_download::GitHubRepo;

use super::super::{step_failed, step_success};
use super::gh_cli::{gh_diagnostic_text, GitHubCommandFailureDiagnostic, ReleaseAssetPublication};
use super::notes::GitHubReleaseBody;
use super::repair::{
    existing_draft_repair_hints, repair_data, repair_hints, GitHubReleaseRepairCommands,
};

fn github_command_error_details(
    diagnostics: &[GitHubCommandFailureDiagnostic],
) -> Option<serde_json::Value> {
    (!diagnostics.is_empty()).then(|| {
        serde_json::json!({
            "code": "github_command_failed",
            "failures": diagnostics,
        })
    })
}

pub(crate) fn published_release_url(
    github: &GitHubRepo,
    tag: &str,
    _draft_response: &str,
    publish_response: &str,
) -> String {
    let published_url = publish_response.trim();
    if !published_url.is_empty() {
        return published_url.to_string();
    }

    format!(
        "https://{}/{}/{}/releases/tag/{}",
        github.host, github.owner, github.repo, tag
    )
}

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

/// An existing GitHub Release was still an unpublished Draft and this run
/// published it (issue #10441).
///
/// The tag was already durable on `origin` — the cargo-dist build matrix checks
/// the tag out, so it must exist before any artifact can be built — which means
/// the Draft was a pushed tag that shipped nothing. Publishing it is the action
/// that makes the tag deliverable, so this is a real `Success`, not a skip.
pub(crate) fn published_existing_draft_result(
    tag: &str,
    github: &GitHubRepo,
    asset_count: usize,
    url: &str,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release.publish_existing_draft",
            "skipped": false,
            "reason": "draft-release-published",
            "tag": tag,
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
            "url": url,
            "artifact_count": asset_count,
            "published": true,
        })),
        Vec::new(),
    )
}

/// A GitHub Release exists for the tag but this run could neither confirm nor
/// make it published (issue #10441).
///
/// This must be `Failed`. The tag is already on `origin`, so reporting success
/// here publishes nothing while telling every downstream consumer — and the
/// `verify-published` release-workflow gate — that the version shipped. The
/// three states that land here are: the release's publication state could not
/// be read, `gh release edit --draft=false` failed, and an empty Draft that
/// must not be published over.
pub(crate) fn unfinished_release_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    error: &str,
    repair: GitHubReleaseRepairCommands,
    diagnostics: &[GitHubCommandFailureDiagnostic],
) -> ReleaseStepResult {
    let mut data = serde_json::json!({
        "skipped": false,
        "release_created": true,
        "published": false,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "repair": repair_data(&repair),
    });
    if let Some(details) = github_command_error_details(diagnostics) {
        data["error_details"] = details;
    }

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error.to_string()),
        existing_draft_repair_hints(&repair),
    )
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
    output: &super::gh_cli::GhCommandOutput,
    repair: GitHubReleaseRepairCommands,
    body: &GitHubReleaseBody,
    persisted_notes_path: Option<&str>,
    diagnostics: &[GitHubCommandFailureDiagnostic],
) -> ReleaseStepResult {
    let stdout = gh_diagnostic_text(&output.stdout);
    let stderr = gh_diagnostic_text(&output.stderr);
    let mut data = serde_json::json!({
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
    if let Some(details) = github_command_error_details(diagnostics) {
        data["error_details"] = details;
    }

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
    exit_code: Option<i32>,
    timed_out: bool,
    artifact_count: usize,
    repair: GitHubReleaseRepairCommands,
    diagnostics: &[GitHubCommandFailureDiagnostic],
) -> ReleaseStepResult {
    let stdout = gh_diagnostic_text(&stdout);
    let stderr = gh_diagnostic_text(&stderr);
    let mut data = serde_json::json!({
        "skipped": false,
        "release_created": true,
        "reason": "gh-upload-failed",
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "stdout": stdout,
        "stderr": stderr.clone(),
        "exit_code": exit_code,
        "timed_out": timed_out,
        "artifact_count": artifact_count,
        "repair": repair_data(&repair),
    });
    if let Some(details) = github_command_error_details(diagnostics) {
        data["error_details"] = details;
    }

    let detail = stderr.trim();
    let error = if timed_out {
        format!("`gh release upload` timed out for {}", tag)
    } else if detail.is_empty() {
        format!("`gh release upload` failed for {}", tag)
    } else {
        format!("`gh release upload` failed for {}: {}", tag, detail)
    };

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error),
        existing_draft_repair_hints(&repair),
    )
}

pub(crate) fn upload_success_result(
    tag: &str,
    github: &GitHubRepo,
    artifact_count: usize,
) -> ReleaseStepResult {
    upload_success_result_with_publications(tag, github, artifact_count, &[])
}

pub(crate) fn upload_success_result_with_publications(
    tag: &str,
    github: &GitHubRepo,
    artifact_count: usize,
    publications: &[ReleaseAssetPublication],
) -> ReleaseStepResult {
    let url = published_release_url(github, tag, "", "");
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release.upload",
            "tag": tag,
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
            "url": url,
            "artifact_count": artifact_count,
            "asset_publications": publications,
        })),
        Vec::new(),
    )
}
