//! Release step implementations.
//!
//! Each step is a free function that takes the component, the mutable
//! [`ReleaseState`] threaded through the release, and whatever step-specific
//! inputs it needs, then returns a [`ReleaseStepResult`]. The caller
//! ([`super::pipeline::run`], via the release plan dispatcher) runs them in
//! order and handles skip-on-failure logic for subsequent steps.
//!
//! This used to be a trait-object-dispatched `PipelineStepExecutor` driving a
//! generic DAG (`engine::pipeline`). In practice every release runs the same
//! linear sequence with a sequential `Mutex<ReleaseContext>` shared between
//! steps, so the DAG scaffolding bought nothing but indirection. The logic
//! inside each `run_*` function is unchanged; only the plumbing is different.

use crate::core::component::Component;
use crate::core::engine::local_files::FileSystem;
use crate::core::engine::validation;
use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionManifest};
use crate::core::release::{changelog as release_changelog, version};

use super::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult, ReleaseStepStatus};
use super::utils::{extract_latest_notes, parse_release_artifacts};

pub(crate) mod artifacts;
pub(crate) mod changelog;
pub(crate) mod github_release;
pub(crate) mod prepare;
mod version_targets;

use version_targets::{collect_head_version_mismatches, collect_version_target_mismatches};

/// Build a successful step result with optional data and hints.
pub(crate) fn step_success(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    hints: Vec<crate::core::error::Hint>,
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
    hints: Vec<crate::core::error::Hint>,
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
///
/// After the bump, every version target is re-read from disk and compared to
/// the new version. If any target wasn't updated, the step is marked Failed
/// so downstream steps (commit, tag, push) bail out before producing the
/// orphan-tag pattern from issue #2234 — a tag pushed onto an unbumped
/// commit. Without this invariant a silent no-op bump (e.g. a regression in
/// the changelog finalization path that swallows the version write) leaves
/// `state.version` advanced in memory while the working tree stays clean,
/// `git.commit` skips, and `git.tag` lands on the wrong commit.
pub(crate) fn run_version(
    component: &Component,
    state: &mut ReleaseState,
    bump_type: &str,
) -> Result<ReleaseStepResult> {
    let result = version::bump_component_version_with_changelog(
        component,
        bump_type,
        None,
        state.changelog_validation.as_ref(),
    )?;
    let data = serde_json::to_value(&result)
        .map_err(|e| Error::internal_json(e.to_string(), Some("version output".to_string())))?;

    if let Some(mismatches) = collect_version_target_mismatches(component, &result.new_version) {
        let error_msg = format!(
            "Version bump verification failed: {} target(s) on disk are not at {} after bump_component_version: {}. \
             Refusing to continue — tagging now would create an orphan tag (no release: commit, no version-file bump). See issue #2234.",
            mismatches.len(),
            result.new_version,
            mismatches
                .iter()
                .map(|m| format!("{} = {}", m.file, m.found.as_deref().unwrap_or("<unreadable>")))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let mut failure_data = data;
        failure_data["mismatches"] = serde_json::to_value(&mismatches).unwrap_or_default();
        failure_data["new_version"] = serde_json::Value::String(result.new_version.clone());
        return Ok(step_failed(
            "version",
            "version",
            Some(failure_data),
            Some(error_msg),
            vec![crate::core::error::Hint {
                message: "If a previous run partially bumped this component, run `homeboy release <component> --recover` to finish it cleanly.".to_string(),
            }],
        ));
    }

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
    let status_output =
        crate::core::git::status_at(Some(component_id), Some(&component.local_path))?;
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

    let options = crate::core::git::CommitOptions {
        staged_only: false,
        files: None,
        exclude: None,
        amend: should_amend,
    };

    let output = crate::core::git::commit_at(
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
///
/// Final invariant before tagging: HEAD's tree must contain every version
/// target at `state.version`. If HEAD wasn't updated to the new version
/// (orphan-tag pattern from issue #2234), the step fails *before* creating
/// the tag instead of pushing a tag onto the wrong commit.
pub(crate) fn run_git_tag(
    component: &Component,
    component_id: &str,
    state: &mut ReleaseState,
    tag_name: &str,
) -> Result<ReleaseStepResult> {
    if let Some(version) = state.version.as_deref() {
        if let Some(mismatches) = collect_head_version_mismatches(component, version) {
            let error_msg = format!(
                "Tag invariant failed: HEAD does not show version {} for {} target(s): {}. \
                 Refusing to create tag {} on the wrong commit (would produce the orphan-tag pattern from issue #2234).",
                version,
                mismatches.len(),
                mismatches
                    .iter()
                    .map(|m| format!("{} = {}", m.file, m.found.as_deref().unwrap_or("<unreadable>")))
                    .collect::<Vec<_>>()
                    .join("; "),
                tag_name,
            );
            return Ok(step_failed(
                "git.tag",
                "git.tag",
                Some(serde_json::json!({
                    "tag": tag_name,
                    "expected_version": version,
                    "mismatches": mismatches,
                })),
                Some(error_msg),
                vec![crate::core::error::Hint {
                    message: format!(
                        "Inspect the failed bump: `git status` then `git log -1`. To finish a partial release, run `homeboy release {} --recover`.",
                        component_id
                    ),
                }],
            ));
        }
    }

    let head_commit = crate::core::git::get_head_commit(&component.local_path)?;
    let local_tag_commit =
        if crate::core::git::tag_exists_locally(&component.local_path, tag_name).unwrap_or(false) {
            Some(crate::core::git::get_tag_commit(
                &component.local_path,
                tag_name,
            )?)
        } else {
            None
        };
    let remote_tag_commit = crate::core::git::remote_tag_commit(&component.local_path, tag_name)?;

    if local_tag_commit.as_deref() == Some(head_commit.as_str())
        || remote_tag_commit.as_deref() == Some(head_commit.as_str())
    {
        state.tag = Some(tag_name.to_string());
        return Ok(step_success(
            "git.tag",
            "git.tag",
            Some(serde_json::json!({
                "action": "tag",
                "component_id": component_id,
                "tag": tag_name,
                "skipped": true,
                "reason": "tag already exists and points to HEAD",
                "head": head_commit,
                "local_tag": local_tag_commit,
                "remote_tag": remote_tag_commit,
            })),
            Vec::new(),
        ));
    }

    if local_tag_commit.is_some() || remote_tag_commit.is_some() {
        let short_sha = |commit: &str| commit[..8.min(commit.len())].to_string();
        let tag_state = |commit: Option<&str>| {
            commit
                .map(&short_sha)
                .unwrap_or_else(|| "absent".to_string())
        };
        let github_release = component
            .remote_url
            .clone()
            .or_else(|| {
                crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                    &component.local_path,
                ))
            })
            .and_then(|remote_url| {
                crate::core::deploy::release_download::parse_github_url(&remote_url)
            })
            .and_then(|github| {
                let github_env = github_release::github_command_env(&github);
                if !github_release::gh_is_available(&github_env)
                    || !github_release::gh_is_authenticated(&github.host, &github_env)
                {
                    return None;
                }

                let repo_flag = format!("{}/{}", github.owner, github.repo);
                Some(github_release::gh_release_exists(
                    tag_name,
                    &repo_flag,
                    &github_env,
                ))
            });
        let github_release_label = match github_release {
            Some(true) => "exists".to_string(),
            Some(false) => "not found".to_string(),
            None => "not checked".to_string(),
        };
        let mut hints = vec![crate::core::error::Hint {
            message: format!(
                "If {} is a previous successful release, bump the component version and rerun `homeboy release {}` so Homeboy creates the next tag.",
                tag_name, component_id
            ),
        }];

        if github_release == Some(true) {
            hints.push(crate::core::error::Hint {
                message: format!(
                    "If {} is an abandoned pre-release tag with a GitHub Release, delete both deliberately: `gh release delete {} --cleanup-tag --yes`.",
                    tag_name, tag_name
                ),
            });
        } else if remote_tag_commit.is_some() {
            hints.push(crate::core::error::Hint {
                message: format!(
                    "If {} is an abandoned pre-release tag without a GitHub Release, delete the remote tag deliberately: `git push origin :refs/tags/{}`.",
                    tag_name, tag_name
                ),
            });
        }

        if local_tag_commit.is_some() {
            hints.push(crate::core::error::Hint {
                message: format!(
                    "After confirming the remote state, remove the stale local tag with `git tag -d {}`.",
                    tag_name
                ),
            });
        }

        hints.push(crate::core::error::Hint {
            message: format!(
                "Retry with `homeboy release {}` after cleanup.",
                component_id
            ),
        });

        return Ok(step_failed(
            "git.tag",
            "git.tag",
            Some(serde_json::json!({
                "action": "tag",
                "component_id": component_id,
                "tag": tag_name,
                "head": head_commit,
                "local_tag": local_tag_commit,
                "remote_tag": remote_tag_commit,
                "github_release": github_release,
            })),
            Some(format!(
                "Release tag {} already exists but does not point at HEAD ({}). Local tag: {}; origin tag: {}; GitHub Release: {}. Refusing to move or overwrite the tag.",
                tag_name,
                short_sha(&head_commit),
                tag_state(local_tag_commit.as_deref()),
                tag_state(remote_tag_commit.as_deref()),
                github_release_label,
            )),
            hints,
        ));
    }

    let message = format!("Release {}", tag_name);
    let output = crate::core::git::tag_at(
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
                crate::core::git::tag_exists_locally(&component.local_path, tag_name)
                    .unwrap_or(false);
            let remote_exists =
                crate::core::git::tag_exists_on_remote(&component.local_path, tag_name)
                    .unwrap_or(false);

            if local_exists && !remote_exists {
                hints.push(crate::core::error::Hint {
                    message: format!(
                        "Tag '{}' exists locally but not on remote. Push it with: git push origin {}",
                        tag_name, tag_name
                    ),
                });
            } else if local_exists && remote_exists {
                hints.push(crate::core::error::Hint {
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
    let branch = crate::core::git::current_branch(std::path::Path::new(&component.local_path))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "branch",
                "Release push requires a checked-out branch",
                Some(component.local_path.clone()),
                Some(vec![
                    "Check out the release branch before running `homeboy release`.".to_string(),
                ]),
            )
        })?;
    let output = crate::core::git::push_at(
        Some(component_id),
        crate::core::git::PushOptions {
            tags: true,
            force_with_lease: false,
            refspec: Some(format!("HEAD:refs/heads/{branch}")),
            ..Default::default()
        },
        Some(&component.local_path),
    )?;
    let data = serde_json::to_value(output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git push output".to_string())))?;

    let success = data
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !success {
        let error = data
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .filter(|stderr| !stderr.trim().is_empty())
            .or_else(|| data.get("stdout").and_then(serde_json::Value::as_str))
            .unwrap_or("git push failed")
            .trim()
            .to_string();

        return Ok(step_failed(
            "git.push",
            "git.push",
            Some(data),
            Some(error),
            Vec::new(),
        ));
    }

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
                    "Add an extension with a release.package action to the component".to_string(),
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
    component: &Component,
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

    let mut payload = build_release_payload(state, &component.id, &component.local_path, None);
    if let Some(github) = release_github_repo(component) {
        payload["release"]["github"] = serde_json::json!({
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
        });
    }
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

    if let Some(reason) = extension_skip_reason(response) {
        return step_skipped(
            step_id,
            step_id,
            data,
            format!(
                "Skipped publish to {} via {}: {}",
                target, extension_id, reason
            ),
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

fn extension_skip_reason(response: &serde_json::Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str())?;
    if !matches!(status, "skipped" | "missing_secret" | "auth_required") {
        return None;
    }

    response
        .get("reason")
        .or_else(|| response.get("message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let output = publish_response_output(response);
            let detail = output.trim();
            (!detail.is_empty()).then(|| detail.to_string())
        })
        .or_else(|| Some(status.replace('_', " ")))
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
    let hook_result = crate::core::engine::hooks::run_commands(
        commands,
        &component.local_path,
        crate::core::engine::hooks::events::POST_RELEASE,
        crate::core::engine::hooks::HookFailureMode::NonFatal,
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

    let github_env = github_release::github_command_env(&github);

    if !github_release::gh_is_available(&github_env) {
        let fallback = github_release::fallback_gh_command(&tag);
        log_status!(
            "release",
            "⚠ `gh` CLI not found on PATH — skipping GitHub Release creation"
        );
        log_status!("release", "Manual fallback: {}", fallback);
        return Ok(github_release::skipped_result(
            &tag,
            &github,
            "gh-not-available",
            Some(fallback),
        ));
    }

    if !github_release::gh_is_authenticated(&github.host, &github_env) {
        let fallback = github_release::fallback_gh_command(&tag);
        log_status!(
            "release",
            "⚠ `gh` is not authenticated — skipping GitHub Release creation"
        );
        log_status!(
            "release",
            "Authenticate with `gh auth login`, then manual fallback: {}",
            fallback
        );
        return Ok(github_release::skipped_result(
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
    if github_release::gh_release_exists(&tag, &repo_flag, &github_env) {
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
            return Ok(github_release::skipped_result(
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
            .envs(
                github_env
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
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
            return Ok(github_release::upload_failed_result(
                &tag,
                &github,
                stdout,
                stderr,
                artifact_paths.len(),
            ));
        }

        return Ok(github_release::upload_success_result(
            &tag,
            &github,
            artifact_paths.len(),
        ));
    }

    let notes_body = if notes.trim().is_empty() {
        format!("Release {}", tag)
    } else {
        notes
    };

    let tmp_dir = crate::core::engine::temp::runtime_temp_dir("github-release")?;
    let notes_path = tmp_dir.join(format!(
        "notes-{}.md",
        github_release::sanitize_tag_for_filename(&tag)
    ));
    std::fs::write(&notes_path, &notes_body).map_err(|e| {
        Error::internal_io(
            format!("Failed to write release notes file: {}", e),
            Some(notes_path.display().to_string()),
        )
    })?;

    let notes_path_str = notes_path.to_str().ok_or_else(|| {
        Error::internal_unexpected("github.release: notes-file path is not valid UTF-8".to_string())
    })?;

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
        "--notes-file",
        notes_path_str,
        "-R",
        &repo_flag,
    ];
    for path in &artifact_paths {
        create_args.push(path);
    }

    let output = std::process::Command::new("gh")
        .args(&create_args)
        .envs(
            github_env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
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
        let fallback = github_release::fallback_gh_command(&tag);
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
        })),
        Vec::new(),
    ))
}

fn release_github_repo(
    component: &Component,
) -> Option<crate::core::deploy::release_download::GitHubRepo> {
    component
        .remote_url
        .clone()
        .or_else(|| {
            crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                &component.local_path,
            ))
        })
        .and_then(|remote_url| crate::core::deploy::release_download::parse_github_url(&remote_url))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn load_release_notes(component: &Component) -> Result<String> {
    let changelog_path = release_changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::core::engine::local_files::local().read(&changelog_path)?;
    validation::require(
        extract_latest_notes(&changelog_content),
        "changelog",
        "No finalized changelog entries found for release notes",
    )
}

fn should_amend_release_commit(local_path: &str) -> Result<bool> {
    let log_output =
        crate::core::git::execute_git_for_release(local_path, &["log", "-1", "--format=%s"])
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

    let status_output =
        crate::core::git::execute_git_for_release(local_path, &["status", "-sb"])
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
                 Check that the required packaging tool is installed and configured.",
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

#[cfg(test)]
mod tests {
    use super::{
        github_release, publish_step_result, release_github_repo, run_git_push,
        store_artifacts_from_output,
    };
    use crate::core::component::Component;
    use crate::core::release::ReleaseStepStatus;
    use std::process::Command;

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn sanitize_tag_for_filename_preserves_safe_chars() {
        assert_eq!(
            github_release::sanitize_tag_for_filename("v1.2.3"),
            "v1.2.3"
        );
        assert_eq!(
            github_release::sanitize_tag_for_filename("data-machine-v0.70.2"),
            "data-machine-v0.70.2"
        );
    }

    #[test]
    fn sanitize_tag_for_filename_strips_unsafe_chars() {
        assert_eq!(
            github_release::sanitize_tag_for_filename("v1.2.3 rc1"),
            "v1.2.3-rc1"
        );
        assert_eq!(
            github_release::sanitize_tag_for_filename("feat/foo@1"),
            "feat-foo-1"
        );
    }

    #[test]
    fn fallback_gh_command_includes_tag_twice() {
        let cmd = github_release::fallback_gh_command("v1.2.3");
        assert!(cmd.contains("gh release create v1.2.3"));
        assert!(cmd.contains("--title v1.2.3"));
        assert!(cmd.contains("--notes-file"));
    }

    #[test]
    fn release_github_repo_detects_ghe_remote() {
        let component = Component {
            id: "studio-web".to_string(),
            local_path: "/tmp/studio-web".to_string(),
            remote_url: Some("git@github.a8c.com:chubes4/studio-web.git".to_string()),
            ..Default::default()
        };

        let github = release_github_repo(&component).expect("github repo");

        assert_eq!(github.host, "github.a8c.com");
        assert_eq!(github.owner, "chubes4");
        assert_eq!(github.repo, "studio-web");
    }

    #[test]
    fn git_push_step_fails_when_git_push_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let init = Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .output()
            .expect("git init");
        assert!(init.status.success());

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result = run_git_push(&component, "fixture").expect("push step should return result");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(!result.error.unwrap().trim().is_empty());
        assert_eq!(
            result
                .data
                .and_then(|data| data.get("success").and_then(serde_json::Value::as_bool)),
            Some(false)
        );
    }

    #[test]
    fn test_run_git_push_without_upstream() {
        let local = tempfile::tempdir().expect("local tempdir");
        let remote = tempfile::tempdir().expect("remote tempdir");
        git(remote.path(), &["init", "--bare"]);
        git(local.path(), &["init"]);
        git(local.path(), &["checkout", "-b", "main"]);
        git(local.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            local.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        git(
            local.path(),
            &[
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("remote path"),
            ],
        );
        std::fs::write(local.path().join("release.txt"), "release").expect("write fixture");
        git(local.path(), &["add", "release.txt"]);
        git(local.path(), &["commit", "-m", "release: v1.0.0"]);
        git(
            local.path(),
            &["tag", "-a", "v1.0.0", "-m", "Release v1.0.0"],
        );

        let upstream = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .current_dir(local.path())
            .output()
            .expect("check upstream");
        assert!(
            !upstream.status.success(),
            "fixture should not have upstream"
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: local.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result = run_git_push(&component, "fixture").expect("push step should return result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        git(remote.path(), &["show-ref", "--verify", "refs/heads/main"]);
        git(remote.path(), &["show-ref", "--verify", "refs/tags/v1.0.0"]);
    }

    #[test]
    fn publish_step_skips_when_extension_reports_missing_secret() {
        let response = serde_json::json!({
            "success": false,
            "status": "missing_secret",
            "reason": "registry token is not configured",
            "stdout": "",
            "stderr": "",
        });
        let data = serde_json::json!({ "response": response.clone() });

        let result = publish_step_result(
            "publish.registry",
            "registry",
            "registry",
            Some(data),
            &response,
        );

        assert_eq!(result.status, ReleaseStepStatus::Skipped);
        assert!(result.error.is_none());
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("registry token is not configured"));
    }

    #[test]
    fn publish_step_skips_when_extension_reports_auth_required() {
        let response = serde_json::json!({
            "success": false,
            "status": "auth_required",
            "message": "run the extension login command",
        });

        let result =
            publish_step_result("publish.registry", "registry", "registry", None, &response);

        assert_eq!(result.status, ReleaseStepStatus::Skipped);
        assert!(result.warnings[0].contains("run the extension login command"));
    }

    #[test]
    fn publish_step_fails_when_extension_error_has_no_skip_status() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stderr": "error: failed to upload package: 500 server error",
        });

        let result =
            publish_step_result("publish.registry", "registry", "registry", None, &response);

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(result.error.unwrap().contains("500 server error"));
    }

    #[test]
    fn package_error_message_is_extension_generic() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stdout": "",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("empty failing package output should fail");

        assert!(err.message.contains("required packaging tool"));
        assert!(!err.message.contains("example-package-manager"));
    }

    // ----- Orphan-tag regression coverage (issue #2234) -----

    use super::run_git_tag;
    use super::version_targets::{
        collect_head_version_mismatches, collect_version_target_mismatches,
    };
    use crate::core::component::VersionTarget;
    use crate::core::release::types::ReleaseState;

    fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        let output = std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn command");
        assert!(
            output.status.success(),
            "command {:?} failed: stdout={:?} stderr={:?}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }

    /// Fixture: a git repo with one committed plugin header file at version
    /// `committed_version`. The working tree is clean. Returns (temp, component).
    fn plugin_repo_at(committed_version: &str) -> (tempfile::TempDir, Component) {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_in(dir, &["git", "init", "-q"]);
        run_in(dir, &["git", "config", "user.email", "test@example.com"]);
        run_in(dir, &["git", "config", "user.name", "Test"]);
        run_in(dir, &["git", "config", "commit.gpgsign", "false"]);

        let plugin = format!(
            "<?php\n/*\nPlugin Name: Fixture\nVersion: {}\n*/\n",
            committed_version
        );
        std::fs::write(dir.join("plugin.php"), plugin).expect("write plugin");
        run_in(dir, &["git", "add", "."]);
        run_in(dir, &["git", "commit", "-q", "-m", "Initial commit"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"(?:Version|version)[:=]\s+([0-9]+\.[0-9]+\.[0-9]+)".to_string()),
            }]),
            ..Component::default()
        };
        (temp, component)
    }

    #[test]
    fn collect_version_target_mismatches_returns_none_when_disk_matches_expected() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        assert!(collect_version_target_mismatches(&component, "0.6.13").is_none());
    }

    #[test]
    fn collect_version_target_mismatches_flags_unbumped_target() {
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mismatches = collect_version_target_mismatches(&component, "0.6.13")
            .expect("expected mismatch on stale version file");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].file, "plugin.php");
        assert_eq!(mismatches[0].expected, "0.6.13");
        assert_eq!(mismatches[0].found.as_deref(), Some("0.6.12"));
    }

    #[test]
    fn collect_head_version_mismatches_flags_when_head_lacks_new_version() {
        // HEAD has 0.6.12 committed, but state.version is 0.6.13.
        // Working tree is also at 0.6.12 (no bump happened). HEAD check fires.
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mismatches = collect_head_version_mismatches(&component, "0.6.13")
            .expect("HEAD should not show 0.6.13");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].found.as_deref(), Some("0.6.12"));
    }

    #[test]
    fn collect_head_version_mismatches_returns_none_when_head_matches() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        assert!(collect_head_version_mismatches(&component, "0.6.13").is_none());
    }

    /// Fixture: a git repo whose toplevel contains a `subdir/` with the
    /// version target file. `component.local_path` points at `<toplevel>/subdir`,
    /// NOT the git toplevel. This mirrors the monorepo-extension layout
    /// (`homeboy-extensions/wordpress`, `homeboy-extensions/swift`, etc.) that
    /// triggers issue #2327.
    fn plugin_repo_at_subdir(
        committed_version: &str,
        subdir: &str,
    ) -> (tempfile::TempDir, Component) {
        let temp = tempfile::tempdir().expect("tempdir");
        let toplevel = temp.path();
        run_in(toplevel, &["git", "init", "-q"]);
        run_in(
            toplevel,
            &["git", "config", "user.email", "test@example.com"],
        );
        run_in(toplevel, &["git", "config", "user.name", "Test"]);
        run_in(toplevel, &["git", "config", "commit.gpgsign", "false"]);

        let sub = toplevel.join(subdir);
        std::fs::create_dir_all(&sub).expect("create subdir");

        let plugin = format!(
            "<?php\n/*\nPlugin Name: Fixture\nVersion: {}\n*/\n",
            committed_version
        );
        std::fs::write(sub.join("plugin.php"), plugin).expect("write plugin");
        run_in(toplevel, &["git", "add", "."]);
        run_in(toplevel, &["git", "commit", "-q", "-m", "Initial commit"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: sub.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"(?:Version|version)[:=]\s+([0-9]+\.[0-9]+\.[0-9]+)".to_string()),
            }]),
            ..Component::default()
        };
        (temp, component)
    }

    #[test]
    fn collect_head_version_mismatches_works_in_monorepo_subdir() {
        // Issue #2327: when the component's local_path is a subdir of the git
        // toplevel (monorepo extension layout), `git show HEAD:<path>` must
        // resolve `<path>` against the git toplevel, not against
        // component.local_path. Before the fix this returns `None` because
        // `git show HEAD:plugin.php` fails ("path subdir/plugin.php exists,
        // but not plugin.php"), which makes the HEAD invariant treat the
        // committed version as `<unreadable>` and either flag a spurious
        // mismatch or silently pass when comparing `None != Some(expected)`.
        let (_temp, component) = plugin_repo_at_subdir("0.6.13", "wordpress");

        // HEAD has 0.6.13. With a correct toplevel-relative path resolution,
        // `read_version_at_head` returns Some("0.6.13") and the mismatch
        // collector returns None.
        assert!(
            collect_head_version_mismatches(&component, "0.6.13").is_none(),
            "HEAD has the expected version; mismatch collector should return None \
             but the bug makes git show fail and mismatches are reported with \
             found = None"
        );

        // And when HEAD does NOT have the expected version, the collector
        // must still surface the real committed value (0.6.13) — not None.
        let mismatches = collect_head_version_mismatches(&component, "0.7.0")
            .expect("HEAD does not have 0.7.0; mismatch expected");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].file, "plugin.php");
        assert_eq!(mismatches[0].expected, "0.7.0");
        assert_eq!(
            mismatches[0].found.as_deref(),
            Some("0.6.13"),
            "found value must be the version actually committed at HEAD, \
             not None from a failed `git show HEAD:<wrong-path>`"
        );
    }

    #[test]
    fn git_tag_step_refuses_invalid_release_tags() {
        // The orphan-tag scenario from issue #2234: the in-memory state.version
        // says 0.6.13, but HEAD's plugin.php still reads 0.6.12. Without the
        // invariant check this would happily push a tag onto the wrong commit.
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("step should return a result, not propagate Err");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        let err = result.error.expect("expected failure error");
        assert!(
            err.contains("issue #2234"),
            "expected #2234 reference in error, got: {}",
            err
        );
        assert!(err.contains("v0.6.13"), "error should name the tag");
        assert!(
            !crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap_or(true),
            "tag must NOT have been created when invariant fails"
        );

        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        run_in(component_path, &["git", "tag", "v0.6.13"]);

        std::fs::write(
            component_path.join("plugin.php"),
            "<?php\n/*\nPlugin Name: Fixture\nVersion: 0.6.13\n*/\n",
        )
        .expect("write bumped plugin");
        run_in(component_path, &["git", "add", "plugin.php"]);
        run_in(
            component_path,
            &["git", "commit", "-q", "-m", "release: v0.6.13"],
        );

        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("existing tag on older commit should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        let err = result.error.expect("expected tag divergence error");
        assert!(
            err.contains("does not point at HEAD"),
            "expected tag divergence message, got: {}",
            err
        );
        assert!(err.contains("Local tag:"), "expected local tag state");
        assert!(err.contains("origin tag:"), "expected remote tag state");
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("local_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected local tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("previous successful release")),
            "expected next-version recovery guidance"
        );

        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        let remote = tempfile::tempdir().expect("remote tempdir");
        run_in(remote.path(), &["git", "init", "--bare", "-q"]);
        run_in(
            component_path,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("remote path"),
            ],
        );
        run_in(component_path, &["git", "tag", "v0.6.13"]);
        run_in(component_path, &["git", "push", "origin", "v0.6.13"]);
        run_in(component_path, &["git", "tag", "-d", "v0.6.13"]);

        std::fs::write(
            component_path.join("plugin.php"),
            "<?php\n/*\nPlugin Name: Fixture\nVersion: 0.6.13\n*/\n",
        )
        .expect("write bumped plugin");
        run_in(component_path, &["git", "add", "plugin.php"]);
        run_in(
            component_path,
            &["git", "commit", "-q", "-m", "release: v0.6.13"],
        );

        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("remote-only divergent tag should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(
            !crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap(),
            "remote-only divergent tag must not be recreated locally"
        );
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("remote_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected remote tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git push origin :refs/tags/v0.6.13")),
            "expected deliberate remote tag cleanup guidance"
        );
    }

    #[test]
    fn git_tag_step_creates_tag_when_head_matches_state_version() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("step should succeed when HEAD shows the bumped version");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(
            crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap_or(false),
            "tag should have been created on HEAD"
        );
    }
}
