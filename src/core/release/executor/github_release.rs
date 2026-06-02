//! GitHub Release helper result builders and probes.

use crate::core::component::Component;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};
use crate::core::release::changelog;
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::step_success;

/// Create a GitHub Release for the just-pushed tag. Fails soft in every
/// plausible failure mode (no `gh` binary, not authenticated, release already
/// exists, `gh release create` errors) — the tag is already pushed by the
/// time this runs and we don't want to mark an otherwise-successful release
/// as failed.
pub(crate) fn run_github_release(
    component: &Component,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let tag = state.tag.clone().ok_or_else(|| {
        Error::internal_unexpected(
            "github.release: tag state not set (git.tag must run first)".to_string(),
        )
    })?;
    let local_path = &component.local_path;

    let remote_url = component
        .remote_url
        .clone()
        .or_else(|| {
            crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                local_path,
            ))
        })
        .ok_or_else(|| {
            Error::internal_unexpected(
                "github.release: no remote_url configured and git remote get-url origin failed"
                    .to_string(),
            )
        })?;

    let github =
        crate::core::deploy::release_download::parse_github_url(&remote_url).ok_or_else(|| {
            Error::validation_invalid_argument(
                "github.release",
                format!("Remote URL '{}' is not a GitHub URL", remote_url),
                None,
                Some(vec![
                    "Only github.com remotes are supported for automatic GitHub Releases"
                        .to_string(),
                    "Use --no-github-release to skip this step".to_string(),
                ]),
            )
        })?;

    if !gh_is_available() {
        let fallback = fallback_gh_command(&tag);
        log_status!(
            "release",
            "⚠ `gh` CLI not found on PATH — skipping GitHub Release creation"
        );
        log_status!("release", "Manual fallback: {}", fallback);
        return Ok(skipped_result(
            &tag,
            &github,
            "gh-not-available",
            Some(fallback),
        ));
    }

    if !gh_is_authenticated() {
        let fallback = fallback_gh_command(&tag);
        log_status!(
            "release",
            "⚠ `gh` is not authenticated — skipping GitHub Release creation"
        );
        log_status!(
            "release",
            "Authenticate with `gh auth login`, then manual fallback: {}",
            fallback
        );
        return Ok(skipped_result(
            &tag,
            &github,
            "gh-not-authenticated",
            Some(fallback),
        ));
    }

    // Collect artifact paths from state. Populated by release.package
    // (or any other extension action that emits artifact metadata into
    // ReleaseState::artifacts). Passing these to `gh release create` or
    // `gh release upload --clobber` attaches them to the Release in a
    // single API call — keeping the github.release step responsible for
    // the full Release lifecycle (entry + assets) instead of requiring a
    // separate publish.<target> step.
    let artifact_paths: Vec<String> = state
        .artifacts
        .iter()
        .filter(|artifact| {
            std::fs::metadata(&artifact.path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        })
        .map(|artifact| artifact.path.clone())
        .collect();
    let has_artifacts = !artifact_paths.is_empty();

    let repo_flag = format!("{}/{}", github.owner, github.repo);
    if gh_release_exists(&tag, &repo_flag) {
        // Release entry already exists (idempotent retry, or release
        // created out of band). When the release has no artifacts to
        // attach, skip — there is nothing to update. When artifacts are
        // present, upload them with --clobber so retries keep the latest
        // build attached without duplicating the GitHub Release entry.
        if !has_artifacts {
            log_status!(
                "release",
                "GitHub Release {} already exists for {} — skipping (idempotent)",
                tag,
                repo_flag
            );
            return Ok(skipped_result(
                &tag,
                &github,
                "release-already-exists",
                None,
            ));
        }

        log_status!(
            "release",
            "GitHub Release {} already exists for {} — uploading {} artifact(s) with --clobber",
            tag,
            repo_flag,
            artifact_paths.len()
        );

        let mut upload_args: Vec<&str> = vec!["release", "upload", &tag];
        for path in &artifact_paths {
            upload_args.push(path);
        }
        upload_args.extend_from_slice(&["--clobber", "-R", &repo_flag]);

        let upload_output = std::process::Command::new("gh")
            .args(&upload_args)
            .output()
            .map_err(|e| {
                Error::internal_io(
                    format!("Failed to invoke gh: {}", e),
                    Some("gh release upload".to_string()),
                )
            })?;

        if !upload_output.status.success() {
            let stderr = String::from_utf8_lossy(&upload_output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&upload_output.stdout).to_string();
            log_status!("release", "⚠ `gh release upload` failed: {}", stderr.trim());
            return Ok(upload_failed_result(
                &tag,
                &github,
                stdout,
                stderr,
                artifact_paths.len(),
            ));
        }

        return Ok(upload_success_result(&tag, &github, artifact_paths.len()));
    }

    let notes_start_tag = github_generated_notes_start_tag(component, &tag)?;
    let generated_notes = match github_generated_notes(&github, &tag, notes_start_tag.as_deref()) {
        Ok(notes) => notes,
        Err(err) => {
            let fallback = fallback_gh_command(&tag);
            log_status!(
                "release",
                "⚠ GitHub generated release notes failed: {}",
                err
            );
            log_status!("release", "Manual fallback: {}", fallback);
            return Ok(skipped_result(
                &tag,
                &github,
                "generated-notes-failed",
                Some(fallback),
            ));
        }
    };
    let changelog_url = github_changelog_url(component, &github, &tag);
    let release_notes = changelog_url
        .as_deref()
        .map(|url| replace_full_changelog_footer(&generated_notes, url))
        .unwrap_or(generated_notes);

    log_status!(
        "release",
        "Creating GitHub Release {} on {} with {} artifact(s)...",
        tag,
        repo_flag,
        artifact_paths.len()
    );

    // Build args dynamically so we can append artifact paths as positional
    // arguments — `gh release create <tag> [files...]` attaches each file
    // as a Release asset in the same API call.
    let mut create_args: Vec<&str> = vec![
        "release",
        "create",
        &tag,
        "--title",
        &tag,
        "--notes",
        &release_notes,
        "-R",
        &repo_flag,
    ];
    if let Some(previous_tag) = notes_start_tag.as_deref() {
        create_args.extend_from_slice(&["--notes-start-tag", previous_tag]);
    }
    for path in &artifact_paths {
        create_args.push(path);
    }

    let output = std::process::Command::new("gh")
        .args(&create_args)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to invoke gh: {}", e),
                Some("gh release create".to_string()),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let fallback = fallback_gh_command(&tag);
        log_status!("release", "⚠ `gh release create` failed: {}", stderr.trim());
        log_status!("release", "Manual fallback: {}", fallback);
        return Ok(step_success(
            "github.release",
            "github.release",
            Some(serde_json::json!({
                "skipped": true,
                "reason": "gh-command-failed",
                "tag": tag,
                "owner": github.owner,
                "repo": github.repo,
                "stdout": stdout,
                "stderr": stderr,
                "fallback_command": fallback,
            })),
            Vec::new(),
        ));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    log_status!("release", "Created GitHub Release: {}", url);

    Ok(step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "url": url,
            "artifact_count": artifact_paths.len(),
            "generated_notes": true,
            "changelog_url": changelog_url,
            "notes_start_tag": notes_start_tag,
        })),
        Vec::new(),
    ))
}

fn github_generated_notes(
    github: &GitHubRepo,
    tag: &str,
    previous_tag: Option<&str>,
) -> Result<String> {
    let endpoint = format!(
        "repos/{}/{}/releases/generate-notes",
        github.owner, github.repo
    );
    let tag_field = format!("tag_name={}", tag);
    let mut args: Vec<&str> = vec!["api", &endpoint, "-f", &tag_field, "--jq", ".body"];
    let previous_field;
    if let Some(previous) = previous_tag {
        previous_field = format!("previous_tag_name={}", previous);
        args.extend_from_slice(&["-f", &previous_field]);
    }

    let output = std::process::Command::new("gh")
        .args(&args)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to invoke gh: {}", e),
                Some("gh api releases/generate-notes".to_string()),
            )
        })?;

    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "gh api releases/generate-notes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn github_changelog_url(component: &Component, github: &GitHubRepo, tag: &str) -> Option<String> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let local_path = std::path::Path::new(&component.local_path);
    let relative = changelog_path
        .strip_prefix(local_path)
        .unwrap_or(&changelog_path)
        .to_string_lossy()
        .replace('\\', "/");
    Some(format!(
        "https://github.com/{}/{}/blob/{}/{}",
        github.owner, github.repo, tag, relative
    ))
}

pub(super) fn replace_full_changelog_footer(notes: &str, changelog_url: &str) -> String {
    let replacement = format!("**Full Changelog**: {}", changelog_url);
    let mut lines: Vec<&str> = notes.lines().collect();

    if let Some(index) = lines.iter().rposition(|line| {
        line.trim_start()
            .starts_with("**Full Changelog**: https://github.com/")
    }) {
        lines[index] = &replacement;
        return lines.join("\n");
    }

    if notes.trim().is_empty() {
        return replacement;
    }

    format!("{}\n\n{}", notes.trim_end(), replacement)
}

fn github_generated_notes_start_tag(component: &Component, tag: &str) -> Result<Option<String>> {
    let monorepo = crate::core::git::MonorepoContext::detect(&component.local_path, &component.id);
    let (git_root, tag_prefix) = match monorepo.as_ref() {
        Some(ctx) => (ctx.git_root.as_str(), Some(ctx.tag_prefix.as_str())),
        None => (component.local_path.as_str(), None),
    };
    crate::core::git::get_previous_tag_before_with_prefix(git_root, tag, tag_prefix)
}

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
    format!("gh release create {} --title {} --generate-notes", tag, tag)
}
