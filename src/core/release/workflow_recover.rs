//! Release recovery (`release --recover`): finish a partially-completed release
//! by reconciling the branch, retagging to HEAD when needed, and re-pushing —
//! plus the orphan-tag diagnostics and the recovery plan/step builders.
//!
//! Split out of `workflow.rs` to keep the main release-command flow focused.

use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::plan::PlanStep;

use super::context::load_component;
use super::types::{ReleaseCommandInput, ReleaseCommandResult, ReleaseOptions, ReleasePlan};
use super::workflow::{format_tag, release_execution_plan, short_sha};

pub(super) fn run_recover(input: &ReleaseCommandInput) -> Result<(ReleaseCommandResult, i32)> {
    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;

    // Configure git identity for recovery commits/tags
    if let Some(ref identity_str) = input.git_identity {
        let identity = git::parse_git_identity(Some(identity_str));
        git::configure_identity(&component.local_path, &identity)?;
    }

    let monorepo = git::MonorepoContext::detect(&component.local_path, &input.component_id);
    let version_info = crate::core::release::version::read_component_version(&component)?;
    let current_version = &version_info.version;
    let tag_name = format_tag(current_version, monorepo.as_ref());

    // Create the annotated release tag, surfacing `err_label` on failure. Shared
    // by the retag-to-HEAD and first-time create paths below, which issue the
    // identical `git tag` and differ only in the error wording.
    let create_tag = |err_label: &str| -> Result<()> {
        let tag_result = git::tag(
            Some(&input.component_id),
            Some(&tag_name),
            Some(&format!("Release {}", tag_name)),
        )?;
        if !tag_result.success {
            return Err(Error::git_command_failed(format!(
                "{}: {}",
                err_label, tag_result.stderr
            )));
        }
        Ok(())
    };

    // Push commits and tags to origin, surfacing `err_label` on failure. Shared
    // by the retag and first-time push paths below, which issue the identical
    // tags-included push and differ only in the error wording.
    let push_tags = |err_label: &str| -> Result<()> {
        let push_result = git::push(
            Some(&input.component_id),
            git::PushOptions {
                tags: true,
                ..Default::default()
            },
        )?;
        if !push_result.success {
            return Err(Error::git_command_failed(format!(
                "{}: {}",
                err_label, push_result.stderr
            )));
        }
        Ok(())
    };

    // Surface the orphan-tag pattern from issue #2234. When the latest release
    // tag points at a commit whose subject is *not* `release: vX.Y.Z`, the
    // previous release was botched (tag without bump). Recover should warn
    // loudly so the operator can decide whether to delete the orphan tag, hand
    // back-fill a release: commit, or run `--recover` to commit the version
    // files at the tagged commit.
    if let Some(latest_tag) = latest_release_tag(&component.local_path, monorepo.as_ref()) {
        if let Some(diagnostic) = diagnose_orphan_tag(&component.local_path, &latest_tag) {
            log_status!("recover", "{}", diagnostic);
        }
    }

    let tag_exists_local =
        git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false);
    let tag_exists_remote =
        git::tag_exists_on_remote(&component.local_path, &tag_name).unwrap_or(false);
    let head_commit = git::get_head_commit(&component.local_path)?;
    let local_tag_commit = if tag_exists_local {
        Some(git::get_tag_commit(&component.local_path, &tag_name)?)
    } else {
        None
    };
    let remote_tag_commit = git::remote_tag_commit(&component.local_path, &tag_name)?;

    let tag_is_stale = local_tag_commit
        .as_deref()
        .is_some_and(|commit| commit != head_commit)
        || remote_tag_commit
            .as_deref()
            .is_some_and(|commit| commit != head_commit);

    if tag_is_stale && input.retag {
        // Guarded retag: only move the tag forward to HEAD when it is safe.
        //   1. Every existing tag commit (local + remote) is a strict ancestor
        //      of HEAD — never relocate onto divergent/unrelated history.
        //   2. HEAD satisfies all version targets at the current version —
        //      preserves the orphan-tag invariant (#2234): the tag must land on
        //      a commit whose tree actually shows this version.
        //   3. No GitHub Release exists for the tag — moving a published
        //      release is destructive to consumers and must be done explicitly.
        for candidate in [local_tag_commit.as_deref(), remote_tag_commit.as_deref()]
            .into_iter()
            .flatten()
        {
            let is_ancestor = git::is_ancestor(&component.local_path, candidate, &head_commit)?;
            if !is_ancestor {
                return Err(Error::validation_invalid_argument(
                    "retag",
                    format!(
                        "Refusing to retag '{}': existing tag commit {} is not an ancestor of HEAD {}",
                        tag_name,
                        short_sha(candidate),
                        short_sha(&head_commit)
                    ),
                    None,
                    Some(vec![
                        "The tag points at divergent history. Resolve manually before retagging.".to_string(),
                    ]),
                ));
            }
        }

        if let Some(mismatches) =
            crate::core::release::executor::version_targets::collect_head_version_mismatches(
                &component,
                current_version,
            )
        {
            let detail = mismatches
                .iter()
                .map(|m| {
                    format!(
                        "{} = {}",
                        m.file,
                        m.found.as_deref().unwrap_or("<unreadable>")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::validation_invalid_argument(
                "retag",
                format!(
                    "Refusing to retag '{}': HEAD does not show version {} for {} target(s): {}",
                    tag_name,
                    current_version,
                    mismatches.len(),
                    detail
                ),
                None,
                Some(vec![
                    "Bump the version targets at HEAD first, or run a normal release.".to_string(),
                ]),
            ));
        }

        if crate::core::release::executor::github_release_exists_for_tag(&component, &tag_name)
            == Some(true)
        {
            return Err(Error::validation_invalid_argument(
                "retag",
                format!(
                    "Refusing to retag '{}': a GitHub Release already exists for this tag",
                    tag_name
                ),
                None,
                Some(vec![
                    format!(
                        "Moving a published release is destructive. Delete it deliberately if intended: gh release delete {}",
                        tag_name
                    ),
                ]),
            ));
        }

        // Safe to move: delete the stale tag (local + remote) and re-create at HEAD.
        log_status!(
            "recover",
            "Retagging {} from {} to HEAD {}...",
            tag_name,
            local_tag_commit
                .as_deref()
                .or(remote_tag_commit.as_deref())
                .map(short_sha)
                .unwrap_or("<unknown>"),
            short_sha(&head_commit)
        );

        if tag_exists_local {
            git::delete_local_tag(&component.local_path, &tag_name)?;
        }
        if tag_exists_remote {
            git::delete_remote_tag(&component.local_path, &tag_name)?;
        }

        create_tag("Failed to re-create tag at HEAD")?;

        push_tags(&format!("Failed to push retagged {}", tag_name))?;

        let actions = vec![format!("retagged {} to HEAD", tag_name)];
        log_status!(
            "recover",
            "Recovery complete for v{}: {}",
            current_version,
            actions.join(", ")
        );
        return Ok((
            ReleaseCommandResult {
                component_id: input.component_id.clone(),
                status: "recovered".to_string(),
                phase: release_execution_plan(input).phase,
                bump_type: "recover".to_string(),
                dry_run: false,
                releasable_commits: 0,
                new_version: None,
                tag: Some(tag_name.clone()),
                skipped_reason: None,
                plan: Some(recovery_release_plan(
                    &input.component_id,
                    current_version,
                    &tag_name,
                    false,
                    true,
                    true,
                    &actions,
                )),
                run: None,
                deployment: None,
                release_summary: actions.clone(),
            },
            0,
        ));
    }

    if tag_is_stale {
        return Err(Error::validation_invalid_argument(
            "tag",
            format!("Tag '{}' exists but does not point to HEAD", tag_name),
            Some(format!(
                "local tag points to {}, origin tag points to {}, HEAD is {}",
                local_tag_commit
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or("<missing>"),
                remote_tag_commit
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or("<missing>"),
                short_sha(&head_commit)
            )),
            Some(vec![
                format!(
                    "Inspect the existing tag before recovery: git show --no-patch --decorate {}",
                    tag_name
                ),
                format!(
                    "If the existing tag is valid, create a new releasable commit and run: homeboy release {}",
                    input.component_id
                ),
                format!(
                    "If the tag is an abandoned partial release, delete the GitHub release/tag explicitly, then run: homeboy release {} --recover",
                    input.component_id
                ),
                format!(
                    "If config-only commits landed after tagging (tag is behind HEAD, version unchanged, no GitHub Release), move the tag to HEAD: homeboy release {} --recover --retag",
                    input.component_id
                ),
            ]),
        ));
    }

    let uncommitted = git::get_uncommitted_changes(&component.local_path)?;

    let mut actions = Vec::new();

    if uncommitted.has_changes {
        log_status!("recover", "Committing uncommitted changes...");
        let msg = format!("release: v{}", current_version);
        let commit_result = git::commit(
            Some(&input.component_id),
            Some(msg.as_str()),
            git::CommitOptions {
                staged_only: false,
                files: None,
                exclude: None,
                amend: false,
            },
        )?;
        if !commit_result.success {
            return Err(Error::git_command_failed(format!(
                "Failed to commit: {}",
                commit_result.stderr
            )));
        }
        actions.push("committed version files".to_string());
    }

    if !tag_exists_local {
        log_status!("recover", "Creating tag {}...", tag_name);
        create_tag("Failed to create tag")?;
        actions.push(format!("created tag {}", tag_name));
    }

    if !tag_exists_remote {
        log_status!("recover", "Pushing to remote...");
        push_tags("Failed to push")?;
        actions.push("pushed commits and tags".to_string());
    }

    // Issue #3611: the partial state where the TAG was pushed but the branch
    // push was rejected because the remote advanced. Here the tag points at
    // HEAD (not stale) and there are no uncommitted changes, so the checks
    // above are all satisfied — yet the release commit is still missing from
    // the remote branch. Detect that the local release commit is not on the
    // remote branch and reconcile it (rebase onto the advanced remote, push)
    // without re-tagging or force-pushing.
    if let Some(reconcile_action) = reconcile_release_branch(&component, &input.component_id)? {
        actions.push(reconcile_action);
    }

    if actions.is_empty() {
        log_status!(
            "recover",
            "Release v{} appears complete — nothing to recover.",
            current_version
        );
    } else {
        log_status!(
            "recover",
            "Recovery complete for v{}: {}",
            current_version,
            actions.join(", ")
        );
    }

    Ok((
        ReleaseCommandResult {
            component_id: input.component_id.clone(),
            status: if actions.is_empty() {
                "already_recovered".to_string()
            } else {
                "recovered".to_string()
            },
            phase: release_execution_plan(input).phase,
            bump_type: "recover".to_string(),
            dry_run: false,
            releasable_commits: 0,
            new_version: None,
            tag: Some(tag_name.clone()),
            skipped_reason: None,
            plan: Some(recovery_release_plan(
                &input.component_id,
                current_version,
                &tag_name,
                uncommitted.has_changes,
                !tag_exists_local,
                !tag_exists_remote,
                &actions,
            )),
            run: None,
            deployment: None,
            release_summary: if actions.is_empty() {
                vec![format!("Release already exists: {}", tag_name)]
            } else {
                actions.to_vec()
            },
        },
        0,
    ))
}

/// Reconcile the release branch with an advanced remote during `--recover`
/// (issue #3611).
///
/// Handles the partial state where the release tag was pushed but the branch
/// push was rejected because `origin/<branch>` advanced. When the local release
/// commit (HEAD) is not contained in the remote branch, this fetches, rebases
/// HEAD onto the advanced remote head (only when histories share an ancestor —
/// never a force-push over divergent history), and re-pushes the branch.
///
/// Returns `Ok(Some(description))` when it reconciled the branch, `Ok(None)`
/// when nothing needed doing (or no remote branch / detached HEAD), and `Err`
/// when reconciliation was attempted but failed (e.g. rebase conflict) so the
/// operator gets a clear, non-guessing failure.
pub(super) fn reconcile_release_branch(
    component: &crate::core::component::Component,
    component_id: &str,
) -> Result<Option<String>> {
    let path = &component.local_path;
    let Some(branch) = git::current_branch(std::path::Path::new(path)) else {
        // Detached HEAD — no branch to reconcile.
        return Ok(None);
    };

    git::fetch_origin(path)?;
    let Some(remote_commit) = git::remote_branch_commit(path, &branch)? else {
        // Branch not on remote yet; the tag-push block above already pushes the
        // branch when it pushes tags, so there is nothing to reconcile here.
        return Ok(None);
    };
    let head_commit = git::get_head_commit(path)?;

    // Push HEAD to the branch (tags included), surfacing `err_label` on failure.
    // Shared by the fast-forward and post-rebase paths below, which push the
    // identical refspec and differ only in the error wording.
    let push_branch = |err_label: &str| -> Result<()> {
        let push = git::push_at(
            Some(component_id),
            git::PushOptions {
                tags: true,
                refspec: Some(format!("HEAD:refs/heads/{branch}")),
                ..Default::default()
            },
            Some(path),
        )?;
        if !push.success {
            return Err(Error::git_command_failed(format!(
                "Failed to push {} branch {}: {}",
                err_label, branch, push.stderr
            )));
        }
        Ok(())
    };

    // The release commit is already on the remote branch — nothing to do.
    if git::is_ancestor(path, &head_commit, &remote_commit)? {
        return Ok(None);
    }

    // Remote head already contained in HEAD (a plain non-pushed branch): push.
    if git::is_ancestor(path, &remote_commit, &head_commit)? {
        log_status!(
            "recover",
            "Pushing release commit to remote {} (remote did not advance)...",
            branch
        );
        push_branch("release")?;
        return Ok(Some(format!("pushed release commit to {}", branch)));
    }

    // Histories diverged: the remote advanced after the release commit. Rebase
    // the release commit onto the advanced remote head, then push. Never force.
    log_status!(
        "recover",
        "Remote {} advanced — rebasing release commit onto the new head and re-pushing...",
        branch
    );
    let rebase = git::rebase_at(
        Some(component_id),
        git::RebaseOptions {
            onto: Some(remote_commit.clone()),
            ..Default::default()
        },
        Some(path),
    )?;
    if !rebase.success {
        let _ = git::rebase_at(
            Some(component_id),
            git::RebaseOptions {
                abort: true,
                ..Default::default()
            },
            Some(path),
        );
        return Err(Error::validation_invalid_argument(
            "recover",
            format!(
                "Rebasing the release commit onto the advanced remote {} hit a conflict",
                branch
            ),
            None,
            Some(vec![
                format!(
                    "Resolve manually: git fetch origin && git rebase origin/{branch}, fix conflicts, then: homeboy release {} --recover",
                    component_id
                ),
            ]),
        ));
    }

    push_branch("rebased release")?;

    Ok(Some(format!(
        "rebased release commit onto advanced remote and pushed {}",
        branch
    )))
}

pub(super) fn recovery_release_plan(
    component_id: &str,
    version: &str,
    tag_name: &str,
    commit_needed: bool,
    tag_needed: bool,
    push_needed: bool,
    actions: &[String],
) -> ReleasePlan {
    let mut steps = Vec::new();
    steps.push(recovery_step(
        "recover.commit",
        "Commit recovery changes",
        commit_needed,
        vec![],
    ));
    steps.push(recovery_step(
        "recover.tag",
        format!("Create tag {}", tag_name),
        tag_needed,
        vec!["recover.commit".to_string()],
    ));
    steps.push(recovery_step(
        "recover.push",
        "Push recovery state",
        push_needed,
        vec!["recover.tag".to_string()],
    ));

    for step in &mut steps {
        step.inputs.insert(
            "version".to_string(),
            serde_json::Value::String(version.to_string()),
        );
        step.inputs.insert(
            "tag".to_string(),
            serde_json::Value::String(tag_name.to_string()),
        );
    }

    ReleasePlan::new(
        component_id,
        !actions.is_empty(),
        steps,
        None,
        Vec::new(),
        actions.to_vec(),
    )
}

fn recovery_step(id: &str, label: impl Into<String>, needed: bool, needs: Vec<String>) -> PlanStep {
    if needed {
        PlanStep::ready_labeled(id, id, label, needs, std::iter::empty())
    } else {
        PlanStep::disabled_with_reason(id, id, "already-complete")
            .label(label)
            .needs(needs)
            .build()
    }
}

/// Resolve the most recent release-shaped tag for the component, honoring
/// monorepo prefixes. Returns `None` if no matching tag is found.
fn latest_release_tag(local_path: &str, monorepo: Option<&git::MonorepoContext>) -> Option<String> {
    match monorepo {
        Some(ctx) => git::get_latest_tag_with_prefix(&ctx.git_root, Some(&ctx.tag_prefix)).ok()?,
        None => git::get_latest_tag(local_path).ok()?,
    }
}

/// Inspect the latest release tag for the orphan-tag pattern (#2234): a tag
/// whose tagged commit subject is not `release: vX.Y.Z`. Returns a one-line
/// warning when the tag looks orphaned, otherwise `None`.
///
/// This is intentionally a soft warning — `--recover` may still be the
/// right move (re-commit the working tree), but the operator deserves to
/// know they're recovering on top of a misplaced tag before they push more
/// state to origin.
pub(super) fn diagnose_orphan_tag(local_path: &str, tag: &str) -> Option<String> {
    let tag_commit = git::get_tag_commit(local_path, tag).ok()?;
    let subject_output =
        git::execute_git_for_release(local_path, &["log", "-1", "--format=%s", &tag_commit])
            .ok()?;
    if !subject_output.status.success() {
        return None;
    }
    let subject = String::from_utf8_lossy(&subject_output.stdout)
        .trim()
        .to_string();

    if subject.starts_with("release: v") || subject.starts_with("release:v") {
        return None;
    }

    Some(format!(
        "⚠ Latest tag {} points at commit {} ({}) — not a `release: v...` commit. \
         This matches the orphan-tag pattern from issue #2234. Inspect the tag/commit before recovering: \
         `git show {}`. To delete a misplaced tag locally and on origin: \
         `git tag -d {} && git push origin :refs/tags/{}`",
        tag,
        &tag_commit[..8.min(tag_commit.len())],
        subject,
        tag,
        tag,
        tag,
    ))
}
