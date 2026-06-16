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
mod github_release;
pub(crate) mod package_preflight;
pub(crate) mod prepare;
mod publish;
pub(crate) mod version_targets;

pub(crate) use github_release::run_github_release;
pub(crate) use publish::{publish_response_output, run_publish};
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
pub(crate) fn step_failed(
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
pub(crate) fn step_skipped(
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

pub(crate) fn run_tag_availability_preflight(
    component: &Component,
    component_id: &str,
    tag_name: &str,
) -> Result<ReleaseStepResult> {
    let tag_state = inspect_release_tag_state(component, tag_name)?;

    if tag_state.has_existing_release_surface() {
        return Ok(build_existing_tag_failure(
            "preflight.tag_availability",
            "preflight.tag_availability",
            component_id,
            tag_name,
            tag_state,
        ));
    }

    Ok(step_success(
        "preflight.tag_availability",
        "preflight.tag_availability",
        Some(serde_json::json!({
            "component_id": component_id,
            "tag": tag_name,
            "head": tag_state.head_commit,
            "local_tag": tag_state.local_tag_commit,
            "remote_tag": tag_state.remote_tag_commit,
            "github_release": tag_state.github_release,
        })),
        Vec::new(),
    ))
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

    let tag_state = inspect_release_tag_state(component, tag_name)?;
    let head_commit = tag_state.head_commit.clone();
    let local_tag_commit = tag_state.local_tag_commit.clone();
    let remote_tag_commit = tag_state.remote_tag_commit.clone();

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

    if tag_state.has_existing_release_surface() {
        return Ok(build_existing_tag_failure(
            "git.tag",
            "git.tag",
            component_id,
            tag_name,
            tag_state,
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

#[derive(Debug, Clone)]
struct ReleaseTagState {
    head_commit: String,
    local_tag_commit: Option<String>,
    remote_tag_commit: Option<String>,
    github_release: Option<bool>,
}

impl ReleaseTagState {
    fn has_existing_release_surface(&self) -> bool {
        self.local_tag_commit.is_some()
            || self.remote_tag_commit.is_some()
            || self.github_release == Some(true)
    }
}

fn inspect_release_tag_state(component: &Component, tag_name: &str) -> Result<ReleaseTagState> {
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
    let github_release = component
        .remote_url
        .clone()
        .or_else(|| {
            crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                &component.local_path,
            ))
        })
        .and_then(|remote_url| crate::core::deploy::release_download::parse_github_url(&remote_url))
        .and_then(|github| {
            if !github_release::gh_is_available()
                || !github_release::gh_is_authenticated(&github, &component.github)
            {
                return None;
            }

            let repo_flag = format!("{}/{}", github.owner, github.repo);
            Some(github_release::gh_release_exists(
                &github,
                &component.github,
                tag_name,
                &repo_flag,
            ))
        });

    Ok(ReleaseTagState {
        head_commit,
        local_tag_commit,
        remote_tag_commit,
        github_release,
    })
}

/// Best-effort check for whether a published GitHub Release exists for `tag_name`.
///
/// Returns `Some(true)`/`Some(false)` when GitHub is reachable and the
/// repository resolves, or `None` when it cannot be determined (no remote, `gh`
/// unavailable/unauthenticated, or a non-GitHub remote). Callers that must not
/// move a published release should treat `None` conservatively.
pub(crate) fn github_release_exists_for_tag(component: &Component, tag_name: &str) -> Option<bool> {
    component
        .remote_url
        .clone()
        .or_else(|| {
            crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                &component.local_path,
            ))
        })
        .and_then(|remote_url| crate::core::deploy::release_download::parse_github_url(&remote_url))
        .and_then(|github| {
            if !github_release::gh_is_available()
                || !github_release::gh_is_authenticated(&github, &component.github)
            {
                return None;
            }

            let repo_flag = format!("{}/{}", github.owner, github.repo);
            Some(github_release::gh_release_exists(
                &github,
                &component.github,
                tag_name,
                &repo_flag,
            ))
        })
}

fn build_existing_tag_failure(
    id: &str,
    step_type: &str,
    component_id: &str,
    tag_name: &str,
    tag_state: ReleaseTagState,
) -> ReleaseStepResult {
    let short_sha = |commit: &str| commit[..8.min(commit.len())].to_string();
    let tag_commit_label = |commit: Option<&str>| {
        commit
            .map(&short_sha)
            .unwrap_or_else(|| "absent".to_string())
    };
    let github_release_label = match tag_state.github_release {
        Some(true) => "exists".to_string(),
        Some(false) => "not found".to_string(),
        None => "not checked".to_string(),
    };
    let error = if step_type == "preflight.tag_availability" {
        format!(
            "Release tag {} already exists before release mutation. Local tag: {}; origin tag: {}; GitHub Release: {}. Refusing to mutate changelog/version files or create a duplicate release commit.",
            tag_name,
            tag_commit_label(tag_state.local_tag_commit.as_deref()),
            tag_commit_label(tag_state.remote_tag_commit.as_deref()),
            github_release_label,
        )
    } else {
        format!(
            "Release tag {} already exists but does not point at HEAD ({}). Local tag: {}; origin tag: {}; GitHub Release: {}. Refusing to move or overwrite the tag.",
            tag_name,
            short_sha(&tag_state.head_commit),
            tag_commit_label(tag_state.local_tag_commit.as_deref()),
            tag_commit_label(tag_state.remote_tag_commit.as_deref()),
            github_release_label,
        )
    };
    let mut hints = vec![crate::core::error::Hint {
        message: format!(
            "If {} is a previous successful release, bump the component version and rerun `homeboy release {}` so Homeboy creates the next tag.",
            tag_name, component_id
        ),
    }];

    if tag_state.github_release == Some(true) {
        hints.push(crate::core::error::Hint {
            message: format!(
                "If {} is an abandoned pre-release tag with a GitHub Release, delete both deliberately: `gh release delete {} --cleanup-tag --yes`.",
                tag_name, tag_name
            ),
        });
    } else if tag_state.remote_tag_commit.is_some() {
        hints.push(crate::core::error::Hint {
            message: format!(
                "If {} is an abandoned pre-release tag without a GitHub Release, delete the remote tag deliberately: `git push origin :refs/tags/{}`.",
                tag_name, tag_name
            ),
        });
    }

    if tag_state.local_tag_commit.is_some() {
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

    step_failed(
        id,
        step_type,
        Some(serde_json::json!({
            "action": "tag",
            "component_id": component_id,
            "tag": tag_name,
            "head": tag_state.head_commit,
            "local_tag": tag_state.local_tag_commit,
            "remote_tag": tag_state.remote_tag_commit,
            "github_release": tag_state.github_release,
        })),
        Some(error),
        hints,
    )
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
    let output = git_push_release_branch(component, component_id, &branch)?;
    let data = serde_json::to_value(&output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git push output".to_string())))?;

    if output.success {
        return Ok(step_success("git.push", "git.push", Some(data), Vec::new()));
    }

    // The branch push was rejected. When the remote branch advanced after the
    // release commit + tag were created (issue #3611), git rejects the branch
    // ref as non-fast-forward — typically leaving the tag pushed but the branch
    // behind. Attempt a clean, non-force recovery: fetch, rebase the release
    // commit onto the advanced remote head, and re-push the branch.
    if is_non_fast_forward_rejection(&output.stderr) {
        match recover_advanced_remote_push(component, component_id, &branch) {
            Ok(Some(recovered)) => {
                let recovered_data = serde_json::to_value(&recovered).map_err(|e| {
                    Error::internal_json(e.to_string(), Some("git push output".to_string()))
                })?;
                log_status!(
                    "release",
                    "Remote {} advanced during release — rebased the release commit onto the new head and re-pushed.",
                    branch
                );
                return Ok(step_success(
                    "git.push",
                    "git.push",
                    Some(serde_json::json!({
                        "success": true,
                        "recovered": "advanced-remote-rebased",
                        "branch": branch,
                        "push": recovered_data,
                    })),
                    Vec::new(),
                ));
            }
            Ok(None) => {
                // Recovery was not safe to perform automatically; fall through
                // to the failure path with explicit recovery guidance.
            }
            Err(recover_err) => {
                log_status!(
                    "release",
                    "⚠ Automatic recovery from advanced remote failed: {}",
                    recover_err
                );
            }
        }

        let error = push_error_message(&output);
        return Ok(step_failed(
            "git.push",
            "git.push",
            Some(data),
            Some(error),
            non_fast_forward_recovery_hints(component_id, &branch),
        ));
    }

    let error = push_error_message(&output);
    Ok(step_failed(
        "git.push",
        "git.push",
        Some(data),
        Some(error),
        Vec::new(),
    ))
}

/// Push the release branch (and tags) to `origin`.
fn git_push_release_branch(
    component: &Component,
    component_id: &str,
    branch: &str,
) -> Result<crate::core::git::GitOutput> {
    crate::core::git::push_at(
        Some(component_id),
        crate::core::git::PushOptions {
            tags: true,
            force_with_lease: false,
            refspec: Some(format!("HEAD:refs/heads/{branch}")),
            ..Default::default()
        },
        Some(&component.local_path),
    )
}

/// Recover from a non-fast-forward branch rejection caused by the remote
/// advancing after the release commit/tag were created (issue #3611).
///
/// Fetches `origin`, confirms the local branch is strictly ahead of a common
/// ancestor (so a rebase is the right reconciliation, not a force-push over
/// divergent history), rebases HEAD onto the advanced remote head, and re-pushes
/// the branch. The already-pushed tag is left untouched. Returns:
/// - `Ok(Some(push_output))` when the rebase + re-push succeeded,
/// - `Ok(None)` when automatic recovery is unsafe (e.g. rebase conflict, or the
///   remote branch is unexpectedly gone) — the caller emits manual guidance,
/// - `Err(_)` on an unexpected git failure.
fn recover_advanced_remote_push(
    component: &Component,
    component_id: &str,
    branch: &str,
) -> Result<Option<crate::core::git::GitOutput>> {
    let path = &component.local_path;
    crate::core::git::fetch_origin(path)?;

    let Some(remote_commit) = crate::core::git::remote_branch_commit(path, branch)? else {
        // The branch is not on the remote at all — non-fast-forward against a
        // missing branch is unexpected; don't guess, defer to manual recovery.
        return Ok(None);
    };
    let head_commit = crate::core::git::get_head_commit(path)?;

    // Already reconciled (e.g. a retry after a manual fix): nothing to do.
    if remote_commit == head_commit {
        return git_push_release_branch(component, component_id, branch).map(Some);
    }

    // Only rebase when the remote head is NOT already contained in HEAD — if it
    // were, the push would have fast-forwarded. Confirm the histories share an
    // ancestor before rebasing so we never replay onto unrelated history.
    if crate::core::git::is_ancestor(path, &remote_commit, &head_commit)? {
        // Remote head is an ancestor of HEAD; the rejection was spurious or
        // already resolved. Re-push directly.
        return git_push_release_branch(component, component_id, branch).map(Some);
    }

    log_status!(
        "release",
        "Rebasing release commit onto advanced remote {} ({})...",
        branch,
        &remote_commit[..remote_commit.len().min(8)]
    );
    let rebase = crate::core::git::rebase_at(
        Some(component_id),
        crate::core::git::RebaseOptions {
            onto: Some(remote_commit.clone()),
            ..Default::default()
        },
        Some(path),
    )?;
    if !rebase.success {
        // Conflicting rebase — abort to leave the tree clean and defer to the
        // operator. Recovery is not safe to automate here.
        let _ = crate::core::git::rebase_at(
            Some(component_id),
            crate::core::git::RebaseOptions {
                abort: true,
                ..Default::default()
            },
            Some(path),
        );
        return Ok(None);
    }

    git_push_release_branch(component, component_id, branch).map(Some)
}

/// True when git's stderr indicates a non-fast-forward / stale-remote branch
/// rejection — the signature of the advanced-remote race in issue #3611.
fn is_non_fast_forward_rejection(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("[rejected]")
        || lower.contains("non-fast-forward")
        || lower.contains("fetch first")
        || lower.contains("tip of your current branch is behind")
        || lower.contains("updates were rejected")
}

fn push_error_message(output: &crate::core::git::GitOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    "git push failed".to_string()
}

/// Hints emitted when the branch push was rejected as non-fast-forward and
/// automatic recovery did not complete. They give the operator a deterministic,
/// non-force recovery path (issue #3611).
fn non_fast_forward_recovery_hints(
    component_id: &str,
    branch: &str,
) -> Vec<crate::core::error::Hint> {
    vec![
        crate::core::error::Hint {
            message: format!(
                "Remote '{}' advanced after the release commit/tag were created. The tag may already be pushed; the branch was rejected as non-fast-forward.",
                branch
            ),
        },
        crate::core::error::Hint {
            message: format!(
                "Reconcile and finish the release without re-tagging or force-pushing: homeboy release {} --recover",
                component_id
            ),
        },
        crate::core::error::Hint {
            message: format!(
                "Or resolve manually: git fetch origin && git rebase origin/{branch} && git push origin HEAD:{branch}",
            ),
        },
    ]
}

/// Maximum number of attempts for a transient package-command failure.
///
/// npm install (and similar dependency resolvers) can fail intermittently due
/// to registry hiccups, lock contention, or output-pipe timing.  A warm-cache
/// retry usually succeeds, so we retry once before surfacing the error.
/// Issue #3238.
const PACKAGE_ACTION_MAX_ATTEMPTS: usize = 2;

/// Invoke the `release.package` action on every extension that provides it,
/// parse the emitted artifacts, and stash them in [`ReleaseState::artifacts`]
/// for downstream publish targets and for the GitHub Release step.
pub(crate) fn run_package(
    extensions: &[ExtensionManifest],
    state: &mut ReleaseState,
    component_id: &str,
    component_local_path: &str,
) -> Result<ReleaseStepResult> {
    let package_extensions: Vec<&ExtensionManifest> = extensions
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.package"))
        .collect();

    if package_extensions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.package",
            "No extension provides release.package action",
            None,
            Some(vec![
                "Add an extension with a release.package action to the component".to_string(),
            ]),
        ));
    }

    let mut responses = Vec::new();
    for extension in package_extensions {
        let payload = build_release_payload(state, component_id, component_local_path, None);
        let response = run_package_action_with_retry(&extension.id, &payload)
            .map_err(|err| package_provider_error(&extension.id, err))?;

        store_artifacts_from_output(state, &response)
            .map_err(|err| package_provider_error(&extension.id, err))?;
        responses.push(serde_json::json!({
            "extension": extension.id,
            "response": response,
        }));
    }

    let data = if responses.len() == 1 {
        let response = responses.pop().expect("single package response");
        serde_json::json!({
            "extension": response["extension"],
            "action": "release.package",
            "response": response["response"],
        })
    } else {
        serde_json::json!({
            "action": "release.package",
            "extensions": responses.iter().map(|response| response["extension"].clone()).collect::<Vec<_>>(),
            "responses": responses,
        })
    };

    Ok(step_success("package", "package", Some(data), Vec::new()))
}

/// Execute a `release.package` action with a bounded retry for transient
/// failures.
///
/// Returns the action response (which may carry `success: false` on the final
/// attempt) so the caller can surface the full captured stdout/stderr via
/// [`store_artifacts_from_output`].
fn run_package_action_with_retry(
    extension_id: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    for attempt in 1..=PACKAGE_ACTION_MAX_ATTEMPTS {
        match extension::execute_action(extension_id, "release.package", None, None, Some(payload))
        {
            Ok(response) => {
                let success = response
                    .get("success")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let exit_code = response
                    .get("exitCode")
                    .or_else(|| response.get("exit_code"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(-1);

                if success || exit_code == 0 {
                    return Ok(response);
                }

                // Transient failure — retry once before surfacing the error.
                if attempt < PACKAGE_ACTION_MAX_ATTEMPTS {
                    log_status!(
                        "package",
                        "Package command exited {} (attempt {}/{}); retrying…",
                        exit_code,
                        attempt,
                        PACKAGE_ACTION_MAX_ATTEMPTS
                    );
                    continue;
                }

                // Final attempt — return the response so the caller can
                // surface the full captured output in the error.
                return Ok(response);
            }
            Err(err) => {
                if attempt < PACKAGE_ACTION_MAX_ATTEMPTS {
                    log_status!(
                        "package",
                        "Package action error (attempt {}/{}); retrying…",
                        attempt,
                        PACKAGE_ACTION_MAX_ATTEMPTS
                    );
                    continue;
                }
                return Err(err);
            }
        }
    }

    // Unreachable when PACKAGE_ACTION_MAX_ATTEMPTS >= 1.
    Err(Error::internal_unexpected(
        "Package command did not produce a result",
    ))
}

/// Invoke an extension-declared release preflight action.
pub(crate) fn run_extension_release_preflight(
    step: &crate::core::plan::PlanStep,
    extensions: &[ExtensionManifest],
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
) -> ReleaseStepResult {
    let extension_id = step
        .inputs
        .get("extension")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let action_id = step
        .inputs
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let Some(extension) = extensions
        .iter()
        .find(|extension| extension.id == extension_id)
    else {
        return step_failed(
            &step.id,
            &step.kind,
            Some(serde_json::json!({
                "extension": extension_id,
                "action": action_id,
            })),
            Some(format!(
                "Release preflight references missing extension '{}'",
                extension_id
            )),
            Vec::new(),
        );
    };

    if !extension
        .actions
        .iter()
        .any(|action| action.id == action_id)
    {
        return step_failed(
            &step.id,
            &step.kind,
            Some(serde_json::json!({
                "extension": extension_id,
                "action": action_id,
            })),
            Some(format!(
                "Release preflight references missing action '{}' on extension '{}'",
                action_id, extension_id
            )),
            Vec::new(),
        );
    }

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let response =
        match extension::execute_action(extension_id, action_id, None, None, Some(&payload)) {
            Ok(response) => response,
            Err(err) => {
                return step_failed(&step.id, &step.kind, None, Some(err.message), err.hints)
            }
        };

    let data = Some(serde_json::json!({
        "extension": extension_id,
        "action": action_id,
        "response": response,
    }));

    if response.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        let reason = response
            .get("reason")
            .or_else(|| response.get("error"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("extension release preflight reported failure");
        return step_failed(
            &step.id,
            &step.kind,
            data,
            Some(reason.to_string()),
            Vec::new(),
        );
    }

    step_success(&step.id, &step.kind, data, Vec::new())
}

fn package_provider_error(extension_id: &str, err: Error) -> Error {
    let mut wrapped = Error::new(
        err.code,
        format!(
            "release.package failed for extension '{}': {}",
            extension_id, err.message
        ),
        serde_json::json!({
            "extension": extension_id,
            "action": "release.package",
            "source": err.details,
        }),
    );
    wrapped.hints = err.hints;
    wrapped.retryable = err.retryable;
    wrapped
}

/// Delete release-generated artifact directories. Skipped when the caller chose
/// `--deploy` so the deploy step can still find the artifact.
pub(crate) fn run_cleanup(
    component: &Component,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let cleanup_paths = release_cleanup_paths(&component.local_path, &state.artifacts);
    let mut removed_paths = Vec::new();

    for path in &cleanup_paths {
        if !path.exists() {
            continue;
        }
        std::fs::remove_dir_all(path).map_err(|e| {
            Error::internal_io(
                format!("Failed to clean up {}: {}", path.display(), e),
                Some(path.display().to_string()),
            )
        })?;
        removed_paths.push(path.display().to_string());
    }

    let distrib_path = std::path::Path::new(&component.local_path).join("target/distrib");
    let data = serde_json::json!({
        "action": "cleanup",
        "path": distrib_path.display().to_string(),
        "paths": cleanup_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "removed_paths": removed_paths,
        "removed": !removed_paths.is_empty(),
    });

    Ok(step_success("cleanup", "cleanup", Some(data), Vec::new()))
}

pub(crate) fn release_cleanup_paths(
    local_path: &str,
    artifacts: &[ReleaseArtifact],
) -> Vec<std::path::PathBuf> {
    let component_path = std::path::Path::new(local_path);
    let mut paths = vec![component_path.join("target/distrib")];

    let build_path = component_path.join("build");
    let has_build_artifact = artifacts.iter().any(|artifact| {
        let artifact_path = std::path::Path::new(&artifact.path);
        let artifact_path = if artifact_path.is_absolute() {
            artifact_path.to_path_buf()
        } else {
            component_path.join(artifact_path)
        };

        artifact_path.starts_with(&build_path)
    });

    if has_build_artifact {
        paths.push(build_path);
    }

    paths
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

    let success = response
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(exit_code == 0);

    // Surface the full captured output when the package command itself failed,
    // rather than trying to parse partial stdout as JSON (which swallowed
    // stderr behind a generic "Failed to parse" error).  Issue #3238: npm
    // install inside the build script can fail intermittently, and the real
    // npm error must be visible in the structured error payload.
    if !success {
        return Err(package_command_failure_error(exit_code, stdout, stderr));
    }

    if stdout.trim().is_empty() {
        return Err(Error::internal_unexpected(
            "Package command produced no artifact output. \
             The packaging tool may not be installed or configured correctly.",
        ));
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

/// Build an [`Error`] that surfaces *all* captured output from a failed
/// package command — stdout, stderr, and exit code.
///
/// npm and similar build tools write progress to stdout and errors to stderr.
/// Including both streams ensures the operator can diagnose the real failure
/// instead of seeing truncated output.  Issue #3238.
fn package_command_failure_error(exit_code: i64, stdout: &str, stderr: &str) -> Error {
    let stderr_trimmed = stderr.trim();
    let stdout_trimmed = stdout.trim();
    let has_stderr = !stderr_trimmed.is_empty();
    let has_stdout = !stdout_trimmed.is_empty();

    let mut detail = format!("Package command failed (exit {})", exit_code);

    if has_stderr {
        detail.push_str(": ");
        detail.push_str(stderr_trimmed);
    } else if has_stdout {
        detail.push_str(": ");
        detail.push_str(stdout_trimmed);
    } else {
        detail.push_str(". Check that the required packaging tool is installed and configured.");
    }

    // When both streams have content, append stdout as additional context.
    // npm install failures often write progress lines to stdout and the
    // actual error to stderr; the operator needs both to see what happened
    // before the crash.
    if has_stderr && has_stdout {
        detail.push_str("\n\n--- stdout ---\n");
        detail.push_str(stdout_trimmed);
    }

    Error::internal_unexpected(detail)
}

#[cfg(test)]
mod tests {
    use super::{
        github_release, is_non_fast_forward_rejection, run_cleanup, run_git_push, run_package,
        store_artifacts_from_output,
    };
    use crate::core::component::Component;
    use crate::core::deploy::release_download::GitHubRepo;
    use crate::core::extension::ExtensionManifest;
    use crate::core::release::types::ReleaseState;
    use crate::core::release::{ReleaseArtifact, ReleaseStepStatus};
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
    fn github_release_repair_commands_include_repo_asset_notes_and_enterprise_env() {
        let github = GitHubRepo {
            host: "github.a8c.com".to_string(),
            owner: "chubes4".to_string(),
            repo: "studio-web".to_string(),
        };
        let artifacts = vec!["build/studio-web.zip".to_string()];

        let repair = github_release::github_release_repair_commands_with_proxy(
            "v0.10.5",
            &github,
            &artifacts,
            Some("v0.10.4"),
            Some("socks5://127.0.0.1:8080"),
        );

        assert_eq!(repair.notes_file, "build/v0.10.5-release-notes.md");
        assert!(repair.notes_guidance.contains("--notes-file"));
        assert!(repair
            .generate_notes_command
            .contains("GH_HOST=github.a8c.com HTTPS_PROXY=socks5://127.0.0.1:8080 gh api repos/chubes4/studio-web/releases/generate-notes"));
        assert!(repair
            .generate_notes_command
            .contains("-f tag_name=v0.10.5"));
        assert!(repair
            .generate_notes_command
            .contains("-f previous_tag_name=v0.10.4"));
        assert!(repair
            .generate_notes_command
            .contains("> build/v0.10.5-release-notes.md"));
        assert!(repair.create_command.contains("gh release create v0.10.5"));
        assert!(repair
            .create_command
            .contains("--notes-file build/v0.10.5-release-notes.md"));
        assert!(repair.create_command.contains("build/studio-web.zip"));
        assert!(repair.create_command.contains("-R chubes4/studio-web"));
        assert_eq!(
            repair.view_command,
            "GH_HOST=github.a8c.com HTTPS_PROXY=socks5://127.0.0.1:8080 gh release view v0.10.5 -R chubes4/studio-web"
        );
        assert!(repair
            .env_hint
            .as_deref()
            .unwrap_or_default()
            .contains("GitHub Enterprise host detected"));
    }

    #[test]
    fn github_release_notes_link_full_changelog_to_changelog_file() {
        let notes = concat!(
            "## What's Changed\n",
            "* fix release notes by @chubes in https://github.com/Extra-Chill/homeboy/pull/1\n",
            "\n",
            "**Full Changelog**: https://github.com/Extra-Chill/homeboy/compare/v0.8.1...v0.9.0"
        );

        let rewritten = github_release::replace_full_changelog_footer(
            notes,
            "https://github.com/Extra-Chill/homeboy/blob/v0.9.0/CHANGELOG.md",
        );

        assert!(rewritten.contains(
            "**Full Changelog**: https://github.com/Extra-Chill/homeboy/blob/v0.9.0/CHANGELOG.md"
        ));
        assert!(!rewritten.contains("/compare/v0.8.1...v0.9.0"));
    }

    #[test]
    fn github_release_notes_append_changelog_link_when_footer_is_missing() {
        let rewritten = github_release::replace_full_changelog_footer(
            "## What's Changed\n* release note",
            "https://github.com/Extra-Chill/homeboy/blob/v0.9.0/docs/CHANGELOG.md",
        );

        assert_eq!(
            rewritten,
            concat!(
                "## What's Changed\n",
                "* release note\n\n",
                "**Full Changelog**: https://github.com/Extra-Chill/homeboy/blob/v0.9.0/docs/CHANGELOG.md"
            )
        );
    }

    #[test]
    fn cleanup_removes_build_dir_when_release_artifact_is_inside_build() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        let distrib_dir = temp.path().join("target/distrib");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::create_dir_all(&distrib_dir).expect("distrib dir");
        let artifact_path = build_dir.join("fixture.zip");
        std::fs::write(&artifact_path, "artifact").expect("artifact");

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let state = ReleaseState {
            artifacts: vec![ReleaseArtifact {
                path: artifact_path.display().to_string(),
                artifact_type: None,
                platform: None,
            }],
            ..ReleaseState::default()
        };

        let result = run_cleanup(&component, &state).expect("cleanup");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(!build_dir.exists());
        assert!(!distrib_dir.exists());
    }

    #[test]
    fn cleanup_leaves_build_dir_without_release_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::write(build_dir.join("bundle.js"), "source build").expect("build file");

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let state = ReleaseState::default();

        let result = run_cleanup(&component, &state).expect("cleanup");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(build_dir.exists());
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
    fn test_is_non_fast_forward_rejection() {
        // The exact shape of git's stderr from issue #3611's failed push.
        let stderr = " ! [rejected]        HEAD -> main (fetch first)\n\
            error: failed to push some refs to 'https://github.com/owner/repo.git'\n\
            hint: Updates were rejected because the remote contains work that you do not\n\
            hint: have locally.";
        assert!(is_non_fast_forward_rejection(stderr));
        assert!(is_non_fast_forward_rejection("hint: non-fast-forward"));
        assert!(is_non_fast_forward_rejection(
            "Updates were rejected because the tip of your current branch is behind"
        ));
        // Unrelated failures must not trigger the rebase-recovery path.
        assert!(!is_non_fast_forward_rejection(
            "fatal: Authentication failed"
        ));
        assert!(!is_non_fast_forward_rejection(""));
    }

    /// Issue #3611: when the remote branch advances after the release commit and
    /// tag are created, `run_git_push` must rebase the release commit onto the
    /// advanced remote head and re-push — without force-pushing or re-tagging.
    #[test]
    fn run_git_push_recovers_when_remote_advanced() {
        let remote = tempfile::tempdir().expect("remote tempdir");
        let other = tempfile::tempdir().expect("other clone tempdir");
        let local = tempfile::tempdir().expect("local tempdir");
        git(remote.path(), &["init", "--bare", "-b", "main"]);

        let setup_identity = |dir: &std::path::Path| {
            git(dir, &["config", "user.name", "Homeboy Test"]);
            git(dir, &["config", "user.email", "homeboy@example.test"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
        };

        // Seed the remote with an initial commit via the "other" clone.
        git(
            other.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        setup_identity(other.path());
        std::fs::write(other.path().join("base.txt"), "base").unwrap();
        git(other.path(), &["add", "."]);
        git(other.path(), &["commit", "-m", "base"]);
        git(other.path(), &["push", "origin", "main"]);

        // The release clone starts from that base.
        git(
            local.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        setup_identity(local.path());

        // The remote advances AFTER the release clone was made.
        std::fs::write(other.path().join("advance.txt"), "advance").unwrap();
        git(other.path(), &["add", "."]);
        git(other.path(), &["commit", "-m", "remote advance"]);
        git(other.path(), &["push", "origin", "main"]);

        // The release commit + tag are created locally (mirroring the release
        // pipeline state right before the racing push).
        std::fs::write(local.path().join("release.txt"), "release").unwrap();
        git(local.path(), &["add", "."]);
        git(local.path(), &["commit", "-m", "release: v1.0.0"]);
        git(
            local.path(),
            &["tag", "-a", "v1.0.0", "-m", "Release v1.0.0"],
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: local.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result = run_git_push(&component, "fixture").expect("push step returns a result");

        assert_eq!(
            result.status,
            ReleaseStepStatus::Success,
            "push should recover from the advanced remote: {:?}",
            result.error
        );
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|d| d.get("recovered").and_then(serde_json::Value::as_str)),
            Some("advanced-remote-rebased")
        );

        // The remote main now contains BOTH the remote advance and the release
        // commit (the release commit was rebased on top), and the tag is pushed.
        git(remote.path(), &["show-ref", "--verify", "refs/tags/v1.0.0"]);
        let log = Command::new("git")
            .args(["log", "--oneline", "origin/main"])
            .current_dir(local.path())
            .output()
            .expect("git log");
        // Refresh remote-tracking ref first.
        git(local.path(), &["fetch", "origin"]);
        let log = Command::new("git")
            .args(["log", "--format=%s", "origin/main"])
            .current_dir(local.path())
            .output()
            .expect("git log");
        let subjects = String::from_utf8_lossy(&log.stdout);
        assert!(
            subjects.contains("release: v1.0.0"),
            "remote main must contain the release commit, got: {}",
            subjects
        );
        assert!(
            subjects.contains("remote advance"),
            "remote main must retain the advance commit (no force-push), got: {}",
            subjects
        );
        let _ = log;
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

    #[test]
    fn package_failure_surfaces_both_stdout_and_stderr() {
        // Issue #3238: when the build command fails mid-stream, stdout
        // contains partial progress (e.g. "Installing npm dependencies...")
        // and stderr contains the real error.  Both must appear in the
        // structured error payload so the operator can diagnose the failure.
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stdout": "[BUILD] Installing npm dependencies...\n",
            "stderr": "npm error: ERESOLVE unable to resolve dependency tree\n",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("failing package output should fail");

        // stderr (the npm error) is the primary message
        assert!(
            err.message.contains("npm error: ERESOLVE"),
            "error should surface npm stderr, got: {}",
            err.message
        );
        // stdout (the build progress) is appended as additional context
        assert!(
            err.message.contains("Installing npm dependencies"),
            "error should also include stdout context, got: {}",
            err.message
        );
        assert!(
            err.message.contains("--- stdout ---"),
            "error should label the stdout section"
        );
        assert!(err.message.contains("exit 1"));
    }

    #[test]
    fn package_failure_with_stdout_only_still_surfaces_output() {
        // When the build writes everything to stdout and then dies, the
        // error should include that stdout rather than a generic JSON-parse
        // failure.
        let response = serde_json::json!({
            "success": false,
            "exitCode": 42,
            "stdout": "[BUILD] Installing npm dependencies...\nnpm error: ENOTFOUND registry\n",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("failing package with stdout-only should fail");

        assert!(
            err.message.contains("npm error: ENOTFOUND registry"),
            "error should surface stdout content, got: {}",
            err.message
        );
        assert!(err.message.contains("exit 42"));
    }

    #[test]
    fn package_failure_detects_nonzero_exit_without_success_field() {
        // Some extension responses omit the "success" field.  In that case
        // the exit code alone must trigger the failure path.
        let response = serde_json::json!({
            "exitCode": 1,
            "stdout": "[BUILD] Installing npm dependencies...\n",
            "stderr": "some build error\n",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("nonzero exit without success field should fail");

        assert!(err.message.contains("some build error"));
    }

    #[test]
    fn package_success_with_valid_json_still_works() {
        // Regression guard: the new success-first check must not break the
        // happy path.
        let response = serde_json::json!({
            "success": true,
            "exitCode": 0,
            "stdout": "[{\"path\":\"build/artifact.zip\",\"type\":\"archive\"}]",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        store_artifacts_from_output(&mut state, &response).expect("valid artifacts should parse");

        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.artifacts[0].path, "build/artifact.zip");
    }

    fn release_package_extension(id: &str, command: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "actions": [{
                "id": "release.package",
                "label": "Package release",
                "type": "command",
                "command": command,
            }]
        }))
        .expect("manifest fixture");
        manifest.id = id.to_string();
        manifest
    }

    fn non_package_extension(id: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "actions": [{
                "id": "release.prepare",
                "label": "Prepare release",
                "type": "command",
                "command": "true",
            }]
        }))
        .expect("manifest fixture");
        manifest.id = id.to_string();
        manifest
    }

    #[test]
    fn run_package_collects_artifacts_from_multiple_package_providers() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let package_a = release_package_extension(
                "package-a",
                "printf '[{\"path\":\"target/package-a.tgz\",\"type\":\"archive\"}]'",
            );
            let package_b = release_package_extension(
                "package-b",
                "printf '[{\"path\":\"target/package-b.zip\",\"type\":\"archive\"}]'",
            );
            crate::core::extension::save_manifest(&package_a).expect("save package A extension");
            crate::core::extension::save_manifest(&package_b).expect("save package B extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package_a, non_package_extension("docs"), package_b],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
            )
            .expect("package step");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 2);
            assert_eq!(state.artifacts[0].path, "target/package-a.tgz");
            assert_eq!(state.artifacts[1].path, "target/package-b.zip");
            let data = result.data.expect("package data");
            assert_eq!(
                data["extensions"],
                serde_json::json!(["package-a", "package-b"])
            );
        });
    }

    #[test]
    fn run_package_passes_component_id_from_release_payload_to_action_env() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir_in(std::env::temp_dir())
                .expect("component tempdir with mismatched basename");
            let package = release_package_extension(
                "wordpress",
                "printf '[{\"path\":\"build/%s.zip\",\"type\":\"wordpress\"}]' \"$HOMEBOY_COMPONENT_ID\"",
            );
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package],
                &mut state,
                "intelligence-horse-theme",
                &component.path().to_string_lossy(),
            )
            .expect("package step");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(
                state.artifacts[0].path,
                "build/intelligence-horse-theme.zip"
            );
        });
    }

    #[test]
    fn run_package_failure_names_the_failing_package_provider() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let package_a = release_package_extension(
                "package-a",
                "printf '[{\"path\":\"target/package-a.tgz\"}]'",
            );
            let package_b = release_package_extension(
                "package-b",
                "printf 'archive command failed' >&2; exit 7",
            );
            crate::core::extension::save_manifest(&package_a).expect("save package A extension");
            crate::core::extension::save_manifest(&package_b).expect("save package B extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let err = run_package(
                &[package_a, package_b],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
            )
            .expect_err("failing package provider should fail the step");

            assert!(err
                .message
                .contains("release.package failed for extension 'package-b'"));
            assert!(err.message.contains("archive command failed"));
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(state.artifacts[0].path, "target/package-a.tgz");
        });
    }

    #[test]
    fn run_package_retries_transient_failure_then_surfaces_full_output() {
        // Issue #3238: the package command (npm install inside build.sh) can
        // fail intermittently.  A bounded retry gives the warm-cache path a
        // chance; when both attempts fail, the error must surface the full
        // captured stdout AND stderr so the operator can diagnose it.
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            // Simulates a build.sh that prints progress to stdout, writes
            // the npm error to stderr, and exits non-zero on every attempt.
            let package = release_package_extension(
                "wordpress",
                "printf '[BUILD] Installing npm dependencies...\\n'; \
                 printf 'npm error: ERESOLVE\\n' >&2; exit 1",
            );
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let err = run_package(
                &[package],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
            )
            .expect_err("persistently failing package should fail after retry");

            // stderr (npm error) must be surfaced — this is the core bug.
            assert!(
                err.message.contains("npm error: ERESOLVE"),
                "error should surface npm stderr, got: {}",
                err.message
            );
            // stdout (build progress) should also be visible.
            assert!(
                err.message.contains("Installing npm dependencies"),
                "error should include build progress stdout, got: {}",
                err.message
            );
            // No artifacts stored from the failed build.
            assert!(state.artifacts.is_empty());
        });
    }

    #[test]
    fn run_package_retries_transient_failure_then_recovers() {
        // When the first attempt fails but the retry succeeds, the step
        // should complete normally with the artifacts from the successful
        // attempt.
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let marker = component.path().join("attempt-counter");
            // Script that fails on the first invocation but succeeds on the
            // second (warm-cache retry).
            let script = format!(
                "n=$(cat '{marker}' 2>/dev/null || echo 0); \
                 echo $((n + 1)) > '{marker}'; \
                 if [ \"$n\" = \"0\" ]; then \
                   printf 'npm error: transient registry failure\\n' >&2; exit 1; \
                 else \
                   printf '[{{\"path\":\"build/artifact.zip\",\"type\":\"archive\"}}]'; \
                 fi",
                marker = marker.display()
            );
            let package = release_package_extension("wordpress", &script);
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
            )
            .expect("retry after transient failure should succeed");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(state.artifacts[0].path, "build/artifact.zip");
        });
    }

    // ----- Orphan-tag regression coverage (issue #2234) -----

    use super::version_targets::{
        collect_head_version_mismatches, collect_version_target_mismatches,
    };
    use super::{run_git_tag, run_tag_availability_preflight};
    use crate::core::component::VersionTarget;

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
                artifact_path: None,
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
                artifact_path: None,
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
    fn tag_availability_preflight_refuses_existing_release_tags_before_mutation() {
        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        run_in(component_path, &["git", "tag", "v0.6.13"]);

        let result = run_tag_availability_preflight(&component, "fixture", "v0.6.13")
            .expect("preflight should return a result, not propagate Err");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(result.id, "preflight.tag_availability");
        let err = result.error.expect("expected tag availability error");
        assert!(
            err.contains("Release tag v0.6.13 already exists"),
            "expected existing tag message, got: {}",
            err
        );
        assert!(err.contains("Local tag:"), "expected local tag state");
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git tag -d v0.6.13")),
            "expected deliberate local tag cleanup guidance"
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

        let result = run_tag_availability_preflight(&component, "fixture", "v0.6.13")
            .expect("remote-only existing tag should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("remote_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected origin tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git push origin :refs/tags/v0.6.13")),
            "expected deliberate origin tag cleanup guidance"
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
