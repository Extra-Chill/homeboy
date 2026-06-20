//! The tag-availability preflight and `git.tag` release steps, plus the shared
//! tag-state inspection used by both.
//!
//! Split out of `executor.rs` to keep tag creation/guarding logic together.

use crate::core::component::Component;
use crate::core::error::{Error, Result};

use super::super::types::{ReleaseState, ReleaseStepResult};
use super::version_targets::collect_head_version_mismatches;
use super::{github_release, step_failed, step_success};

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
