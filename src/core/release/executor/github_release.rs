//! GitHub Release helper result builders and probes.

use crate::core::deploy::release_download::GitHubRepo;

use super::step_success;
use crate::core::release::types::ReleaseStepResult;

pub(super) fn skipped_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    fallback_command: Option<String>,
) -> ReleaseStepResult {
    let mut data = serde_json::json!({
        "skipped": true,
        "reason": reason,
        "tag": tag,
        "owner": github.owner,
        "repo": github.repo,
    });
    if let Some(fallback) = fallback_command {
        data["fallback_command"] = serde_json::json!(fallback);
    }

    step_success("github.release", "github.release", Some(data), Vec::new())
}

pub(super) fn upload_failed_result(
    tag: &str,
    github: &GitHubRepo,
    stdout: String,
    stderr: String,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "skipped": true,
            "reason": "gh-upload-failed",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "stdout": stdout,
            "stderr": stderr,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
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
            "owner": github.owner,
            "repo": github.repo,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
    )
}

pub(super) fn gh_is_available() -> bool {
    crate::core::git::gh_probe_succeeds(&["--version"])
}

pub(super) fn gh_is_authenticated() -> bool {
    crate::core::git::gh_probe_succeeds(&["auth", "status", "--hostname", "github.com"])
}

pub(super) fn gh_release_exists(tag: &str, repo_flag: &str) -> bool {
    crate::core::git::gh_probe_succeeds(&["release", "view", tag, "-R", repo_flag])
}

pub(super) fn fallback_gh_command(tag: &str) -> String {
    format!(
        "gh release create {} --title {} --notes-file <path-to-release-notes>",
        tag, tag
    )
}

pub(super) fn sanitize_tag_for_filename(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
