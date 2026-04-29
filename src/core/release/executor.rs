//! Release step implementations.
//!
//! Each step is a free function that takes the component, the mutable
//! [`ReleaseState`] threaded through the release, and whatever step-specific
//! inputs it needs, then returns a [`ReleaseStepResult`]. The caller
//! ([`super::pipeline::execute`]) runs them in order and handles skip-on-failure
//! logic for subsequent steps.
//!
//! This used to be a trait-object-dispatched `PipelineStepExecutor` driving a
//! generic DAG (`engine::pipeline`). In practice every release runs the same
//! linear sequence with a sequential `Mutex<ReleaseContext>` shared between
//! steps, so the DAG scaffolding bought nothing but indirection. The logic
//! inside each `run_*` function is unchanged; only the plumbing is different.

use crate::component::Component;
use crate::engine::local_files::FileSystem;
use crate::engine::validation;
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};
use crate::{changelog, version};

use super::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult, ReleaseStepStatus};
use super::utils::{extract_latest_notes, parse_release_artifacts};

/// Build a successful step result with optional data and hints.
pub(crate) fn step_success(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    hints: Vec<crate::error::Hint>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Success,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints,
        data,
        error: None,
    }
}

/// Build a failed step result carrying error text and optional data.
fn step_failed(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    error: Option<String>,
    hints: Vec<crate::error::Hint>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Failed,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints,
        data,
        error,
    }
}

/// Build a skipped step result carrying an explanatory warning.
fn step_skipped(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    warning: impl Into<String>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Skipped,
        missing: Vec::new(),
        warnings: vec![warning.into()],
        hints: Vec::new(),
        data,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Core steps
// ---------------------------------------------------------------------------

/// Bump the version file(s) on disk and, if any changelog entries were
/// auto-generated from commits, finalize them into the new version section.
///
/// Populates [`ReleaseState::version`], [`tag`][ReleaseState::tag] (default
/// `v{version}`), and [`notes`][ReleaseState::notes] from the just-written
/// changelog section.
pub(crate) fn run_version(
    component: &Component,
    state: &mut ReleaseState,
    bump_type: &str,
    changelog_entries: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<ReleaseStepResult> {
    let result = version::bump_component_version(component, bump_type, changelog_entries)?;
    let data = serde_json::to_value(&result)
        .map_err(|e| Error::internal_json(e.to_string(), Some("version output".to_string())))?;

    state.version = Some(result.new_version.clone());
    state.tag = Some(format!("v{}", result.new_version));
    state.notes = Some(load_release_notes(component)?);

    Ok(step_success("version", "version", Some(data), Vec::new()))
}

/// Commit any staged release artifacts (changelog/version files). Amends the
/// HEAD commit when the last commit is already a release commit and the
/// branch is ahead of origin — matches the original amend heuristic.
pub(crate) fn run_git_commit(
    component: &Component,
    component_id: &str,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let status_output = crate::git::status_at(Some(component_id), Some(&component.local_path))?;
    let is_clean = status_output.stdout.trim().is_empty();

    if is_clean {
        let data = serde_json::json!({
            "skipped": true,
            "reason": "working tree is clean, nothing to commit"
        });
        return Ok(step_success(
            "git.commit",
            "git.commit",
            Some(data),
            Vec::new(),
        ));
    }

    let should_amend = should_amend_release_commit(&component.local_path)?;
    let message = state
        .version
        .as_ref()
        .map(|v| format!("release: v{}", v))
        .unwrap_or_else(|| "release: unknown".to_string());

    let options = crate::git::CommitOptions {
        staged_only: false,
        files: None,
        exclude: None,
        amend: should_amend,
    };

    let output = crate::git::commit_at(
        Some(component_id),
        Some(&message),
        options,
        Some(&component.local_path),
    )?;
    let mut data = serde_json::to_value(&output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git commit output".to_string())))?;

    if should_amend {
        data["amended"] = serde_json::json!(true);
    }

    if output.success {
        Ok(step_success(
            "git.commit",
            "git.commit",
            Some(data),
            Vec::new(),
        ))
    } else {
        Ok(step_failed(
            "git.commit",
            "git.commit",
            Some(data),
            None,
            Vec::new(),
        ))
    }
}

/// Create (or reuse) the release tag. Idempotent when the tag already points
/// to HEAD; errors when it exists but points elsewhere. Updates
/// [`ReleaseState::tag`] to the final tag name (may have been overridden by
/// the caller for monorepo components).
pub(crate) fn run_git_tag(
    component: &Component,
    component_id: &str,
    state: &mut ReleaseState,
    tag_name: &str,
) -> Result<ReleaseStepResult> {
    if crate::git::tag_exists_locally(&component.local_path, tag_name).unwrap_or(false) {
        let tag_commit = crate::git::get_tag_commit(&component.local_path, tag_name)?;
        let head_commit = crate::git::get_head_commit(&component.local_path)?;

        if tag_commit == head_commit {
            state.tag = Some(tag_name.to_string());
            return Ok(step_success(
                "git.tag",
                "git.tag",
                Some(serde_json::json!({
                    "action": "tag",
                    "component_id": component_id,
                    "tag": tag_name,
                    "skipped": true,
                    "reason": "tag already exists and points to HEAD"
                })),
                Vec::new(),
            ));
        }

        return Err(Error::validation_invalid_argument(
            "tag",
            format!("Tag '{}' exists but points to different commit", tag_name),
            Some(format!(
                "Tag points to {}, HEAD is {}",
                &tag_commit[..8.min(tag_commit.len())],
                &head_commit[..8.min(head_commit.len())]
            )),
            Some(vec![
                format!("Delete stale tag: git tag -d {}", tag_name),
                format!("Then retry: homeboy release {}", component_id),
            ]),
        ));
    }

    let message = format!("Release {}", tag_name);
    let output = crate::git::tag_at(
        Some(component_id),
        Some(tag_name),
        Some(&message),
        Some(&component.local_path),
    )?;
    let data = serde_json::to_value(&output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git tag output".to_string())))?;

    if !output.success {
        let mut hints = Vec::new();

        if output.stderr.contains("already exists") {
            let local_exists =
                crate::git::tag_exists_locally(&component.local_path, tag_name).unwrap_or(false);
            let remote_exists =
                crate::git::tag_exists_on_remote(&component.local_path, tag_name).unwrap_or(false);

            if local_exists && !remote_exists {
                hints.push(crate::error::Hint {
                    message: format!(
                        "Tag '{}' exists locally but not on remote. Push it with: git push origin {}",
                        tag_name, tag_name
                    ),
                });
            } else if local_exists && remote_exists {
                hints.push(crate::error::Hint {
                    message: format!(
                        "Tag '{}' already exists locally and on remote. Delete local tag first: git tag -d {}",
                        tag_name, tag_name
                    ),
                });
            }
        }

        return Ok(step_failed(
            "git.tag",
            "git.tag",
            Some(data),
            Some(output.stderr),
            hints,
        ));
    }

    state.tag = Some(tag_name.to_string());
    Ok(step_success("git.tag", "git.tag", Some(data), Vec::new()))
}

/// Push commits (and tags) to the remote.
pub(crate) fn run_git_push(component: &Component, component_id: &str) -> Result<ReleaseStepResult> {
    let output = crate::git::push_at(
        Some(component_id),
        crate::git::PushOptions {
            tags: true,
            force_with_lease: false,
        },
        Some(&component.local_path),
    )?;
    let data = serde_json::to_value(output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git push output".to_string())))?;
    Ok(step_success("git.push", "git.push", Some(data), Vec::new()))
}

/// Invoke the `release.package` action on whichever extension provides it,
/// parse the emitted artifacts, and stash them in [`ReleaseState::artifacts`]
/// for downstream publish targets and for the GitHub Release step.
pub(crate) fn run_package(
    extensions: &[ExtensionManifest],
    state: &mut ReleaseState,
    component_id: &str,
    component_local_path: &str,
) -> Result<ReleaseStepResult> {
    let extension = extensions
        .iter()
        .find(|m| m.actions.iter().any(|a| a.id == "release.package"))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "release.package",
                "No extension provides release.package action",
                None,
                Some(vec![
                    "Add a extension with release.package action to the component".to_string(),
                    "For Rust projects, add: \"extensions\": { \"rust\": {} }".to_string(),
                ]),
            )
        })?;

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let response =
        extension::execute_action(&extension.id, "release.package", None, None, Some(&payload))?;

    store_artifacts_from_output(state, &response)?;

    let data = serde_json::json!({
        "extension": extension.id,
        "action": "release.package",
        "response": response,
    });

    Ok(step_success("package", "package", Some(data), Vec::new()))
}

/// Invoke the `release.publish` action on the named extension.
pub(crate) fn run_publish(
    extensions: &[ExtensionManifest],
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
    target: &str,
) -> Result<ReleaseStepResult> {
    let extension = extensions.iter().find(|m| m.id == target).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release.publish",
            format!("No extension '{}' found for publish target", target),
            None,
            Some(vec![format!(
                "Add extension to component config: \"extensions\": {{ \"{}\": {{}} }}",
                target
            )]),
        )
    })?;

    let action_id = "release.publish";
    let has_action = extension.actions.iter().any(|a| a.id == action_id);
    if !has_action {
        return Err(Error::validation_invalid_argument(
            "release.publish",
            format!(
                "Extension '{}' does not provide action '{}'",
                target, action_id
            ),
            None,
            None,
        ));
    }

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let response = extension::execute_action(&extension.id, action_id, None, None, Some(&payload))?;
    let extension_data = serde_json::to_value(&response).map_err(|e| {
        Error::internal_json(e.to_string(), Some("extension action output".to_string()))
    })?;

    let step_id = format!("publish.{}", target);
    let data = serde_json::json!({
        "target": target,
        "extension": extension.id,
        "action": action_id,
        "response": extension_data,
    });

    Ok(publish_step_result(
        &step_id,
        target,
        &extension.id,
        Some(data),
        &response,
    ))
}

fn publish_step_result(
    step_id: &str,
    target: &str,
    extension_id: &str,
    data: Option<serde_json::Value>,
    response: &serde_json::Value,
) -> ReleaseStepResult {
    if response
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
    {
        return step_success(step_id, step_id, data, Vec::new());
    }

    if is_missing_cargo_token_publish_response(target, extension_id, response) {
        return step_skipped(
            step_id,
            step_id,
            data,
            "Skipped Rust publish: no Cargo registry token is configured",
        );
    }

    step_failed(
        step_id,
        step_id,
        data,
        Some(publish_failure_message(target, response)),
        Vec::new(),
    )
}

fn is_missing_cargo_token_publish_response(
    target: &str,
    extension_id: &str,
    response: &serde_json::Value,
) -> bool {
    if target != "rust" && extension_id != "rust" {
        return false;
    }

    let output = publish_response_output(response).to_ascii_lowercase();
    output.contains("no token found")
        && (output.contains("cargo login") || output.contains("cargo_registry_token"))
}

fn publish_response_output(response: &serde_json::Value) -> String {
    let stdout = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stderr = response
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!("{}\n{}", stdout, stderr)
}

fn publish_failure_message(target: &str, response: &serde_json::Value) -> String {
    let exit_code = response
        .get("exit_code")
        .or_else(|| response.get("exitCode"))
        .and_then(|v| v.as_i64());
    let output = publish_response_output(response);
    let detail = output.trim();

    match (exit_code, detail.is_empty()) {
        (Some(code), false) => format!("Publish to {} failed (exit {}): {}", target, code, detail),
        (Some(code), true) => format!("Publish to {} failed (exit {})", target, code),
        (None, false) => format!("Publish to {} failed: {}", target, detail),
        (None, true) => format!("Publish to {} failed", target),
    }
}

/// Delete the packaging staging dir (`target/distrib`). Skipped when the
/// caller chose `--deploy` so the deploy step can still find the artifact.
pub(crate) fn run_cleanup(component: &Component) -> Result<ReleaseStepResult> {
    let distrib_path = format!("{}/target/distrib", component.local_path);

    let mut removed = false;
    if std::path::Path::new(&distrib_path).exists() {
        std::fs::remove_dir_all(&distrib_path).map_err(|e| {
            Error::internal_io(
                format!("Failed to clean up {}: {}", distrib_path, e),
                Some(distrib_path.clone()),
            )
        })?;
        removed = true;
    }

    let data = serde_json::json!({
        "action": "cleanup",
        "path": distrib_path,
        "removed": removed,
    });

    Ok(step_success("cleanup", "cleanup", Some(data), Vec::new()))
}

/// Run the component's `post_release` hook commands. Failures are non-fatal —
/// the release has already been published, so the most we can do is log the
/// warning and surface it in the step result for the overall summary to pick
/// up.
pub(crate) fn run_post_release(
    component: &Component,
    commands: &[String],
) -> Result<ReleaseStepResult> {
    let hook_result = crate::engine::hooks::run_commands(
        commands,
        &component.local_path,
        crate::engine::hooks::events::POST_RELEASE,
        crate::engine::hooks::HookFailureMode::NonFatal,
    )?;

    if !hook_result.all_succeeded {
        for failed in hook_result.commands.iter().filter(|c| !c.success) {
            let error_text = if failed.stderr.trim().is_empty() {
                &failed.stdout
            } else {
                &failed.stderr
            };
            log_status!(
                "warning",
                "Post-release hook failed: '{}': {}",
                failed.command,
                error_text.trim()
            );
        }
    }

    let commands_summary: Vec<serde_json::Value> = hook_result
        .commands
        .iter()
        .map(|c| {
            serde_json::json!({
                "command": c.command,
                "success": c.success,
                "exit_code": c.exit_code,
            })
        })
        .collect();

    let data = serde_json::json!({
        "action": "post_release",
        "commands": commands_summary,
        "all_succeeded": hook_result.all_succeeded,
    });

    Ok(step_success(
        "post_release",
        "post_release",
        Some(data),
        Vec::new(),
    ))
}

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
    let notes = state.notes.clone().unwrap_or_default();

    let local_path = &component.local_path;

    let remote_url = component
        .remote_url
        .clone()
        .or_else(|| {
            crate::deploy::release_download::detect_remote_url(std::path::Path::new(local_path))
        })
        .ok_or_else(|| {
            Error::internal_unexpected(
                "github.release: no remote_url configured and git remote get-url origin failed"
                    .to_string(),
            )
        })?;

    let github =
        crate::deploy::release_download::parse_github_url(&remote_url).ok_or_else(|| {
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
        return Ok(step_success(
            "github.release",
            "github.release",
            Some(serde_json::json!({
                "skipped": true,
                "reason": "gh-not-available",
                "tag": tag,
                "owner": github.owner,
                "repo": github.repo,
                "fallback_command": fallback,
            })),
            Vec::new(),
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
        return Ok(step_success(
            "github.release",
            "github.release",
            Some(serde_json::json!({
                "skipped": true,
                "reason": "gh-not-authenticated",
                "tag": tag,
                "owner": github.owner,
                "repo": github.repo,
                "fallback_command": fallback,
            })),
            Vec::new(),
        ));
    }

    let repo_flag = format!("{}/{}", github.owner, github.repo);
    if gh_release_exists(&tag, &repo_flag) {
        log_status!(
            "release",
            "GitHub Release {} already exists for {} — skipping (idempotent)",
            tag,
            repo_flag
        );
        return Ok(step_success(
            "github.release",
            "github.release",
            Some(serde_json::json!({
                "skipped": true,
                "reason": "release-already-exists",
                "tag": tag,
                "owner": github.owner,
                "repo": github.repo,
            })),
            Vec::new(),
        ));
    }

    let notes_body = if notes.trim().is_empty() {
        format!("Release {}", tag)
    } else {
        notes
    };

    let tmp_dir = crate::engine::temp::runtime_temp_dir("github-release")?;
    let notes_path = tmp_dir.join(format!("notes-{}.md", sanitize_tag_for_filename(&tag)));
    std::fs::write(&notes_path, &notes_body).map_err(|e| {
        Error::internal_io(
            format!("Failed to write release notes file: {}", e),
            Some(notes_path.display().to_string()),
        )
    })?;

    log_status!(
        "release",
        "Creating GitHub Release {} on {}...",
        tag,
        repo_flag
    );

    let output = std::process::Command::new("gh")
        .args([
            "release",
            "create",
            &tag,
            "--title",
            &tag,
            "--notes-file",
            notes_path.to_str().ok_or_else(|| {
                Error::internal_unexpected(
                    "github.release: notes-file path is not valid UTF-8".to_string(),
                )
            })?,
            "-R",
            &repo_flag,
        ])
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
        })),
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn load_release_notes(component: &Component) -> Result<String> {
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::engine::local_files::local().read(&changelog_path)?;
    validation::require(
        extract_latest_notes(&changelog_content),
        "changelog",
        "No finalized changelog entries found for release notes",
    )
}

fn should_amend_release_commit(local_path: &str) -> Result<bool> {
    let log_output = crate::git::execute_git_for_release(local_path, &["log", "-1", "--format=%s"])
        .map_err(|e| Error::internal_io(e.to_string(), Some("git log".to_string())))?;
    if !log_output.status.success() {
        return Ok(false);
    }
    let last_message = String::from_utf8_lossy(&log_output.stdout)
        .trim()
        .to_string();

    if !last_message.starts_with("release: v") {
        return Ok(false);
    }

    let status_output = crate::git::execute_git_for_release(local_path, &["status", "-sb"])
        .map_err(|e| Error::internal_io(e.to_string(), Some("git status".to_string())))?;
    if !status_output.status.success() {
        return Ok(false);
    }
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    Ok(status_str.contains("[ahead"))
}

/// Payload passed to extension actions — mirrors the pre-refactor shape so
/// extensions don't need to change.
pub(crate) fn build_release_payload(
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
    extra_config: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> serde_json::Value {
    let version = state.version.clone().unwrap_or_default();
    let tag = state.tag.clone().unwrap_or_else(|| format!("v{}", version));
    let notes = state.notes.clone().unwrap_or_default();

    let mut payload = serde_json::json!({
        "release": {
            "version": version,
            "tag": tag,
            "notes": notes,
            "component_id": component_id,
            "local_path": component_local_path,
            "artifacts": state.artifacts,
        }
    });

    if let Some(config) = extra_config {
        if !config.is_empty() {
            payload["config"] = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
        }
    }

    payload
}

fn store_artifacts_from_output(
    state: &mut ReleaseState,
    response: &serde_json::Value,
) -> Result<()> {
    let stdout = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let stderr = response
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let exit_code = response
        .get("exit_code")
        .or_else(|| response.get("exitCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);

    if stdout.trim().is_empty() {
        let detail = if !stderr.is_empty() {
            format!(
                "Package command failed (exit {}): {}",
                exit_code,
                stderr.trim()
            )
        } else if exit_code != 0 {
            format!(
                "Package command failed (exit {}) with no output. \
                 Check that the required packaging tool is installed (e.g., cargo-dist)",
                exit_code
            )
        } else {
            "Package command produced no artifact output. \
             The packaging tool may not be installed or configured correctly."
                .to_string()
        };
        return Err(Error::internal_unexpected(detail));
    }

    let raw_artifacts: serde_json::Value = serde_json::from_str(stdout).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some(format!("Failed to parse package artifacts: {}", stdout)),
        )
    })?;
    let artifacts: Vec<ReleaseArtifact> = parse_release_artifacts(&raw_artifacts)?;
    state.artifacts.extend(artifacts);
    Ok(())
}

// ---------------------------------------------------------------------------
// `gh` CLI probes
// ---------------------------------------------------------------------------

fn gh_is_available() -> bool {
    crate::git::gh_probe_succeeds(&["--version"])
}

fn gh_is_authenticated() -> bool {
    crate::git::gh_probe_succeeds(&["auth", "status", "--hostname", "github.com"])
}

fn gh_release_exists(tag: &str, repo_flag: &str) -> bool {
    crate::git::gh_probe_succeeds(&["release", "view", tag, "-R", repo_flag])
}

fn fallback_gh_command(tag: &str) -> String {
    format!(
        "gh release create {} --title {} --notes-file <path-to-release-notes>",
        tag, tag
    )
}

fn sanitize_tag_for_filename(tag: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{fallback_gh_command, publish_step_result, sanitize_tag_for_filename};
    use crate::release::ReleaseStepStatus;

    #[test]
    fn sanitize_tag_for_filename_preserves_safe_chars() {
        assert_eq!(sanitize_tag_for_filename("v1.2.3"), "v1.2.3");
        assert_eq!(
            sanitize_tag_for_filename("data-machine-v0.70.2"),
            "data-machine-v0.70.2"
        );
    }

    #[test]
    fn sanitize_tag_for_filename_strips_unsafe_chars() {
        assert_eq!(sanitize_tag_for_filename("v1.2.3 rc1"), "v1.2.3-rc1");
        assert_eq!(sanitize_tag_for_filename("feat/foo@1"), "feat-foo-1");
    }

    #[test]
    fn fallback_gh_command_includes_tag_twice() {
        let cmd = fallback_gh_command("v1.2.3");
        assert!(cmd.contains("gh release create v1.2.3"));
        assert!(cmd.contains("--title v1.2.3"));
        assert!(cmd.contains("--notes-file"));
    }

    #[test]
    fn publish_step_skips_rust_when_cargo_token_is_missing() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 101,
            "stdout": "",
            "stderr": "error: no token found, please run cargo login\nor use environment variable CARGO_REGISTRY_TOKEN",
        });
        let data = serde_json::json!({ "response": response.clone() });

        let result = publish_step_result("publish.rust", "rust", "rust", Some(data), &response);

        assert_eq!(result.status, ReleaseStepStatus::Skipped);
        assert!(result.error.is_none());
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("no Cargo registry token"));
    }

    #[test]
    fn publish_step_fails_rust_when_error_is_not_missing_token() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 101,
            "stdout": "",
            "stderr": "error: failed to upload package: 500 server error",
        });

        let result = publish_step_result("publish.rust", "rust", "rust", None, &response);

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(result.error.unwrap().contains("500 server error"));
    }

    #[test]
    fn publish_step_fails_non_rust_missing_token_text() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stderr": "error: no token found, please run cargo login",
        });

        let result = publish_step_result("publish.npm", "npm", "npm", None, &response);

        assert_eq!(result.status, ReleaseStepStatus::Failed);
    }
}
