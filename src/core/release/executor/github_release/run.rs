//! The `github.release` step entry point that drives the full release lifecycle.

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::super::step_success;
use super::gh_cli::{
    gh_command, gh_is_authenticated, gh_is_available, gh_release_exists,
    github_release_artifact_paths,
};
use super::notes::{
    build_github_release_body, github_changelog_url, github_release_notes_start_tag,
    persist_release_body,
};
use super::repair::{gh_auth_failure_message, github_release_repair_commands, log_repair_commands};
use super::results::{
    create_failed_result, not_created_result, skipped_result, upload_failed_result,
    upload_success_result,
};

/// Create a GitHub Release for the just-pushed tag.
///
/// The step result must faithfully represent whether a GitHub Release object
/// now exists, because downstream `publish.<target>` / upload steps assume the
/// release is present (see issue #3541). The rules are:
///
/// - Release object created (or already exists) → `Success`.
/// - Release object NOT created and not recoverable here (no `gh` binary, not
///   authenticated, `gh release create` failed) → `Failed`, carrying the exact
///   recovery commands so the operator can resume from the pushed tag + built
///   artifacts without making a second tag.
/// - Generated release notes failed → we retry the create with fallback notes
///   (the changelog section, or a minimal body) and verify the release exists.
///   Only if that fallback create also fails do we mark the step `Failed`.
///
/// `github.release` is a release-pipeline show-stopper, so a `Failed` result
/// halts the plan before publish/upload runs against a non-existent release.
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
                    "Use a GitHub or GitHub Enterprise remote for automatic GitHub Releases"
                        .to_string(),
                    "Use --no-github-release to skip this step".to_string(),
                ]),
            )
        })?;

    // Collect artifact paths from state. Populated by release.package
    // (or any other extension action that emits artifact metadata into
    // ReleaseState::artifacts). Passing these to `gh release create` or
    // `gh release upload --clobber` attaches them to the Release in a
    // single API call — keeping the github.release step responsible for
    // the full Release lifecycle (entry + assets) instead of requiring a
    // separate publish.<target> step.
    let artifact_paths = github_release_artifact_paths(state);
    let has_artifacts = !artifact_paths.is_empty();
    // Single repair-command builder for every failure path. The persisted
    // exact-body file only exists after `persist_release_body` runs below, so
    // early paths (gh missing / unauthenticated / upload) pass `None` and
    // regenerate notes, while the create path passes the persisted path so the
    // repair `--notes-file` reproduces the body byte-for-byte (issue #3508).
    let repair_commands = |notes_start_tag: Option<&str>, persisted_notes: Option<&str>| {
        github_release_repair_commands(
            &tag,
            &github,
            &component.github,
            &artifact_paths,
            notes_start_tag,
            persisted_notes,
        )
    };

    if !gh_is_available() {
        let repair = repair_commands(None, None);
        log_status!(
            "release",
            "✗ `gh` CLI not found on PATH — GitHub Release was NOT created"
        );
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-available",
            "`gh` CLI not found on PATH; GitHub Release was not created.",
            repair,
        ));
    }

    if !gh_is_authenticated(&github, &component.github) {
        let repair = repair_commands(None, None);
        let auth_error = gh_auth_failure_message(&github, &repair);
        log_status!(
            "release",
            "✗ `gh` is not authenticated — GitHub Release was NOT created"
        );
        log_status!("release", "Authenticate with `gh auth login`, then run:");
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-authenticated",
            &auth_error,
            repair,
        ));
    }

    let repo_flag = format!("{}/{}", github.owner, github.repo);
    if gh_release_exists(&github, &component.github, &tag, &repo_flag) {
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

        let upload_output = gh_command(&github, &component.github, &upload_args)
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
            let repair = repair_commands(None, None);
            log_status!("release", "✗ `gh release upload` failed: {}", stderr.trim());
            log_repair_commands(&repair);
            return Ok(upload_failed_result(
                &tag,
                &github,
                stdout,
                stderr,
                artifact_paths.len(),
                repair,
            ));
        }

        return Ok(upload_success_result(&tag, &github, artifact_paths.len()));
    }

    let notes_start_tag = github_release_notes_start_tag(component, &tag);
    let changelog_url = github_changelog_url(component, &github, &tag);

    // Build the EXACT body Homeboy will post (issue #3508). This is the single
    // source of truth for the release body — generated notes + changelog footer,
    // or the changelog-section fallback + footer. Persisting it (below) lets the
    // repair commands reproduce the identical body via `--notes-file` instead of
    // re-deriving it from source and risking a divergent body.
    let body = build_github_release_body(
        component,
        &github,
        &tag,
        state,
        changelog_url.as_deref(),
        notes_start_tag.as_deref(),
    );
    let generated_notes_ok = body.generated_notes_ok;
    let release_notes = body.body.clone();

    // Persist the exact body so it is inspectable after the fact and so the
    // repair `--notes-file` reproduces it byte-for-byte. A failure to write the
    // artifact is non-fatal: fall back to commands that regenerate notes.
    let persisted_notes_path = persist_release_body(component, &tag, &release_notes);

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
    // Only pass --notes-start-tag when generated notes succeeded. With explicit
    // fallback `--notes`, re-triggering the note generation that just failed
    // would be pointless and could fail the create for the same reason.
    if generated_notes_ok {
        if let Some(previous_tag) = notes_start_tag.as_deref() {
            create_args.extend_from_slice(&["--notes-start-tag", previous_tag]);
        }
    }
    for path in &artifact_paths {
        create_args.push(path);
    }

    let output = gh_command(&github, &component.github, &create_args)
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
        let repair = repair_commands(notes_start_tag.as_deref(), persisted_notes_path.as_deref());
        // Distinguish the path that brought us here so operators (and tests)
        // can see whether the fallback-after-generated-notes-failure also
        // failed, versus a plain create failure with working notes.
        let reason = if generated_notes_ok {
            "gh-command-failed"
        } else {
            "generated-notes-failed"
        };
        log_status!("release", "✗ `gh release create` failed: {}", stderr.trim());
        log_repair_commands(&repair);
        return Ok(create_failed_result(
            &tag,
            &github,
            reason,
            stdout,
            stderr,
            repair,
            &body,
            persisted_notes_path.as_deref(),
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
            "generated_notes": generated_notes_ok,
            // The changelog URL embedded in the release body footer, read back
            // from the single body builder so step metadata and the posted body
            // can never disagree (issue #3508).
            "changelog_url": body.changelog_url,
            "notes_start_tag": notes_start_tag,
            // The exact GitHub Release body Homeboy posted, plus a persisted
            // copy on disk, so manual recovery reproduces the identical body
            // without reconstructing it from source (issue #3508).
            "release_body": release_notes,
            "release_body_source": body.source_label(),
            "release_body_file": persisted_notes_path,
        })),
        Vec::new(),
    ))
}
