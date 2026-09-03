//! Release recovery (`release --recover`): finish a partially-completed release
//! by reconciling the branch, retagging to HEAD when needed, and re-pushing —
//! plus the orphan-tag diagnostics and the recovery plan/step builders.
//!
//! Split out of `workflow.rs` to keep the main release-command flow focused.

use homeboy_core::error::{Error, Result};
use homeboy_core::git;
use homeboy_core::plan::PlanStep;
use homeboy_engine_primitives::shell::quote_path;

use super::advanced_remote;
use super::context::load_component;
use super::scope::ReleaseScope;
use super::types::{ReleaseCommandInput, ReleaseCommandResult, ReleaseOptions, ReleasePlan};
use super::workflow::{release_execution_plan, short_sha};

/// `--recover` repairs Git state only. Keep the remaining release lifecycle
/// explicit so an orchestration result cannot mistake a pushed tag for a
/// published release.
pub(crate) const RECOVERY_INCOMPLETE_EXIT_CODE: i32 = 4;

fn publication_continuation_command(input: &ReleaseCommandInput) -> String {
    let mut command = format!("homeboy release {} --head", input.component_id);
    if let Some(path) = &input.path_override {
        command.push_str(&format!(" --path {}", quote_path(path)));
    }
    if input.skip_checks {
        command.push_str(" --skip-checks");
    } else if !input.skip_checks_granular.is_empty() {
        command.push_str(&format!(
            " --skip-checks={}",
            input.skip_checks_granular.join(",")
        ));
    }
    if input.skip_build_validation {
        command.push_str(" --skip-build-validation");
    }
    if input.pipeline.skip_publish {
        command.push_str(" --skip-publish");
    }
    if input.pipeline.deploy {
        command.push_str(" --deploy");
    }
    if let Some(identity) = &input.git_identity {
        command.push_str(&format!(" --git-identity {}", quote_path(identity)));
    }
    command.push_str(" --apply");
    command
}

pub(super) fn run_recover(
    roots: &homeboy_core::paths::PathRoots,
    input: &ReleaseCommandInput,
    owner_run_ref: Option<&str>,
) -> Result<(
    ReleaseCommandResult,
    Option<super::types::ReleaseWorkspaceOutput>,
    i32,
)> {
    if let Some(record) =
        super::workspace::reconcile_pending(roots, &input.component_id, owner_run_ref)?
    {
        return Ok((ReleaseCommandResult {
            component_id: input.component_id.clone(), status: if record.attributes.get("release_pushed").and_then(serde_json::Value::as_bool).unwrap_or(false) { "released" } else { "workspace_reconciled" }.to_string(),
            phase: release_execution_plan(input).phase, bump_type: "recover".to_string(), dry_run: false,
            releasable_commits: 0, new_version: None, tag: None, skipped_reason: None, plan: None, run: None,
            deployment: None, continuation_command: None,
            release_summary: vec![format!("Reconciled provider workspace `{}` without replaying release mutation or push.", record.owner_run_ref)],
            readiness: None,
        }, Some(super::workspace::output_from_record(&record)), 0));
    }
    if let Some(deployment) = super::deployment::resume_deployment(roots, &input.component_id)? {
        let failed = deployment.summary.failed > 0;
        return Ok((
            ReleaseCommandResult {
                component_id: input.component_id.clone(),
                status: if failed { "deploy_recovery_failed" } else { "released" }.to_string(),
                phase: release_execution_plan(input).phase,
                bump_type: "recover".to_string(),
                dry_run: false,
                releasable_commits: 0,
                new_version: None,
                tag: None,
                skipped_reason: None,
                plan: None,
                run: None,
                deployment: Some(deployment),
                continuation_command: None,
                release_summary: vec!["Resumed only incomplete release deployment targets; publication steps were not replayed.".to_string()],
                readiness: None,
            },
            None,
            if failed { 1 } else { 0 },
        ));
    }
    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;

    let release_scope = ReleaseScope::resolve(&component, &input.component_id)?;
    let version_info = crate::release::version::read_component_version(&component)?;
    let current_version = &version_info.version;
    let tag_name = release_scope.tag_name(current_version);

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
    if let Some(latest_tag) = latest_release_tag(&release_scope) {
        if let Some(diagnostic) = diagnose_orphan_tag(&component.local_path, &latest_tag) {
            homeboy_core::log_status!("recover", "{}", diagnostic);
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

    if input.retag {
        if let Some(result) = recreate_divergent_unpublished_release(
            input,
            &component,
            &release_scope,
            current_version,
            &head_commit,
            |candidate_tag| {
                crate::release::executor::github_release_exists_for_tag(&component, candidate_tag)
            },
        )? {
            return Ok((result, None, RECOVERY_INCOMPLETE_EXIT_CODE));
        }
    }

    let tag_is_stale = local_tag_commit
        .as_deref()
        .is_some_and(|commit| commit != head_commit)
        || remote_tag_commit
            .as_deref()
            .is_some_and(|commit| commit != head_commit);

    // An interrupted attempt can leave a tag only in this checkout while the
    // release commit was recreated on a sibling history. Retagging cannot
    // safely reconcile divergent history, but removing this unpushed ref is a
    // deterministic local-only recovery step.
    let local_tag_is_unpushed_sibling = is_local_unpushed_sibling_tag(
        &component.local_path,
        local_tag_commit.as_deref(),
        remote_tag_commit.as_deref(),
        &head_commit,
    )?;

    if local_tag_is_unpushed_sibling {
        let local_commit = local_tag_commit
            .as_deref()
            .expect("sibling state requires a local tag commit");
        return Err(Error::validation_invalid_argument(
            "tag",
            format!(
                "Tag '{}' exists only locally at sibling commit {}; HEAD is {}. Refusing to retag or delete the tag automatically.",
                tag_name,
                short_sha(local_commit),
                short_sha(&head_commit)
            ),
            None,
            Some(local_sibling_tag_cleanup_hints(
                &input.component_id,
                &tag_name,
                local_commit,
                &head_commit,
            )),
        ));
    }

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
            crate::release::executor::version_targets::collect_head_version_mismatches(
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

        require_unpublished_github_release(
            &tag_name,
            crate::release::executor::github_release_exists_for_tag(&component, &tag_name),
        )?;

        if input.dry_run {
            let actions = vec![format!("would retag {} to HEAD", tag_name)];
            return Ok((
                recovery_dry_run_result(
                    input,
                    current_version,
                    &tag_name,
                    false,
                    true,
                    true,
                    actions,
                ),
                None,
                0,
            ));
        }

        if let Some(ref identity_str) = input.git_identity {
            let identity = git::parse_git_identity(Some(identity_str));
            git::configure_identity(&component.local_path, &identity)?;
        }

        // Safe to move: delete the stale tag (local + remote) and re-create at HEAD.
        homeboy_core::log_status!(
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

        require_unpublished_github_release(
            &tag_name,
            crate::release::executor::github_release_exists_for_tag(&component, &tag_name),
        )?;

        if tag_exists_local {
            git::delete_local_tag(&component.local_path, &tag_name)?;
        }
        if tag_exists_remote {
            git::delete_remote_tag(&component.local_path, &tag_name)?;
        }

        create_tag("Failed to re-create tag at HEAD")?;

        push_tags(&format!("Failed to push retagged {}", tag_name))?;

        let actions = vec![format!("retagged {} to HEAD", tag_name)];
        let continuation_command = publication_continuation_command(input);
        homeboy_core::log_status!(
            "recover",
            "Git recovery complete for v{}: {}. Release publication remains incomplete; run: {}",
            current_version,
            actions.join(", "),
            continuation_command
        );
        return Ok((
            ReleaseCommandResult {
                component_id: input.component_id.clone(),
                status: "git_recovered".to_string(),
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
                continuation_command: Some(continuation_command.clone()),
                release_summary: [
                    actions,
                    vec![format!(
                        "Git state recovered; release publication is incomplete. Run: {}",
                        continuation_command
                    )],
                ]
                .concat(),
                readiness: None,
            },
            None,
            RECOVERY_INCOMPLETE_EXIT_CODE,
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

    if input.dry_run {
        let mut actions = Vec::new();
        if uncommitted.has_changes {
            actions.push("would commit version files".to_string());
        }
        if !tag_exists_local {
            actions.push(format!("would create tag {}", tag_name));
        }
        if !tag_exists_remote {
            actions.push("would push commits and tags".to_string());
        }

        return Ok((
            recovery_dry_run_result(
                input,
                current_version,
                &tag_name,
                uncommitted.has_changes,
                !tag_exists_local,
                !tag_exists_remote,
                actions,
            ),
            None,
            0,
        ));
    }

    // Recovery dry-runs must not update repository config. Apply the requested
    // identity only immediately before a recovery can create a commit or tag.
    if uncommitted.has_changes || !tag_exists_local {
        if let Some(ref identity_str) = input.git_identity {
            let identity = git::parse_git_identity(Some(identity_str));
            git::configure_identity(&component.local_path, &identity)?;
        }
    }

    let mut actions = Vec::new();

    if uncommitted.has_changes {
        homeboy_core::log_status!("recover", "Committing uncommitted changes...");
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
        homeboy_core::log_status!("recover", "Creating tag {}...", tag_name);
        create_tag("Failed to create tag")?;
        actions.push(format!("created tag {}", tag_name));
    }

    if !tag_exists_remote {
        homeboy_core::log_status!("recover", "Pushing to remote...");
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

    let continuation_command = publication_continuation_command(input);
    if actions.is_empty() {
        homeboy_core::log_status!(
            "recover",
            "Git recovery found no pending Git changes for v{}. Release publication was not verified or run; it remains incomplete. Run: {}",
            current_version,
            continuation_command
        );
    } else {
        homeboy_core::log_status!(
            "recover",
            "Git recovery complete for v{}: {}. Release publication remains incomplete; run: {}",
            current_version,
            actions.join(", "),
            continuation_command
        );
    }

    Ok((
        ReleaseCommandResult {
            component_id: input.component_id.clone(),
            status: "git_recovered".to_string(),
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
            continuation_command: Some(continuation_command.clone()),
            release_summary: [
                if actions.is_empty() {
                    vec![format!("Git state already exists: {}", tag_name)]
                } else {
                    actions.to_vec()
                },
                vec![format!(
                    "Git state recovered; release publication is incomplete. Run: {}",
                    continuation_command
                )],
            ]
            .concat(),
            readiness: None,
        },
        None,
        RECOVERY_INCOMPLETE_EXIT_CODE,
    ))
}

/// Recreate an unpublished release commit when an interrupted release left its
/// tag on divergent history while the current branch still carries the prior
/// source version. This is intentionally part of the guarded `--recover
/// --retag --apply` path: it never guesses at a tag, published release, or
/// branch rewrite.
fn recreate_divergent_unpublished_release<F>(
    input: &ReleaseCommandInput,
    component: &homeboy_core::component::Component,
    release_scope: &ReleaseScope,
    current_version: &str,
    head_commit: &str,
    github_release_exists: F,
) -> Result<Option<ReleaseCommandResult>>
where
    F: Fn(&str) -> Option<bool>,
{
    // Recovery must inspect the highest release identity even when it is not
    // reachable from HEAD; that divergence is the state this path repairs.
    let Some(tag_name) = release_scope.latest_tag_any()? else {
        return Ok(None);
    };
    let Some(version) = tag_name.rsplit_once('v').map(|(_, version)| version) else {
        return Ok(None);
    };
    let (Ok(tag_version), Ok(source_version)) = (
        semver::Version::parse(version),
        semver::Version::parse(current_version),
    ) else {
        return Ok(None);
    };
    if tag_version < source_version {
        return Ok(None);
    }

    let path = &component.local_path;
    let local_tag = if git::tag_exists_locally(path, &tag_name)? {
        Some(git::get_tag_commit(path, &tag_name)?)
    } else {
        None
    };
    let (Some(local_tag), Some(remote_tag)) = (local_tag, git::remote_tag_commit(path, &tag_name)?)
    else {
        return Ok(None);
    };
    if local_tag != remote_tag || git::is_ancestor(path, &local_tag, head_commit)? {
        return Ok(None);
    }
    if input.bump_override.as_deref() != Some(version) {
        return Err(Error::validation_invalid_argument(
            "bump",
            format!(
                "Divergent tag recovery requires --bump {} to name the exact release being replaced",
                version
            ),
            None,
            Some(vec![format!(
                "After inspecting {}, retry with: --recover --retag --bump {} --apply",
                tag_name, version
            )]),
        ));
    }
    require_interrupted_release_lineage(path, &tag_name, version, &local_tag, head_commit)?;

    require_unpublished_github_release(&tag_name, github_release_exists(&tag_name))?;
    let changelog_entries = if tag_version > source_version {
        Some(super::planning_changelog::generate_changelog_entries(
            component,
            &input.component_id,
            &ReleaseOptions {
                dry_run: input.dry_run,
                ..Default::default()
            },
            release_scope,
        )?)
    } else {
        None
    };

    let actions = vec![format!(
        "recreated release commit for {} on the current branch and replaced divergent tag {}",
        version, tag_name
    )];
    if input.dry_run {
        return Ok(Some(recovery_dry_run_result(
            input, version, &tag_name, true, true, true, actions,
        )));
    }
    if let Some(identity) = &input.git_identity {
        git::configure_identity(path, &git::parse_git_identity(Some(identity)))?;
    }

    if tag_version > source_version {
        let bump = crate::release::version::bump_component_version_with_changelog(
            component,
            version,
            changelog_entries.as_ref(),
            None,
        )?;
        if let Some(mismatches) =
            crate::release::executor::version_targets::collect_version_target_mismatches(
                component,
                &bump.new_version,
            )
        {
            return Err(version_target_recovery_error(
                &tag_name,
                version,
                "version targets",
                mismatches,
            ));
        }
        let commit = git::commit(
            Some(&input.component_id),
            Some(&format!("release: v{}", version)),
            git::CommitOptions {
                staged_only: false,
                files: None,
                exclude: None,
                amend: false,
            },
        )?;
        if !commit.success {
            return Err(Error::git_command_failed(format!(
                "Failed to recreate release commit: {}",
                commit.stderr
            )));
        }
    }
    if let Some(mismatches) =
        crate::release::executor::version_targets::collect_head_version_mismatches(
            component, version,
        )
    {
        return Err(version_target_recovery_error(
            &tag_name,
            version,
            "committed version targets",
            mismatches,
        ));
    }
    let branch = git::current_branch(std::path::Path::new(path)).ok_or_else(|| {
        Error::validation_invalid_argument(
            "branch",
            "Recovery requires a checked-out branch",
            None,
            None,
        )
    })?;
    let branch_push = git::push_at(
        Some(&input.component_id),
        git::PushOptions {
            refspec: Some(format!("HEAD:refs/heads/{branch}")),
            ..Default::default()
        },
        Some(path),
    )?;
    if !branch_push.success {
        reconcile_release_branch(component, &input.component_id)?;
    }

    git::fetch_origin(path)?;
    let remote_branch = git::remote_branch_commit(path, &branch)?.ok_or_else(|| {
        Error::git_command_failed(format!(
            "Remote branch {} disappeared during recovery",
            branch
        ))
    })?;
    let recovered_head = git::get_head_commit(path)?;
    if !git::is_ancestor(path, &recovered_head, &remote_branch)? {
        return Err(Error::git_command_failed(format!(
            "Recreated release commit is not reachable from origin/{}",
            branch
        )));
    }

    // Recheck immediately before replacing the public ref. The earlier check
    // protects all preceding work; this closes the destructive TOCTOU window.
    require_unpublished_github_release(&tag_name, github_release_exists(&tag_name))?;

    git::delete_local_tag(path, &tag_name)?;
    let tag = git::tag(
        Some(&input.component_id),
        Some(&tag_name),
        Some(&format!("Release {}", tag_name)),
    )?;
    if !tag.success {
        return Err(Error::git_command_failed(format!(
            "Failed to recreate tag {}: {}",
            tag_name, tag.stderr
        )));
    }
    let delete = git::delete_remote_tag(path, &tag_name)?;
    if !delete.success {
        return Err(Error::git_command_failed(format!(
            "Failed to delete remote tag {}: {}",
            tag_name, delete.stderr
        )));
    }
    let tag_push = git::push_at(
        Some(&input.component_id),
        git::PushOptions {
            refspec: Some(format!("refs/tags/{tag_name}:refs/tags/{tag_name}")),
            ..Default::default()
        },
        Some(path),
    )?;
    if !tag_push.success {
        return Err(Error::git_command_failed(format!(
            "Failed to push recreated tag {}: {}",
            tag_name, tag_push.stderr
        )));
    }
    git::fetch_origin(path)?;
    let remote_branch = git::remote_branch_commit(path, &branch)?.ok_or_else(|| {
        Error::git_command_failed(format!(
            "Remote branch {} disappeared during recovery",
            branch
        ))
    })?;
    let remote_tag = git::remote_tag_commit(path, &tag_name)?.ok_or_else(|| {
        Error::git_command_failed(format!(
            "Remote tag {} disappeared during recovery",
            tag_name
        ))
    })?;
    if !git::is_ancestor(path, &remote_tag, &remote_branch)? {
        return Err(Error::git_command_failed(format!(
            "Remote tag {} is not reachable from origin/{}",
            tag_name, branch
        )));
    }

    let continuation_command = publication_continuation_command(input);
    Ok(Some(ReleaseCommandResult {
        component_id: input.component_id.clone(),
        status: "git_recovered".to_string(),
        phase: release_execution_plan(input).phase,
        bump_type: "recover".to_string(),
        dry_run: false,
        releasable_commits: 0,
        new_version: None,
        tag: Some(tag_name.clone()),
        skipped_reason: None,
        plan: Some(recovery_release_plan(
            &input.component_id,
            version,
            &tag_name,
            true,
            true,
            true,
            &actions,
        )),
        run: None,
        deployment: None,
        continuation_command: Some(continuation_command.clone()),
        release_summary: [
            actions,
            vec![format!(
                "Git state recovered; release publication is incomplete. Run: {}",
                continuation_command
            )],
        ]
        .concat(),
        readiness: None,
    }))
}

fn require_interrupted_release_lineage(
    path: &str,
    tag_name: &str,
    version: &str,
    tag_commit: &str,
    head_commit: &str,
) -> Result<()> {
    let subject = git::execute_git_for_release(path, &["log", "-1", "--format=%s", tag_commit])?;
    let subject = String::from_utf8_lossy(&subject.stdout).trim().to_string();
    let expected_subject = format!("release: v{version}");
    let parent = git::execute_git_for_release(path, &["rev-parse", &format!("{tag_commit}^")])?;
    let parent = String::from_utf8_lossy(&parent.stdout).trim().to_string();
    let shares_branch_lineage = subject == expected_subject
        && !parent.is_empty()
        && git::is_ancestor(path, &parent, head_commit)?;
    if shares_branch_lineage {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "retag",
        format!(
            "Refusing to replace divergent tag '{}': its commit is not an interrupted release from the current branch history",
            tag_name
        ),
        None,
        Some(vec![format!(
            "Inspect the unrelated tag before resolving it manually: git show {}",
            tag_name
        )]),
    ))
}

fn require_unpublished_github_release(tag_name: &str, exists: Option<bool>) -> Result<()> {
    match exists {
        Some(false) => Ok(()),
        Some(true) => Err(Error::validation_invalid_argument(
            "retag",
            format!("Refusing to retag '{}': a GitHub Release already exists", tag_name),
            None,
            None,
        )),
        None => Err(Error::validation_invalid_argument(
            "retag",
            format!(
                "Refusing to retag '{}': could not verify whether a GitHub Release exists",
                tag_name
            ),
            None,
            Some(vec!["Authenticate gh for this repository and retry; moving a published release is destructive.".to_string()]),
        )),
    }
}

fn version_target_recovery_error(
    tag_name: &str,
    version: &str,
    target_label: &str,
    mismatches: Vec<crate::release::executor::version_targets::VersionTargetMismatch>,
) -> Error {
    Error::validation_invalid_argument(
        "retag",
        format!(
            "Refusing to recreate '{}': {} do not show {}",
            tag_name, target_label, version
        ),
        None,
        Some(
            mismatches
                .into_iter()
                .map(|m| {
                    format!(
                        "{} = {}",
                        m.file,
                        m.found.unwrap_or_else(|| "<unreadable>".to_string())
                    )
                })
                .collect(),
        ),
    )
}

fn recovery_dry_run_result(
    input: &ReleaseCommandInput,
    version: &str,
    tag_name: &str,
    commit_needed: bool,
    tag_needed: bool,
    push_needed: bool,
    actions: Vec<String>,
) -> ReleaseCommandResult {
    ReleaseCommandResult {
        component_id: input.component_id.clone(),
        status: "planned".to_string(),
        phase: release_execution_plan(input).phase,
        bump_type: "recover".to_string(),
        dry_run: true,
        releasable_commits: 0,
        new_version: None,
        tag: Some(tag_name.to_string()),
        skipped_reason: None,
        plan: Some(recovery_release_plan(
            &input.component_id,
            version,
            tag_name,
            commit_needed,
            tag_needed,
            push_needed,
            &actions,
        )),
        run: None,
        deployment: None,
        continuation_command: None,
        release_summary: actions,
        readiness: None,
    }
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
    component: &homeboy_core::component::Component,
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

    // The release commit is already on the remote branch — nothing to do.
    if git::is_ancestor(path, &head_commit, &remote_commit)? {
        return Ok(None);
    }

    // Remote head already contained in HEAD (a plain non-pushed branch): push.
    if git::is_ancestor(path, &remote_commit, &head_commit)? {
        homeboy_core::log_status!(
            "recover",
            "Pushing release commit to remote {} (remote did not advance)...",
            branch
        );
        let push = advanced_remote::push_release_branch(component, component_id, &branch)?;
        if !push.success {
            return Err(Error::git_command_failed(format!(
                "Failed to push release branch {}: {}",
                branch, push.stderr
            )));
        }
        return Ok(Some(format!("pushed release commit to {}", branch)));
    }

    // Histories diverged: the remote advanced after the release commit. Rebase
    // the release commit onto the advanced remote head, then push. Never force,
    // and never retag — `--recover` reconciles the branch only (the tag-push
    // block above already handled the tag). Shared with the release push step's
    // recovery so the two cannot drift (issue #3611).
    homeboy_core::log_status!(
        "recover",
        "Remote {} advanced — rebasing release commit onto the new head and re-pushing...",
        branch
    );
    match advanced_remote::rebase_onto_advanced_remote_and_push(
        component,
        component_id,
        &branch,
        &head_commit,
        &remote_commit,
        None,
    )? {
        Some(recovery) => {
            if !recovery.push.success {
                return Err(Error::git_command_failed(format!(
                    "Failed to push rebased release branch {}: {}",
                    branch, recovery.push.stderr
                )));
            }
            Ok(Some(format!(
                "rebased release commit onto advanced remote and pushed {}",
                branch
            )))
        }
        None => Err(Error::validation_invalid_argument(
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
        )),
    }
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

fn local_sibling_tag_cleanup_hints(
    component_id: &str,
    tag_name: &str,
    local_tag_commit: &str,
    head_commit: &str,
) -> Vec<String> {
    vec![
        format!(
            "Inspect the abandoned release commit and current HEAD: `git show --no-patch --decorate {} {}`.",
            tag_name, head_commit
        ),
        format!(
            "The tag is absent from origin, so after confirming {} is abandoned, remove only the local tag: `git tag -d {}`. This does not delete remote state.",
            short_sha(local_tag_commit), tag_name
        ),
        format!(
            "Before creating a replacement tag, check for an invalid GitHub Release: `gh release view {}`. If one exists, delete it deliberately before continuing.",
            tag_name
        ),
        format!(
            "Then retry recovery: `homeboy release {} --recover --apply`.",
            component_id
        ),
    ]
}

fn is_local_unpushed_sibling_tag(
    local_path: &str,
    local_tag_commit: Option<&str>,
    remote_tag_commit: Option<&str>,
    head_commit: &str,
) -> Result<bool> {
    match local_tag_commit {
        Some(commit) if remote_tag_commit.is_none() && commit != head_commit => {
            Ok(!git::is_ancestor(local_path, commit, head_commit)?)
        }
        _ => Ok(false),
    }
}

/// Resolve the most recent release-shaped tag for the component, honoring
/// monorepo prefixes. Returns `None` if no matching tag is found.
fn latest_release_tag(release_scope: &ReleaseScope) -> Option<String> {
    release_scope.latest_tag().ok()?
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

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn unpushed_sibling_tag_requires_local_only_cleanup() {
        let repo = tempfile::tempdir().expect("repo");
        git_in(repo.path(), &["init", "-b", "main"]);
        git_in(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git_in(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("version.txt"), "base").expect("write base");
        git_in(repo.path(), &["add", "."]);
        git_in(repo.path(), &["commit", "-m", "base"]);
        git_in(repo.path(), &["switch", "-c", "abandoned-release"]);
        std::fs::write(repo.path().join("version.txt"), "abandoned").expect("write sibling");
        git_in(repo.path(), &["commit", "-am", "release: v1.2.3"]);
        let tagged_commit = git_in(repo.path(), &["rev-parse", "HEAD"]);
        git_in(repo.path(), &["tag", "v1.2.3"]);
        git_in(repo.path(), &["switch", "main"]);
        std::fs::write(repo.path().join("version.txt"), "recreated").expect("write head");
        git_in(repo.path(), &["commit", "-am", "release: v1.2.3"]);
        let head_commit = git_in(repo.path(), &["rev-parse", "HEAD"]);
        let repo_path = repo.path().to_str().expect("utf-8 repo path");

        assert!(
            is_local_unpushed_sibling_tag(repo_path, Some(&tagged_commit), None, &head_commit,)
                .expect("inspect sibling tag")
        );

        let hints = local_sibling_tag_cleanup_hints("demo", "v1.2.3", &tagged_commit, &head_commit)
            .join("\n");
        assert!(hints.contains("git tag -d v1.2.3"));
        assert!(hints.contains("gh release view v1.2.3"));
        assert!(!hints.contains("git push origin :refs/tags/"));
        assert!(!hints.contains("gh release delete"));
    }

    #[test]
    fn publication_continuation_preserves_recovery_execution_flags() {
        let input = ReleaseCommandInput {
            component_id: "sample-plugin".to_string(),
            path_override: Some("/tmp/plugin path".to_string()),
            skip_checks: true,
            skip_build_validation: true,
            pipeline: super::super::types::ReleasePipelineOptions {
                skip_publish: true,
                deploy: true,
                ..Default::default()
            },
            git_identity: Some("Chris Huber <chris@example.com>".to_string()),
            ..Default::default()
        };

        assert_eq!(
            publication_continuation_command(&input),
            "homeboy release sample-plugin --head --path '/tmp/plugin path' --skip-checks --skip-build-validation --skip-publish --deploy --git-identity 'Chris Huber <chris@example.com>' --apply"
        );
    }

    #[test]
    fn recovery_incomplete_exit_code_is_not_success() {
        assert_ne!(RECOVERY_INCOMPLETE_EXIT_CODE, 0);
    }

    #[test]
    fn divergent_retag_refuses_unknown_github_release_lookup() {
        let error = require_unpublished_github_release("v1.2.3", None)
            .expect_err("unknown publication state must refuse tag replacement");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("could not verify"));
    }

    #[test]
    fn divergent_retag_refuses_unrelated_tag_lineage() {
        let repo = tempfile::tempdir().expect("repo");
        git_in(repo.path(), &["init", "-b", "main"]);
        git_in(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git_in(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("file.txt"), "base\n").expect("write base");
        git_in(repo.path(), &["add", "."]);
        git_in(repo.path(), &["commit", "-m", "base"]);
        git_in(repo.path(), &["switch", "-c", "unrelated"]);
        std::fs::write(repo.path().join("file.txt"), "unrelated\n").expect("write unrelated");
        git_in(
            repo.path(),
            &["commit", "-am", "feature: unrelated release candidate"],
        );
        let tag_commit = git_in(repo.path(), &["rev-parse", "HEAD"]);
        git_in(repo.path(), &["tag", "v1.2.3"]);
        git_in(repo.path(), &["switch", "main"]);
        std::fs::write(repo.path().join("main.txt"), "main\n").expect("write main");
        git_in(repo.path(), &["add", "."]);
        git_in(repo.path(), &["commit", "-m", "main advance"]);
        let head_commit = git_in(repo.path(), &["rev-parse", "HEAD"]);

        let error = require_interrupted_release_lineage(
            &repo.path().to_string_lossy(),
            "v1.2.3",
            "1.2.3",
            &tag_commit,
            &head_commit,
        )
        .expect_err("unrelated tag must not be adopted");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("not an interrupted release"));
    }

    #[test]
    fn divergent_unpublished_tag_is_recreated_on_the_remote_branch() {
        use homeboy_core::component::{Component, VersionTarget};

        homeboy_core::test_support::with_isolated_home(|_| {
            let remote = tempfile::tempdir().expect("remote");
            let repo = tempfile::tempdir().expect("repo");
            git_in(remote.path(), &["init", "--bare", "-b", "main"]);
            git_in(repo.path(), &["init", "-b", "main"]);
            git_in(repo.path(), &["config", "user.name", "Homeboy Test"]);
            git_in(
                repo.path(),
                &["config", "user.email", "homeboy@example.test"],
            );
            git_in(
                repo.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.path().to_str().expect("remote path"),
                ],
            );
            std::fs::write(repo.path().join("VERSION"), "0.1.0\n").expect("write version");
            std::fs::write(
                repo.path().join("CHANGELOG.md"),
                "# Changelog\n\n## [0.1.0] - 2026-09-01\n\n- Previous release\n",
            )
            .expect("write changelog");
            git_in(repo.path(), &["add", "."]);
            git_in(repo.path(), &["commit", "-m", "base"]);
            git_in(repo.path(), &["push", "-u", "origin", "main"]);
            let main_before = git_in(repo.path(), &["rev-parse", "HEAD"]);

            git_in(repo.path(), &["switch", "-c", "orphaned-release"]);
            std::fs::write(repo.path().join("VERSION"), "0.1.1\n").expect("write orphan version");
            git_in(repo.path(), &["commit", "-am", "release: v0.1.1"]);
            git_in(
                repo.path(),
                &["tag", "-a", "v0.1.1", "-m", "Release v0.1.1"],
            );
            git_in(repo.path(), &["push", "origin", "refs/tags/v0.1.1"]);
            git_in(repo.path(), &["switch", "main"]);

            let component = Component {
                id: "fixture".to_string(),
                local_path: repo.path().to_string_lossy().to_string(),
                changelog_target: Some("CHANGELOG.md".to_string()),
                version_targets: Some(vec![VersionTarget {
                    file: "VERSION".to_string(),
                    pattern: Some(r"^([0-9]+\.[0-9]+\.[0-9]+)$".to_string()),
                    artifact_path: None,
                }]),
                ..Component::default()
            };
            homeboy_core::component::write_standalone_component_config(&component)
                .expect("register fixture component");
            let scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
            assert_eq!(scope.tag_prefix(), None);
            assert_eq!(
                scope.latest_tag_any().expect("latest tag"),
                Some("v0.1.1".to_string())
            );
            let input = ReleaseCommandInput {
                component_id: "fixture".to_string(),
                path_override: Some(component.local_path.clone()),
                recover: true,
                retag: true,
                bump_override: Some("0.1.1".to_string()),
                ..Default::default()
            };

            let result = recreate_divergent_unpublished_release(
                &input,
                &component,
                &scope,
                "0.1.0",
                &main_before,
                |tag| {
                    assert_eq!(tag, "v0.1.1");
                    Some(false)
                },
            )
            .expect("recovery succeeds")
            .expect("divergent release detected");

            assert_eq!(result.status, "git_recovered");
            assert_eq!(result.tag.as_deref(), Some("v0.1.1"));
            assert_eq!(
                std::fs::read_to_string(repo.path().join("VERSION")).expect("read version"),
                "0.1.1\n"
            );
            assert!(std::fs::read_to_string(repo.path().join("CHANGELOG.md"))
                .expect("read changelog")
                .contains("## [0.1.1]"));
            git_in(repo.path(), &["fetch", "origin"]);
            let remote_main = git_in(repo.path(), &["rev-parse", "origin/main"]);
            let remote_tag = git_in(repo.path(), &["rev-parse", "v0.1.1^{commit}"]);
            assert_eq!(remote_tag, remote_main);
            assert_eq!(
                git_in(repo.path(), &["log", "-1", "--format=%s", "origin/main"]),
                "release: v0.1.1"
            );
        });
    }

    #[test]
    fn divergent_retag_reconciles_interrupted_equal_version_retry() {
        use homeboy_core::component::{Component, VersionTarget};

        homeboy_core::test_support::with_isolated_home(|_| {
            let remote = tempfile::tempdir().expect("remote");
            let repo = tempfile::tempdir().expect("repo");
            let other = tempfile::tempdir().expect("other clone");
            git_in(remote.path(), &["init", "--bare", "-b", "main"]);
            git_in(repo.path(), &["init", "-b", "main"]);
            git_in(repo.path(), &["config", "user.name", "Homeboy Test"]);
            git_in(
                repo.path(),
                &["config", "user.email", "homeboy@example.test"],
            );
            git_in(
                repo.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.path().to_str().expect("remote path"),
                ],
            );
            std::fs::write(repo.path().join("VERSION"), "0.1.0\n").expect("write version");
            std::fs::write(repo.path().join("CHANGELOG.md"), "# Changelog\n")
                .expect("write changelog");
            git_in(repo.path(), &["add", "."]);
            git_in(repo.path(), &["commit", "-m", "base"]);
            git_in(repo.path(), &["push", "-u", "origin", "main"]);
            git_in(
                other.path(),
                &["clone", remote.path().to_str().expect("remote path"), "."],
            );
            git_in(other.path(), &["config", "user.name", "Homeboy Test"]);
            git_in(
                other.path(),
                &["config", "user.email", "homeboy@example.test"],
            );

            git_in(repo.path(), &["switch", "-c", "orphaned-release"]);
            std::fs::write(repo.path().join("VERSION"), "0.1.1\n").expect("write orphan version");
            git_in(repo.path(), &["commit", "-am", "release: v0.1.1"]);
            git_in(
                repo.path(),
                &["tag", "-a", "v0.1.1", "-m", "Release v0.1.1"],
            );
            git_in(repo.path(), &["push", "origin", "refs/tags/v0.1.1"]);

            git_in(repo.path(), &["switch", "main"]);
            std::fs::write(other.path().join("first.txt"), "first\n")
                .expect("write first concurrent change");
            git_in(other.path(), &["add", "."]);
            git_in(other.path(), &["commit", "-m", "first concurrent advance"]);
            git_in(other.path(), &["push", "origin", "main"]);
            git_in(repo.path(), &["pull", "--rebase", "origin", "main"]);

            std::fs::write(repo.path().join("VERSION"), "0.1.1\n").expect("write retry version");
            git_in(repo.path(), &["commit", "-am", "release: v0.1.1"]);
            let retry_head = git_in(repo.path(), &["rev-parse", "HEAD"]);

            std::fs::write(other.path().join("second.txt"), "second\n")
                .expect("write second concurrent change");
            git_in(other.path(), &["add", "."]);
            git_in(other.path(), &["commit", "-m", "second concurrent advance"]);
            git_in(other.path(), &["push", "origin", "main"]);

            let component = Component {
                id: "fixture".to_string(),
                local_path: repo.path().to_string_lossy().to_string(),
                changelog_target: Some("CHANGELOG.md".to_string()),
                version_targets: Some(vec![VersionTarget {
                    file: "VERSION".to_string(),
                    pattern: Some(r"^([0-9]+\.[0-9]+\.[0-9]+)$".to_string()),
                    artifact_path: None,
                }]),
                ..Component::default()
            };
            homeboy_core::component::write_standalone_component_config(&component)
                .expect("register fixture component");
            let scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
            let input = ReleaseCommandInput {
                component_id: "fixture".to_string(),
                path_override: Some(component.local_path.clone()),
                recover: true,
                retag: true,
                bump_override: Some("0.1.1".to_string()),
                ..Default::default()
            };

            assert_eq!(scope.tag_prefix(), None);
            assert_eq!(
                scope.latest_tag_any().expect("latest tag"),
                Some("v0.1.1".to_string())
            );
            let local_tag =
                git::get_tag_commit(&component.local_path, "v0.1.1").expect("local tag commit");
            let remote_tag = git::remote_tag_commit(&component.local_path, "v0.1.1")
                .expect("remote tag lookup")
                .expect("remote tag commit");
            assert_eq!(local_tag, remote_tag);
            assert!(
                !git::is_ancestor(&component.local_path, &local_tag, &retry_head)
                    .expect("tag ancestry")
            );
            let result = recreate_divergent_unpublished_release(
                &input,
                &component,
                &scope,
                "0.1.1",
                &retry_head,
                |_| Some(false),
            )
            .expect("retry recovery succeeds")
            .expect("equal-version divergent release detected");

            assert_eq!(result.status, "git_recovered");
            git_in(repo.path(), &["fetch", "origin"]);
            let remote_main = git_in(repo.path(), &["rev-parse", "origin/main"]);
            let remote_tag = git_in(repo.path(), &["rev-parse", "v0.1.1^{commit}"]);
            assert_eq!(remote_tag, remote_main);
            assert_eq!(
                git_in(repo.path(), &["show", "origin/main:second.txt"]),
                "second"
            );
            assert_eq!(
                git_in(repo.path(), &["show", "origin/main:VERSION"]),
                "0.1.1"
            );
        });
    }
}
