//! Cook workspace validation, base materialization and ancestry.

use super::cook::*;

pub(super) fn resumable_cook_run_id_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    recipe: &super::AgentTaskCookRecipe,
    cook_id: &str,
    initial_run_id: &str,
    requested_attempt: u32,
    verification_pending_continuation: bool,
) -> Option<String> {
    (!verification_pending_continuation)
        .then(|| lifecycle_store.select_cook_candidate(cook_id).ok())
        .flatten()
        .map(|selection| selection.run_id)
        .filter(|run_id| run_id != initial_run_id)
        .filter(|run_id| {
            recipe
                .attempts
                .iter()
                .find(|attempt| attempt.run_id == *run_id)
                .is_some_and(|attempt| attempt.attempt >= requested_attempt)
        })
}

/// A multi-candidate Cook has one controller-owned destination. Reject ambiguous
/// plans before any provider preflight or scheduler execution can spend work.
pub(super) fn validate_cook_candidate_group(plan: &AgentTaskPlan) -> Result<()> {
    if plan.tasks.len() <= 1 {
        return Ok(());
    }
    let group_key = plan.group_key.as_deref().or_else(|| {
        plan.tasks
            .first()
            .and_then(|task| task.group_key.as_deref())
    });
    let Some(group_key) = group_key else {
        return Err(Error::validation_invalid_argument(
            "group_key",
            "Cook candidates require one explicit shared group",
            None,
            None,
        ));
    };
    if plan
        .tasks
        .iter()
        .any(|task| task.group_key.as_deref() != Some(group_key))
    {
        return Err(Error::validation_invalid_argument(
            "group_key",
            "every Cook candidate must use the plan shared group",
            Some(group_key.to_string()),
            None,
        ));
    }
    Ok(())
}

/// Only Cook's authenticated baseline transition may replace a durable task
/// workspace identity. Preserve the predecessor as provenance; callers cannot
/// mint this attestation through an arbitrary provider request.
pub(super) fn bind_dispatch_workspace_attestations(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook dispatch requires a workspace root",
                Some(task.task_id.clone()),
                None,
            )
        })?;
        let prior = task.metadata.get("cook_workspace_identity").cloned();
        let identity =
            crate::agent_task_workspace_identity::attest_workspace(std::path::Path::new(root))?;
        task.metadata["cook_workspace_identity"] = identity;
        if let Some(prior) = prior {
            task.metadata["cook_workspace_identity_predecessor"] = prior;
        }
    }
    Ok(())
}

pub fn cook_continuation_requires_model_provenance(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    !cook_run_record_needs_execution(record)
}

pub(super) fn cook_attempt_needs_execution_with_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> bool {
    lifecycle_store
        .read_record(run_id)
        .map(|record| cook_run_record_needs_execution(&record))
        .unwrap_or(true)
}

pub(super) fn cook_run_record_needs_execution(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    (!agent_task_lifecycle::CookRunState::of(record).terminal
        || retryable_pre_execution_failure(record))
        && !agent_task_lifecycle::CookRunState::accepted_lab_handoff(record)
}

/// Validate the Cook target before a provider can run. An explicit source path
/// is already the authoritative workspace; otherwise resolve the declared
/// handle through the existing local/provider path.
#[cfg(test)]
pub(super) fn validate_cook_workspace(
    options: &CookRequest,
) -> Result<Option<CookWorkspaceBaseValidation>> {
    validate_cook_workspace_with_adopted_candidate(
        &AgentTaskLifecycleStore::from_environment()?,
        options,
        false,
    )
}

pub(super) fn validate_cook_workspace_with_adopted_candidate(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    adopted_dirty_candidate: bool,
) -> Result<Option<CookWorkspaceBaseValidation>> {
    let native_record = native_cook_destination_record(options)?;
    let adopted_dirty_candidate = adopted_dirty_candidate
        && native_record.as_ref().is_none_or(|record| {
            cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)
                .unwrap_or(false)
                && record.run_id.as_deref().is_some_and(|owner| {
                    lifecycle_store
                        .read_cook_index(&options.identity.cook_id)
                        .ok()
                        .is_some_and(|index| {
                            index.attempts.iter().any(|attempt| attempt.run_id == owner)
                        })
                })
        });
    let continuation = tracked_promotion_continuation_in_store(lifecycle_store, options)?;
    let source = options.workspace.source_worktree_path.as_deref();
    let target = if cook_uses_explicit_cwd_workspace(options) {
        source
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace",
                    "explicit Cook workspace is missing its source path",
                    Some(options.workspace.to_worktree.clone()),
                    None,
                )
            })?
            .to_path_buf()
    } else if Path::new(&options.workspace.to_worktree).is_dir() {
        PathBuf::from(&options.workspace.to_worktree)
    } else if let Some(target) =
        homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(
            &options.workspace.to_worktree,
        )?
    {
        target.path
    } else {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook requires an active native worktree",
            Some(options.workspace.to_worktree.clone()),
            None,
        ));
    };
    let target = homeboy_core::worktree_provider::require_linked_worktree_mutation_path(
        &target,
        &options.workspace.to_worktree,
    )?;
    let tracked_candidate = continuation.is_some();
    if let Some(continuation) = continuation {
        authenticate_tracked_promotion_continuation(&target, &continuation)?;
    }
    if !tracked_candidate
        && !cook_uses_explicit_cwd_workspace(options)
        && !cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)?
    {
        supersede_previous_cook_candidate(lifecycle_store, options, &target)?;
    }
    preflight_cook_workspace_base_ancestry_with_provider(
        &target,
        options,
        adopted_dirty_candidate,
        tracked_candidate,
    )
    .map_err(|error| with_pre_execution_phase(error, "workspace_base_ancestry_preflight"))
}

/// Retire only an exact, unpublished destination candidate. A fingerprint binds
/// the entire tree, changed path set, destination and HEAD, not just overlapping
/// paths. Keep a Git-owned durable copy before removing anything from disk.
pub(super) fn supersede_previous_cook_candidate(
    store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    target: &Path,
) -> Result<()> {
    let actual = candidate_fingerprint(target.to_string_lossy().as_ref())?;
    let AgentTaskPromotionCandidate::Git { fingerprint } = &actual else {
        return Ok(());
    };
    if fingerprint.changed_files.is_empty() {
        return Ok(());
    }
    // Do not discard index-only edits hidden by the final workspace tree.
    // Candidate placement stages the whole candidate; an independently edited
    // or partially staged index is not sufficient supersession authority.
    let index_tree =
        homeboy_core::git::run_git(target, &["write-tree"], "inspect candidate index")?;
    let head_tree = homeboy_core::git::run_git(
        target,
        &["rev-parse", "HEAD^{tree}"],
        "inspect candidate base",
    )?;
    if index_tree.trim() != fingerprint.tree && index_tree.trim() != head_tree.trim() {
        return Ok(());
    }
    for old in agent_task_lifecycle::list_records_in_store(store)? {
        if old.run_id == options.identity.initial_run_id
            || old.metadata.get("superseded_by_run_id").is_some()
        {
            continue;
        }
        let Some(delivery) = super::CookDelivery::from_run_metadata(&old.metadata) else {
            continue;
        };
        if delivery.publication.is_some() {
            continue;
        }
        let Some(promotion) = super::recorded_promotion_report(&old.metadata)? else {
            continue;
        };
        let expected = promotion
            .pointer("/provenance/candidate")
            .cloned()
            .and_then(|value| serde_json::from_value::<AgentTaskPromotionCandidate>(value).ok());
        if expected.as_ref() != Some(&actual) {
            continue;
        }
        // Destination identity is included in the fingerprint. Only stopped
        // cooks may surrender it; never steal a live provider's workspace. A
        // Cook cancelled mid-gate leaves its promoted candidate staged just
        // like a failed one (#17910), so it surrenders it too once its driver
        // has exited.
        let state = agent_task_lifecycle::CookRunState::of(&old);
        if !state.recoverable && !state.retryable {
            continue;
        }
        if let Some(recipe) = super::load_recipe_for_attempt(&old.run_id)? {
            if super::cook_prior_candidate::cook_driver_is_live(&recipe.cook_id)? {
                continue;
            }
        }
        let message = format!("homeboy: superseded candidate of {}", old.run_id);
        homeboy_core::git::run_git(
            target,
            &["stash", "push", "--include-untracked", "-m", &message],
            "preserve superseded Cook candidate",
        )?;
        let stash = homeboy_core::git::run_git(
            target,
            &["rev-parse", "refs/stash"],
            "resolve superseded Cook candidate stash",
        )?;
        let status = homeboy_core::git::run_git(
            target,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            "verify superseded candidate was removed",
        )?;
        if !status.trim().is_empty() {
            return Err(Error::validation_invalid_argument("workspace",
                "Superseded candidate was stashed but the destination is still dirty; refusing provider execution",
                Some(target.display().to_string()), None));
        }
        // A dedicated ref survives stash-list edits and ordinary Git GC.
        let preserved_ref = format!("refs/homeboy/superseded/{}", stash.trim());
        homeboy_core::git::run_git(
            target,
            &["update-ref", &preserved_ref, stash.trim()],
            "retain superseded Cook candidate",
        )?;
        let evidence = serde_json::json!({
            "superseded_run_id": old.run_id,
            "preserved_ref": preserved_ref,
            "stash_oid": stash.trim(),
            "workspace": target,
        });
        store.mutate_record(&options.identity.initial_run_id, |record| {
            record.metadata["superseded_candidate"] = evidence.clone();
            true
        })?;
        store.mutate_record(&old.run_id, |record| {
            record.metadata["superseded_by_run_id"] =
                options.identity.initial_run_id.clone().into();
            record.metadata["superseded_candidate"] = evidence.clone();
            true
        })?;
        return Ok(());
    }
    Ok(())
}

/// Snapshot authority is passed to the scheduler, while convergence evidence is
/// durable controller metadata. Keeping them distinct prevents provider details
/// from being interpreted as an immutable workspace snapshot.
#[derive(Debug)]
pub(super) enum CookWorkspaceBaseValidation {
    Snapshot(Value),
    Convergence(Value),
}

pub(super) fn record_cook_workspace_base_convergence(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    evidence: Value,
) -> Result<()> {
    lifecycle_store.mutate_record(run_id, |record| {
        record.metadata["cook_workspace_base_convergence"] = evidence;
        true
    })?;
    Ok(())
}

/// Capture the base once before a new Cook recipe freezes its execution
/// lineage. Existing recipes retain their recorded boundary on every retry.
pub(super) fn pin_initial_cook_workspace_base(options: &mut CookRequest) -> Result<()> {
    let workspace = cook_startup_workspace(options);
    let Some(workspace) = workspace.map(Path::to_path_buf) else {
        return Ok(());
    };
    pin_cook_workspace_base_at(options, &workspace)
}

/// The local tree whose base and projected capacity Cook validates before it
/// dispatches. A pending runner-owned workspace intentionally has no local path.
pub(super) fn cook_startup_workspace(options: &CookRequest) -> Option<&Path> {
    options
        .workspace
        .source_worktree_path
        .as_deref()
        .or_else(|| {
            Path::new(&options.workspace.to_worktree)
                .is_dir()
                .then_some(Path::new(&options.workspace.to_worktree))
        })
        .or_else(|| {
            options
                .identity
                .initial_plan
                .tasks
                .first()
                .and_then(|task| task.workspace.root.as_deref())
                .map(Path::new)
        })
}

pub(super) fn pin_and_persist_initial_cook_workspace_base(
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &mut CookRequest,
) -> Result<()> {
    let cook_id = options.identity.cook_id.clone();
    store.with_workspace_base_capture_lock(&cook_id, || {
        let mut recipe = store.load_recipe(&options.identity.cook_id)?;
        if recipe.finalization["task_base_sha"].is_string() {
            apply_persisted_task_base(options, &recipe)?;
            return reconcile_persisted_task_base_plan(lifecycle_store, options, &recipe);
        }

        pin_initial_cook_workspace_base(options)?;
        let Some(task_base_sha) = options.workspace.task_base_sha.as_ref() else {
            return Ok(());
        };
        let attempt = recipe
            .attempts
            .iter_mut()
            .find(|attempt| attempt.run_id == options.identity.initial_run_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "cook_recipe.attempts",
                    "initial Cook recipe is missing its materialized attempt",
                    Some(options.identity.initial_run_id.clone()),
                    None,
                )
            })?;
        // Only the pinned base joins the frozen attempt. The initial plan may be
        // a run's materialized continuation, and copying it would rewrite the
        // work a retry preparation validates against.
        attempt.plan.metadata["cook_workspace_base"] =
            options.identity.initial_plan.metadata["cook_workspace_base"].clone();
        // The recipe's sensitive mapping projection is derived from its attempt
        // plans. Replacing a plan must recompute it, as every other recipe write
        // does, or the recipe fails validation the next time it is persisted or
        // loaded (#15338).
        recipe.sensitive_mappings =
            super::cook_recipe::canonical_sensitive_mappings(&recipe.attempts)?;
        recipe.finalization["task_base_sha"] = Value::String(task_base_sha.clone());
        store.persist_recipe(&recipe)?;
        #[cfg(test)]
        if WORKSPACE_BASE_CAPTURE_HOOK
            .lock()
            .expect("workspace base capture hook")
            .as_ref()
            .is_some_and(|hook| {
                hook.fail_after_recipe_persistence
                    .swap(false, Ordering::SeqCst)
            })
        {
            return Err(Error::internal_unexpected(
                "test interruption after Cook recipe base persistence",
            ));
        }
        apply_persisted_task_base(options, &recipe)?;
        reconcile_persisted_task_base_plan(lifecycle_store, options, &recipe)
    })
}

pub(super) fn reconcile_persisted_task_base_plan(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
) -> Result<()> {
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == options.identity.initial_run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "initial Cook recipe is missing its materialized attempt",
                Some(options.identity.initial_run_id.clone()),
                None,
            )
        })?;
    // The recipe write precedes the controller plan write. After an interruption,
    // its immutable attempt plan is the only authority needed to repair that
    // second projection without touching origin.
    if lifecycle_store
        .read_controller_plan(&options.identity.initial_run_id)
        .ok()
        .as_ref()
        != Some(&attempt.plan)
    {
        agent_task_lifecycle::persist_controller_plan_in_store(
            lifecycle_store,
            &options.identity.initial_run_id,
            &attempt.plan,
        )?;
    }
    Ok(())
}

pub(super) fn apply_persisted_task_base(
    options: &mut CookRequest,
    recipe: &super::cook_recipe::AgentTaskCookRecipe,
) -> Result<()> {
    let task_base_sha = recipe.finalization["task_base_sha"]
        .as_str()
        .ok_or_else(|| Error::internal_json("persisted Cook task base is not a string", None))?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == options.identity.initial_run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "initial Cook recipe is missing its materialized attempt",
                Some(options.identity.initial_run_id.clone()),
                None,
            )
        })?;
    options.workspace.task_base_sha = Some(task_base_sha.to_string());
    options.identity.initial_plan = attempt.plan.clone();
    Ok(())
}

pub(super) fn pin_cook_workspace_base_at(
    options: &mut CookRequest,
    workspace: &Path,
) -> Result<()> {
    if options.identity.initial_plan.metadata["cook_workspace_base"]["sha"].is_string() {
        return Ok(());
    }
    #[cfg(test)]
    if let Some(counter) = WORKSPACE_BASE_CAPTURE_HOOK
        .lock()
        .expect("workspace base capture counter")
        .clone()
        .filter(|counter| counter.workspace == workspace)
    {
        counter.count.fetch_add(1, Ordering::SeqCst);
    }
    let declared = &options.finalization.base;
    let pinned = match homeboy_core::worktree::convergence::retry_fetch(|| {
        crate::agent_task_promotion::capture_declared_base(workspace, Some(declared))
    }) {
        Ok(Some(base)) => CookWorkspaceBasePin {
            base: base.base,
            sha: base.sha,
            provenance: "origin_ls_remote",
            remote_freshness: "checked",
        },
        Ok(None) => return Ok(()),
        // The controller's authenticated transport is not the only authority
        // for a base that is already present locally. A previously fetched
        // remote-tracking ref names the exact commit this run would have
        // pinned, so admit it rather than terminalizing with zero provider
        // executions. Its unverified freshness is recorded, not hidden.
        Err(error) if error.retryable == Some(true) => {
            match local_tracking_cook_base(workspace, declared) {
                Some(pin) => pin,
                None => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    // The destination's starting HEAD is a diff boundary, not the declared
    // remote base. Preserve it while recording the base's independent identity.
    options
        .workspace
        .task_base_sha
        .get_or_insert_with(|| pinned.sha.clone());
    options.identity.initial_plan.metadata["cook_workspace_base"] = serde_json::json!({
        "schema": "homeboy/cook-workspace-base/v1",
        "base": pinned.base,
        "sha": pinned.sha,
        "provenance": pinned.provenance,
        "remote_freshness": pinned.remote_freshness,
    });
    Ok(())
}

pub(super) struct CookWorkspaceBasePin {
    base: String,
    sha: String,
    provenance: &'static str,
    remote_freshness: &'static str,
}

/// Resolve a declared base from the checkout's existing remote-tracking ref.
/// This reads only local objects, so it stays available when the controller
/// cannot authenticate to the remote.
pub(super) fn local_tracking_cook_base(
    workspace: &Path,
    base: &str,
) -> Option<CookWorkspaceBasePin> {
    let reference = format!("refs/remotes/origin/{base}");
    let sha = homeboy_core::git::output_optional(
        workspace,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )?;
    let sha = sha.trim();
    (sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
        CookWorkspaceBasePin {
            base: base.to_string(),
            sha: sha.to_string(),
            provenance: "local_remote_tracking_ref",
            remote_freshness: "unverified",
        }
    })
}

pub(super) fn native_cook_destination_record(
    options: &CookRequest,
) -> Result<Option<homeboy_core::worktree::TaskWorktreeRecord>> {
    let handle = &options.workspace.to_worktree;
    if Path::new(handle).is_absolute() || handle.contains(std::path::MAIN_SEPARATOR) {
        return Ok(None);
    }
    Ok(
        match homeboy_core::worktree::resolve_workspace_ref_if_present(handle)? {
            Some(homeboy_core::worktree::WorkspaceRefRecord::Task(record)) => Some(record),
            _ => None,
        },
    )
}

pub(super) fn preflight_cook_workspace_base_ancestry_with_provider(
    target: &Path,
    options: &CookRequest,
    adopted_dirty_candidate: bool,
    tracked_candidate: bool,
) -> Result<Option<CookWorkspaceBaseValidation>> {
    crate::agent_task_promotion::destination::check(target, Some(&options.identity.cook_id))?;
    // Explicit CWDs are caller-owned and must remain untouched. Resolve their
    // declared ref into an isolated scheduler snapshot instead of treating the
    // persisted pin as authority to converge the caller's checkout.
    let prepared_base = options
        .identity
        .initial_plan
        .metadata
        .get("cook_prepared_base_sha")
        .and_then(Value::as_str);
    if cook_uses_explicit_cwd_workspace(options) {
        if let Some(prepared_base) = prepared_base {
            let prepared_base =
                materialize_prepared_cook_base(target, &options.finalization.base, prepared_base)?;
            return preflight_cook_workspace_resolved_base_ancestry(
                target,
                target,
                &options.finalization.base,
                &options.finalization.base,
                &prepared_base,
                adopted_dirty_candidate,
                options
                    .identity
                    .initial_plan
                    .tasks
                    .first()
                    .and_then(|task| task.workspace.task_url.as_deref()),
            )
            .map(|snapshot| snapshot.map(CookWorkspaceBaseValidation::Snapshot));
        }
    }
    let base = if cook_uses_explicit_cwd_workspace(options) {
        &options.finalization.base
    } else {
        options
            .identity
            .initial_plan
            .metadata
            .pointer("/cook_workspace_base/sha")
            .and_then(Value::as_str)
            .unwrap_or(&options.finalization.base)
    };
    if tracked_candidate {
        // Authentication happened before this call. Neither convergence nor a
        // scheduler snapshot may replace the gate-verified candidate's HEAD.
        return Ok(None);
    }
    let native_owned = matches!(
        native_cook_destination_record(options)?,
        Some(record)
            if record.state == homeboy_core::worktree::TaskWorktreeState::Active
                && std::fs::canonicalize(&record.worktree_path).ok().as_deref()
                    == std::fs::canonicalize(target).ok().as_deref()
    );
    if !cook_uses_explicit_cwd_workspace(options) && native_owned {
        // This is the sole mutation boundary before provider execution. Preview
        // classifies only; caller-owned checkouts retain snapshot semantics.
        let Some(pin) = options
            .identity
            .initial_plan
            .metadata
            .pointer("/cook_workspace_base/sha")
            .and_then(Value::as_str)
        else {
            // Base capture owns resolution. Converge only after it has pinned
            // the immutable base, not in the earlier read-only preflight.
            return Ok(None);
        };
        crate::agent_task_promotion::destination::before_reset(target, &options.identity.cook_id)?;
        let resolved = materialize_prepared_cook_base(target, &options.finalization.base, pin)?;
        let issue_owned = options
            .identity
            .initial_plan
            .tasks
            .first()
            .is_some_and(|task| {
                task.workspace
                    .task_url
                    .as_deref()
                    .is_some_and(|url| url.contains("/issues/"))
            });
        homeboy_core::worktree::convergence::converge_destination(
            target,
            &resolved,
            adopted_dirty_candidate,
            issue_owned,
            options
                .identity
                .initial_plan
                .tasks
                .first()
                .and_then(|task| task.workspace.task_url.as_deref()),
        )?;
        // An issue-owned attempt that adopts no earlier candidate must start
        // from the exact pinned base and a clean tree (#16292).
        if issue_owned && !adopted_dirty_candidate {
            super::cook_attempt_workspace::assert_attempt_workspace_at_base(target, &resolved)?;
        }
        return Ok(Some(CookWorkspaceBaseValidation::Convergence(
            serde_json::json!({
                "schema": "homeboy/cook-workspace-base-convergence/v1",
                "planned_base_sha": resolved,
                "provider_id": "native",
                "handle": options.workspace.to_worktree,
                "path": target,
            }),
        )));
    }
    preflight_cook_workspace_base_ancestry(
        target,
        &options.finalization.base,
        base,
        adopted_dirty_candidate,
        options
            .identity
            .initial_plan
            .tasks
            .first()
            .and_then(|task| task.workspace.task_url.as_deref()),
    )
    .map(|snapshot| snapshot.map(CookWorkspaceBaseValidation::Snapshot))
}

/// A candidate diff is meaningful only when its destination contains the
/// resolved base. Otherwise base-only files appear to be provider changes.
/// A clean destination that is strictly behind is not mutated. Instead, the
/// returned immutable base pin authorizes the scheduler's isolated attempt
/// workspace. This keeps provider-managed destinations under their owner while
/// ensuring provider output is based on the observed remote base.
pub(super) fn preflight_cook_workspace_base_ancestry(
    target: &Path,
    declared_base: &str,
    base: &str,
    adopted_dirty_candidate: bool,
    task_url: Option<&str>,
) -> Result<Option<Value>> {
    // A provider-owned Git destination can intentionally have no origin. When
    // origin is configured, resolve its authoritative base directly instead of
    // trusting a potentially absent shallow or single-branch tracking ref.
    let origin = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(target)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !origin.status.success() {
        return Ok(None);
    }
    let (declared_base, resolved_base) =
        if base.len() == 40 && base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            (
                declared_base.to_string(),
                materialize_prepared_cook_base(target, declared_base, base)?,
            )
        } else {
            let Some(base) = homeboy_core::worktree::convergence::retry_fetch(|| {
                crate::agent_task_promotion::capture_declared_base(target, Some(base))
            })?
            else {
                return Ok(None);
            };
            (base.base, base.sha)
        };
    preflight_cook_workspace_resolved_base_ancestry(
        target,
        target,
        base,
        &declared_base,
        &resolved_base,
        adopted_dirty_candidate,
        task_url,
    )
}

pub(super) fn materialize_prepared_cook_base(
    target: &Path,
    declared_base: &str,
    base_sha: &str,
) -> Result<String> {
    homeboy_core::worktree::convergence::retry_fetch(|| {
        crate::agent_task_promotion::materialize_declared_base_sha(target, declared_base, base_sha)
    })?;
    Ok(homeboy_core::git::run_git(
        target,
        &["rev-parse", "--verify", &format!("{base_sha}^{{commit}}")],
        "verify prepared Cook base",
    )?
    .trim()
    .to_string())
}

/// Resolve one authoritative Cook base and classify the destination against
/// that same immutable identity. Preview serializes the SHA into replay so
/// execution cannot refresh the branch to a different candidate contract.
pub fn prepare_cook_workspace_base(
    target: &Path,
    base: &str,
    task_url: Option<&str>,
) -> Result<CookBasePreparation> {
    let origin = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(target)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !origin.status.success() {
        return Ok(CookBasePreparation {
            schema: "homeboy/cook-base-preparation/v1",
            declared_base: base.to_string(),
            base_sha: None,
            provenance: "origin_unavailable",
            remote_freshness: "deferred",
            topology: None,
        });
    }
    let origin = String::from_utf8_lossy(&origin.stdout).trim().to_string();
    let graph = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create isolated Cook base preparation repository".to_string()),
        )
    })?;
    homeboy_core::git::run_git(
        graph.path(),
        &["init", "--bare", "--quiet"],
        "initialize isolated Cook base preparation repository",
    )?;
    homeboy_core::git::run_git(
        graph.path(),
        &["remote", "add", "origin", &origin],
        "configure isolated Cook base preparation origin",
    )?;
    let common_dir = homeboy_core::git::run_git(
        target,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "resolve Cook destination object database",
    )?;
    let alternates = graph.path().join("objects/info/alternates");
    std::fs::write(&alternates, format!("{}/objects\n", common_dir.trim())).map_err(|error| {
        Error::internal_io(error.to_string(), Some(alternates.display().to_string()))
    })?;
    let Some(resolved) =
        tolerate_retryable_cook_base_resolution(homeboy_core::worktree::convergence::retry_fetch(
            || crate::agent_task_promotion::capture_declared_base(graph.path(), Some(base)),
        ))?
    else {
        return Ok(deferred_cook_base_preparation(base));
    };
    let topology = preflight_cook_workspace_resolved_base_ancestry(
        target,
        graph.path(),
        base,
        &resolved.base,
        &resolved.sha,
        false,
        task_url,
    )?;
    Ok(CookBasePreparation {
        schema: "homeboy/cook-base-preparation/v1",
        declared_base: resolved.base,
        base_sha: Some(resolved.sha),
        provenance: "origin_ls_remote",
        remote_freshness: "checked",
        topology,
    })
}

pub(super) fn deferred_cook_base_preparation(base: &str) -> CookBasePreparation {
    CookBasePreparation {
        schema: "homeboy/cook-base-preparation/v1",
        declared_base: base.to_string(),
        base_sha: None,
        provenance: "base_unresolved",
        remote_freshness: "deferred",
        topology: None,
    }
}

/// Preview must not present an unverified fallback as an admitted base. A
/// retryable authoritative probe instead produces the same deferred contract as
/// an unavailable declared base, so replay cannot silently claim parity.
pub(super) fn tolerate_retryable_cook_base_resolution<T>(
    result: Result<Option<T>>,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if error.retryable == Some(true) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn preflight_cook_workspace_resolved_base_ancestry(
    target: &Path,
    graph: &Path,
    base: &str,
    declared_base: &str,
    resolved_base: &str,
    adopted_dirty_candidate: bool,
    task_url: Option<&str>,
) -> Result<Option<Value>> {
    let AgentTaskPromotionCandidate::Git { fingerprint } =
        candidate_fingerprint(target.to_string_lossy().as_ref())?
    else {
        return Ok(None);
    };
    let counts = homeboy_core::git::run_git(
        graph,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{resolved_base}...{}", fingerprint.head),
        ],
        "compare Cook destination to resolved base",
    )?;
    let Some((behind, ahead)) = counts
        .split_whitespace()
        .collect::<Vec<_>>()
        .get(..2)
        .and_then(|parts| Some((parts[0].parse::<u64>().ok()?, parts[1].parse::<u64>().ok()?)))
    else {
        return Err(Error::git_command_failed(
            "could not classify Cook destination ancestry against resolved base".to_string(),
        ));
    };
    let status = homeboy_core::git::run_git(
        target,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "verify Cook destination cleanliness before base snapshot",
    )?;
    let dirty_paths = status
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.strip_prefix("?? ").unwrap_or(entry))
        .collect::<Vec<_>>();
    if !dirty_paths.is_empty() && !adopted_dirty_candidate {
        let mut error = Error::validation_invalid_argument(
            "workspace",
            format!(
                "Cook cannot attribute uncommitted destination changes: {}. Preserve them with `git -C {} stash push --include-untracked -m 'before Cook retry'`, then retry the Cook",
                dirty_paths.join(", "),
                quote_arg(target.to_string_lossy().as_ref())
            ),
            Some(target.display().to_string()),
            None,
        );
        error.details["workspace_base_ancestry"] = serde_json::json!({
            "schema": "homeboy/cook-workspace-base-ancestry/v1",
            "base": declared_base,
            "resolved_base": resolved_base,
            "destination_head": fingerprint.head,
            "direction": "dirty",
            "base_only_commits": behind,
            "candidate_only_commits": ahead,
            "next_action": "clean_destination_before_provider",
        });
        error.details["dirty_candidate_adoption"] = serde_json::json!({
            "workspace": target,
            "reason": "first_provider_admission",
        });
        return Err(error);
    }
    if behind == 0 {
        // An unchanged or ahead-only candidate already has a meaningful base
        // relative to its source HEAD; preserve its existing dispatch path.
        return Ok(None);
    }
    if ahead == 0 {
        // Managed destinations converge at admission. Read-only preview and
        // caller-owned checkouts project the immutable scheduler snapshot.
        return Ok(Some(serde_json::json!({
            "schema": "homeboy/cook-workspace-base-snapshot/v1",
            "base": declared_base,
            "resolved_base": resolved_base,
            "destination_head": fingerprint.head,
            "base_only_commits": behind,
            "candidate_only_commits": ahead,
            "mode": "isolated_attempt_snapshot",
        })));
    }
    let direction = "diverged";
    let branch = homeboy_core::git::run_git(
        target,
        &["symbolic-ref", "--short", "HEAD"],
        "identify existing candidate branch",
    )?;
    if homeboy_core::worktree::convergence::is_pr_rework(target, branch.trim(), task_url)? {
        return Ok(None);
    }
    homeboy_core::worktree::convergence::require_issue_destination_without_pr(
        target,
        branch.trim(),
    )?;
    let mut error = homeboy_core::worktree::convergence::diverged_route(branch.trim(), task_url);
    error.details["workspace_base_ancestry"] = serde_json::json!({
        "schema": "homeboy/cook-workspace-base-ancestry/v1",
        "base": base,
        "resolved_base": resolved_base,
        "destination_head": fingerprint.head,
        "direction": direction,
        "base_only_commits": behind,
        "candidate_only_commits": ahead,
        "next_action": "cook_pr_url",
    });
    // Divergence requires an operator merge/rebase; retrying without one can
    // never change the outcome, so it is deterministic and consumes no
    // transport retries (#14699).
    Err(error)
}

/// Admit a locally committed, unpushed provider checkout only when Cook can
/// bind the exception to the exact clean, issue-owned linked worktree and the
/// task's immutable base. The provider resolver consumes the existing
/// path+HEAD capability; no broader unpushed exception is introduced here.
pub(super) fn preflight_initial_cook_workspace_provider(options: &CookRequest) -> Result<()> {
    if cook_uses_explicit_cwd_workspace(options)
        || Path::new(&options.workspace.to_worktree).is_dir()
    {
        return Ok(());
    }
    homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(
        &options.workspace.to_worktree,
    )?
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "Cook requires an active native worktree",
            Some(options.workspace.to_worktree.clone()),
            None,
        )
    })?;
    Ok(())
}

pub(crate) fn cook_uses_explicit_cwd_workspace(options: &CookRequest) -> bool {
    options
        .identity
        .initial_plan
        .tasks
        .first()
        .is_some_and(|task| {
            task.metadata
                .pointer("/worktree_provision/kind")
                .and_then(Value::as_str)
                == Some("explicit_cwd")
        })
}

pub(super) fn admit_explicit_cook_workspace_before_provider(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
) -> Result<()> {
    if !cook_uses_explicit_cwd_workspace(options)
        || cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)?
    {
        return Ok(());
    }
    let source = options
        .workspace
        .source_worktree_path
        .as_deref()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "explicit Cook workspace is missing its source path",
                Some(options.workspace.to_worktree.clone()),
                None,
            )
        })?;
    let source = std::fs::canonicalize(source).map_err(|error| {
        Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    let status = homeboy_core::git::run_git(
        &source,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "verify explicitly targeted Cook checkout cleanliness",
    )?;
    let dirty_paths = status
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.strip_prefix("?? ").unwrap_or(entry))
        .collect::<Vec<_>>();
    if !dirty_paths.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            "explicit Cook checkout must be clean before its first provider execution",
            Some(options.workspace.to_worktree.clone()),
            None,
        );
        error.details["dirty_candidate_adoption"] = serde_json::json!({
            "workspace": source,
            "reason": "first_provider_admission",
        });
        return Err(error);
    }
    if lifecycle_store.record_exists(run_id)? {
        lifecycle_store.record_metadata_value(
            run_id,
            "explicit_cwd_cleanliness_admission",
            serde_json::json!({
                "schema": "homeboy/cook-explicit-cwd-cleanliness/v1",
                "workspace": source,
                "provider_executions_before_admission": 0,
            }),
        )?;
    }
    Ok(())
}

/// A consumed provider execution is the durable boundary after which provider
/// candidate changes are expected to remain in the explicit checkout.
pub(super) fn cook_has_provider_execution_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<bool> {
    let index = match lifecycle_store.read_cook_index(cook_id) {
        Ok(index) => index,
        Err(_) => return Ok(false),
    };
    if index.attempts.is_empty() {
        return Ok(false);
    }
    for attempt in index.attempts {
        if lifecycle_store
            .read_record(&attempt.run_id)
            .ok()
            .and_then(|record| {
                record.metadata["provider_executions_consumed"]
                    .as_u64()
                    .or_else(|| {
                        record.metadata["provider_executions"]
                            .as_array()
                            .map(|executions| executions.len() as u64)
                    })
            })
            .is_some_and(|executions| executions > 0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A Cook whose destination is owned by another live Cook yields to it and
/// follows its result (#18307).
pub(super) fn supersede_by_live_destination_owner(
    stores: CookStores<'_>,
    options: &CookRequest,
) -> Result<Option<AgentTaskRunResult<AgentTaskCookReport>>> {
    let lifecycle_store = stores.lifecycle;
    let Some(owner) = super::cook_prior_candidate::require_destination_free_of_live_cook(
        lifecycle_store,
        options,
    )?
    else {
        return Ok(None);
    };
    lifecycle_store.record_metadata_value(
        &options.identity.initial_run_id,
        "superseded_by_active_cook",
        owner.clone(),
    )?;
    // The attempt never executes: settle it so no queued record is
    // left for status, sweeps, or upgrade admission to wait on.
    agent_task_lifecycle::cancel_run_in_store(
        lifecycle_store,
        &options.identity.initial_run_id,
        Some("superseded_by_active_cook"),
    )?;
    Ok(Some(stores.report(CookReportInput {
        cook_id: options.identity.cook_id.clone(),
        status: homeboy_core::cook_status::CookStatus::SupersededByActiveCook,
        disposition: CookDisposition::Terminal,
        attempts: Vec::new(),
        finalization: None,
        stop_reason: Some(format!(
            "destination owned by active Cook {}",
            owner["cook_id"].as_str().unwrap_or_default()
        )),
        exit_code: 0,
        invocation_latest_run_id: Some(&options.identity.initial_run_id),
    })))
}

pub(super) fn cook_workspace_lookup_pending(plan: &AgentTaskPlan) -> bool {
    let provision = plan.metadata.get("cook_provision").or_else(|| {
        plan.tasks
            .first()
            .and_then(|task| task.metadata.get("worktree_provision"))
    });
    matches!(
        provision
            .and_then(|provision| provision.get("action"))
            .and_then(Value::as_str),
        Some("lookup_pending" | "attestation_pending")
    )
}

pub(super) fn materialize_pending_cook_workspace(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &mut CookRequest,
) -> Result<()> {
    if options.workspace.task_base_sha.is_none() {
        options.workspace.task_base_sha = options
            .identity
            .initial_plan
            .metadata
            .pointer("/cook_workspace_base/sha")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let provider = homeboy_core::worktree_provider::NativeWorktreeProvider;
    let destination = match provider.resolve(&options.workspace.to_worktree)? {
        Some(ownership) => {
            homeboy_core::worktree_provider::WorktreeProvisionDestination { ownership }
        }
        None => provision_pending_cook_workspace(lifecycle_store, options)?.destination,
    };
    if destination.ownership.handle != options.workspace.to_worktree {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "worktree provider did not resolve the declared exact Cook handle",
            Some(options.workspace.to_worktree.clone()),
            None,
        ));
    }
    let target = PathBuf::from(&destination.ownership.path);
    let target = std::fs::canonicalize(&target).map_err(|error| {
        Error::internal_io(error.to_string(), Some(target.display().to_string()))
    })?;
    validate_pending_cook_repository_identity(&options.identity.initial_plan, &target)?;
    bind_materialized_cook_component_workspace(
        &mut options.identity.initial_plan,
        &target,
        options
            .gates
            .gate_environment
            .admitted_component_id
            .as_deref(),
    )?;
    // Deferred provider materialization has no checkout at initial recipe
    // persistence. Capture and persist this immutable boundary before Cook can
    // admit or dispatch the materialized destination.
    pin_cook_workspace_base_at(options, &target)?;
    for task in &mut options.identity.initial_plan.tasks {
        task.workspace.root = Some(target.display().to_string());
        task.metadata["cook_workspace_identity"] =
            crate::agent_task_workspace_identity::attest_workspace(&target)?;
    }
    options.identity.initial_plan.metadata["cook_provision"]["provider_identity"] =
        Value::String("native".to_string());
    options.identity.initial_plan.metadata["cook_provision"]["action"] =
        Value::String("existing".to_string());
    options.workspace.source_worktree_path = Some(target);
    agent_task_lifecycle::persist_controller_plan_in_store(
        lifecycle_store,
        &options.identity.initial_run_id,
        &options.identity.initial_plan,
    )
}

/// The registered component identity Cook resolved and durably recorded at
/// admission (`cook_repository_identity`), when the checkout maps to a real
/// registered component rather than an ad-hoc/synthetic repository target.
///
/// This is the same identity a shared-repository worktree's ambiguous
/// checkout otherwise cannot disambiguate on its own (#14725): callers that
/// need to resolve a review profile or component-scoped path for an
/// already-admitted Cook should prefer this over re-deriving from the bare
/// checkout path.
/// Reject a Cook whose destination needs a dependency tool this controller
/// lacks, before any provider runs (#15364).
pub(super) fn dependency_tool_preflight_error(options: &CookRequest) -> Result<Option<Error>> {
    let Some(gate_workspace) = super::cook_promotion::component_workspace_path(options)?
        .or_else(|| options.workspace.source_worktree_path.clone())
    else {
        return Ok(None);
    };
    if !gate_workspace.is_dir() {
        return Ok(None);
    }
    let component_id = options
        .gates
        .gate_environment
        .admitted_component_id
        .clone()
        .or_else(|| cook_repository_identity_component_id(&options.identity.initial_plan));
    let missing =
        homeboy_core::deps::missing_dependency_tools(&gate_workspace, component_id.as_deref());
    if missing.is_empty() {
        return Ok(None);
    }
    let summary = missing
        .iter()
        .map(|tool| {
            format!(
                "`{}` (needed by `{}` in {} for: {})",
                tool.program,
                tool.provider_id,
                tool.package_root,
                tool.install_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let mut programs = missing
        .iter()
        .map(|tool| tool.program.clone())
        .collect::<Vec<_>>();
    programs.dedup();
    let mut error = Error::validation_invalid_argument(
        "dependency_tools",
        format!(
            "this controller is missing dependency tool(s) its gates need: {summary}"
        ),
        Some(gate_workspace.display().to_string()),
        Some(vec![format!(
            "Install {} where the Homeboy controller runs gates (or put it on PATH), then start Cook again. No provider attempt was spent.",
            programs.join(", ")
        )]),
    );
    error.details["missing_dependency_tools"] = serde_json::to_value(&missing)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(Some(error))
}

pub fn cook_repository_identity_component_id(plan: &AgentTaskPlan) -> Option<String> {
    admitted_component_id(plan.metadata.get("cook_repository_identity"))
}

/// The explicitly registered component identity recorded at Cook admission. Callers that
/// already have this identity must not re-resolve it from a worktree path.
pub fn admitted_component_id(identity: Option<&Value>) -> Option<String> {
    let identity = identity?;
    let component_id = identity.get("component_id").and_then(Value::as_str)?;
    let component_registered = identity
        .get("component_registered")
        .and_then(Value::as_bool)?;
    if !component_registered || component_id.is_empty() {
        return None;
    }
    Some(component_id.to_string())
}

/// Cook selects repository-owned registration before project attachments.
pub fn cook_registered_component_by_id(
    component_id: &str,
) -> Result<Option<homeboy_core::component::Component>> {
    if let Some(component) =
        homeboy_core::component::inventory::registered_primary_by_id(component_id)?
    {
        return Ok(Some(component));
    }
    homeboy_core::component::registered_by_id(component_id)
}

/// Resolve a registered component inside a materialized Cook repository.
/// Preserve the caller's root spelling after verifying canonical containment.
pub(super) fn resolve_cook_component_workspace(
    component_id: &str,
    repository_root: &Path,
    component_cwd: Option<&str>,
) -> Result<PathBuf> {
    let Some(component) = cook_registered_component_by_id(component_id)? else {
        return Err(Error::validation_invalid_argument(
            "component workspace",
            format!("configured component `{component_id}` is no longer registered"),
            Some(component_id.to_string()),
            None,
        ));
    };
    let canonical_root = repository_root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(repository_root.display().to_string()),
        )
    })?;
    // A component ID is not its owning repository name. Validate the owner by
    // remote, then project the component's relative path into that checkout.
    if let Some(expected_url) = component.remote_url.as_deref() {
        let remote = homeboy_core::git::resolve_default_remote(&canonical_root);
        let actual_url = homeboy_core::git::remote_url(&canonical_root, &remote);
        let expected = cook_remote_identity(expected_url);
        let actual = actual_url.as_deref().and_then(cook_remote_identity);
        if actual_url.as_deref() != Some(expected_url) && (expected.is_none() || actual != expected)
        {
            return Err(Error::validation_invalid_argument(
                "component workspace",
                format!(
                    "Cook component `{component_id}` owning repository `{expected_url}` does not match destination repository `{}`",
                    actual_url.as_deref().unwrap_or("<missing remote>")
                ),
                Some(repository_root.display().to_string()),
                None,
            ));
        }
    }
    let effective = match component_cwd {
        Some(component_cwd) => homeboy_core::resolve_contained_local_path(
            &canonical_root,
            component_cwd,
            "component_cwd",
        )?,
        None => homeboy_core::component::resolution::rebase_component_path_to_checkout(
            &component,
            &canonical_root,
        ),
    };
    if !effective.is_dir() {
        return Err(Error::validation_invalid_argument(
            "component workspace",
            format!(
                "resolved component `{component_id}` of owning repository `{}` is not present in materialized Cook workspace",
                component.remote_url.as_deref().unwrap_or(&component.local_path)
            ),
            Some(effective.display().to_string()),
            None,
        ));
    }
    let effective = effective.canonicalize().map_err(|error| {
        Error::internal_io(error.to_string(), Some(effective.display().to_string()))
    })?;
    let component_cwd = effective.strip_prefix(&canonical_root).map_err(|_| {
        Error::validation_invalid_argument(
            "component workspace",
            "resolved component workspace escapes the materialized repository root",
            Some(effective.display().to_string()),
            None,
        )
    })?;
    Ok(repository_root.join(component_cwd))
}

pub(super) fn cook_remote_identity(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = url.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    (!host.is_empty() && !path.is_empty()).then(|| {
        format!(
            "{}/{}",
            host.to_ascii_lowercase(),
            path.to_ascii_lowercase()
        )
    })
}

/// Bind component-relative execution and gate evidence through the same resolver
/// for CLI plans, fanout plans, and deferred materialization.
pub fn bind_materialized_cook_component_workspace(
    plan: &mut AgentTaskPlan,
    repository_root: &Path,
    selected_component_id: Option<&str>,
) -> Result<()> {
    if plan.metadata["caller_workspace"].is_null() {
        if let Some(repository) = plan.metadata["repo"].as_str() {
            plan.metadata["caller_workspace"] = serde_json::json!({
                "repository": repository,
                "working_directory": repository_root,
            });
        }
    }
    let Some(component_id) = selected_component_id
        .map(str::to_string)
        .or_else(|| cook_repository_identity_component_id(plan))
    else {
        return Ok(());
    };
    let effective = resolve_cook_component_workspace(
        &component_id,
        repository_root,
        plan.metadata
            .pointer("/cook_repository_identity/component_cwd")
            .and_then(Value::as_str),
    )?;
    let component_cwd = effective
        .strip_prefix(repository_root)
        .expect("resolved contained workspace");
    if component_cwd.as_os_str().is_empty() {
        return Ok(());
    }
    plan.metadata["gate_workspace"] = serde_json::json!({
        "requested_cwd": repository_root,
        "effective_cwd": effective,
        "component_cwd": component_cwd,
        "component_id": component_id,
    });
    for task in &mut plan.tasks {
        if !task.executor.config.is_object() {
            task.executor.config = serde_json::json!({});
        }
        task.executor.config["component_cwd"] = Value::String(component_cwd.display().to_string());
    }
    Ok(())
}

/// A deferred native lookup can prove absence only after Cook owns a durable
/// identity. It then creates and records the native worktree lifecycle.
pub(super) fn provision_pending_cook_workspace(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &mut CookRequest,
) -> Result<homeboy_core::worktree_provider::WorktreeProvision> {
    let intent = &options.identity.initial_plan.metadata["cook_provision"]["provision_intent"];
    let required = |field: &str| {
        intent.get(field).and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| {
            Error::validation_missing_argument(vec![format!(
                "--{field} is required to create missing provider worktree `{}` after durable Cook admission",
                options.workspace.to_worktree
            )])
        })
    };
    let create_intent = homeboy_core::worktree_provider::WorktreeProvisionIntent {
        handle: options.workspace.to_worktree.clone(),
        repo: required("repo")?.to_string(),
        base: required("base")?.to_string(),
        head: required("head")?.to_string(),
        task_url: intent
            .get("task_url")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
    };
    let lifecycle = options
        .identity
        .initial_plan
        .metadata
        .pointer("/cook_provision/lifecycle_intent")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_provision.lifecycle_intent",
                "native Cook provisioning requires a durable lifecycle intent",
                Some(options.workspace.to_worktree.clone()),
                None,
            )
        })?;
    let purpose = lifecycle
        .get("purpose")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("agent_task_cook")
        .to_string();
    let cleanup_policy = match lifecycle.get("cleanup_policy").and_then(Value::as_str) {
        None | Some("remove_on_success") => {
            homeboy_core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess
        }
        Some("preserve_on_failure") => {
            homeboy_core::worktree_provider::WorktreeCleanupPolicy::PreserveOnFailure
        }
        Some(value) => {
            return Err(Error::validation_invalid_argument(
                "cook_provision.lifecycle_intent.cleanup_policy",
                "Cook provider lifecycle cleanup policy is invalid",
                Some(value.to_string()),
                None,
            ));
        }
    };
    // Bind the pending lifecycle intent to its stable durable run before the
    // native mutation so continuation observes the same ownership.
    options.identity.initial_plan.metadata["cook_provision"]["lifecycle_intent"] = serde_json::json!({
        "purpose": purpose,
        "owner_run_ref": options.identity.initial_run_id,
        "cleanup_policy": match cleanup_policy {
            homeboy_core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess => "remove_on_success",
            homeboy_core::worktree_provider::WorktreeCleanupPolicy::PreserveOnFailure => "preserve_on_failure",
        },
    });
    agent_task_lifecycle::persist_controller_plan_in_store(
        lifecycle_store,
        &options.identity.initial_run_id,
        &options.identity.initial_plan,
    )?;
    let provision = homeboy_core::worktree_provider::ensure_worktree_provision(
        &create_intent,
        &homeboy_core::worktree_provider::WorktreeProvisionLifecycle {
            purpose,
            owner_run_ref: options.identity.initial_run_id.clone(),
            cleanup_policy,
        },
    )?;
    crate::agent_task_promotion::destination::record(
        Path::new(&provision.destination.ownership.path),
        &options.identity.cook_id,
    )?;
    Ok(provision)
}

pub(super) fn validate_pending_cook_repository_identity(
    _plan: &AgentTaskPlan,
    _target: &Path,
) -> Result<()> {
    // Native provisioning already binds the component and branch in its durable record.
    Ok(())
}

pub(super) struct TrackedPromotionContinuation {
    path: PathBuf,
    branch: String,
    candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate,
}

/// A gate-failed promotion may be committed by Cook's recovery path before its
/// final push. Accept only one direct child of the recorded pre-promotion HEAD
/// whose tree is the recorded promoted candidate; this proves the commit did
/// not add unrelated work.
pub(super) fn cook_owned_unpushed_destination(
    continuation: &TrackedPromotionContinuation,
) -> Result<bool> {
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
        &continuation.candidate
    else {
        return Ok(false);
    };
    let path = &continuation.path;
    let clean = homeboy_core::git::run_git(
        path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        "verify Cook-owned committed candidate cleanliness",
    )?
    .trim()
    .is_empty();
    if !clean {
        return Ok(false);
    }
    let head = homeboy_core::git::run_git(
        path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolve Cook-owned committed candidate HEAD",
    )?
    .trim()
    .to_string();
    if head == fingerprint.head {
        return Ok(false);
    }
    let parents = homeboy_core::git::run_git(
        path,
        &["rev-list", "--parents", "-n", "1", "HEAD"],
        "resolve Cook-owned committed candidate parents",
    )?
    .split_whitespace()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if parents.len() != 2 {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            "clean unpushed checkout is not a single-parent Cook-owned promoted candidate",
            Some(path.display().to_string()),
            None,
        );
        error.details["workspace"] = serde_json::json!({
            "classification": "workspace.cook_owned_unpushed_commit_mismatch",
            "reason": "merge_commit",
            "owning_layer": "cook",
            "path": path,
            "parents": parents,
        });
        return Err(error);
    }
    let parent = &parents[1];
    let tree = homeboy_core::git::run_git(
        path,
        &["rev-parse", "--verify", "HEAD^{tree}"],
        "resolve Cook-owned committed candidate tree",
    )?
    .trim()
    .to_string();
    if parent != &fingerprint.head || tree != fingerprint.tree {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            "clean unpushed checkout is not the exact one-commit Cook-owned promoted candidate",
            Some(path.display().to_string()),
            None,
        );
        error.details["workspace"] = serde_json::json!({
            "classification": "workspace.cook_owned_unpushed_commit_mismatch",
            "reason": if parent != &fingerprint.head { "divergent_ancestry" } else { "unrelated_unpushed_commit" },
            "owning_layer": "cook",
            "path": path,
            "expected_parent": fingerprint.head,
            "actual_parent": parent,
            "expected_tree": fingerprint.tree,
            "actual_tree": tree,
        });
        return Err(error);
    }
    Ok(true)
}

/// A dirty destination is reusable only for the exact post-apply candidate
/// checkpoint owned by this Cook attempt. Core verifies the supplied baseline
/// during provider resolution; Cook binds it to this attempt's target identity.
pub(super) fn tracked_promotion_continuation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
) -> Result<Option<TrackedPromotionContinuation>> {
    if !lifecycle_store.record_exists(&options.identity.initial_run_id)? {
        return Ok(None);
    }
    if lifecycle_store
        .read_record(&options.identity.initial_run_id)?
        .metadata
        .get("superseded_by_run_id")
        .is_some()
    {
        return Err(Error::validation_invalid_argument(
            "run_id", "This candidate was superseded by a newer Cook; use its preserved Git stash instead of resuming",
            Some(options.identity.initial_run_id.clone()), None,
        ));
    }
    if super::CookDelivery::from_run_metadata(
        &lifecycle_store
            .read_record(&options.identity.initial_run_id)?
            .metadata,
    )
    .is_some_and(|delivery| delivery.has_publication_receipt())
    {
        return Ok(None);
    }
    let Some(promotion) = persisted_promotion_for_attempt_in_store(
        lifecycle_store,
        &options.identity.initial_run_id,
    )?
    else {
        return Ok(None);
    };
    let has_post_apply_checkpoint = ["/post_apply", "/resumed_post_apply_promotion"]
        .into_iter()
        .any(|pointer| promotion.provenance.pointer(pointer) == Some(&Value::Bool(true)));
    let normal_continuation = matches!(
        promotion.status,
        AgentTaskPromotionStatus::VerificationPending | AgentTaskPromotionStatus::Applied
    ) && has_post_apply_checkpoint;
    // A gate-failed candidate remains eligible only when its immutable recipe,
    // indexed attempt, and run record all identify this Cook. This survives a
    // failed controller process without treating caller-controlled route or
    // plan metadata as authorization for dirty-worktree reuse.
    let route_authorized_gate_failure = promotion.status.gate_failed()
        && has_post_apply_checkpoint
        && cook_owns_attempt_in_store(lifecycle_store, options)?;
    if !(normal_continuation || route_authorized_gate_failure) {
        return Ok(None);
    }
    if promotion.to_worktree != options.workspace.to_worktree
        || promotion.target.worktree != options.workspace.to_worktree
    {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination does not match its tracked post-apply promotion",
            Some(options.workspace.to_worktree.clone()),
            None,
        ));
    }
    let path = promotion.target.path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.target.path",
            "Cook continuation requires the tracked post-apply promotion destination path",
            Some(options.identity.initial_run_id.clone()),
            None,
        )
    })?;
    let branch = promotion
        .target
        .branch
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion.target.branch",
                "Cook continuation requires the tracked post-apply promotion destination branch",
                Some(options.identity.initial_run_id.clone()),
                None,
            )
        })?;
    let candidate = promotion
        .provenance
        .get("candidate")
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion.provenance.candidate",
                "Cook continuation requires the tracked post-apply candidate fingerprint",
                Some(options.identity.initial_run_id.clone()),
                None,
            )
        })?;
    let candidate = serde_json::from_value(candidate).map_err(|_| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.provenance.candidate",
            "Cook continuation tracked candidate fingerprint is invalid",
            Some(options.identity.initial_run_id.clone()),
            None,
        )
    })?;
    Ok(Some(TrackedPromotionContinuation {
        path: PathBuf::from(path),
        branch,
        candidate,
    }))
}

pub(super) fn cook_owns_attempt_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
) -> Result<bool> {
    let recipe_store = CookRecipeStore::for_lifecycle_store(lifecycle_store);
    let recipe = match recipe_store.load_recipe(&options.identity.cook_id) {
        Ok(recipe) => recipe,
        Err(_) => return Ok(false),
    };
    let Some(attempt) = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == options.identity.initial_run_id)
    else {
        return Ok(false);
    };
    let recipe_tasks = attempt
        .plan
        .tasks
        .iter()
        .map(|task| task.workspace.task_url.as_deref())
        .collect::<Vec<_>>();
    let option_tasks = options
        .identity
        .initial_plan
        .tasks
        .iter()
        .map(|task| task.workspace.task_url.as_deref())
        .collect::<Vec<_>>();
    if recipe_tasks != option_tasks
        || recipe
            .finalization
            .get("to_worktree")
            .and_then(Value::as_str)
            != Some(options.workspace.to_worktree.as_str())
    {
        return Ok(false);
    }
    let record = lifecycle_store.read_record(&options.identity.initial_run_id)?;
    Ok(record.metadata.get("cook_id").and_then(Value::as_str)
        == Some(options.identity.cook_id.as_str())
        && record.metadata.get("cook_attempt").and_then(Value::as_u64)
            == Some(u64::from(attempt.attempt)))
}

pub(super) fn authenticate_tracked_promotion_continuation(
    target: &std::path::Path,
    continuation: &TrackedPromotionContinuation,
) -> Result<()> {
    let expected_path = std::fs::canonicalize(&continuation.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(continuation.path.display().to_string()),
        )
    })?;
    if target != expected_path {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination path differs from its tracked post-apply promotion",
            Some(target.display().to_string()),
            None,
        ));
    }
    let branch = homeboy_core::git::current_branch(target).ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination has no branch",
            Some(target.display().to_string()),
            None,
        )
    })?;
    if branch != continuation.branch {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination branch differs from its tracked post-apply promotion",
            Some(target.display().to_string()),
            None,
        ));
    }
    let actual =
        crate::agent_task_promotion::candidate_fingerprint(target.to_string_lossy().as_ref())?;
    if actual != continuation.candidate && !cook_owned_unpushed_destination(continuation)? {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook continuation destination differs from its exact tracked post-apply candidate",
            Some(target.display().to_string()),
            None,
        ));
    }
    Ok(())
}

pub(super) fn record_active_cook_worktree_warning(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
) -> Result<()> {
    let Some(source) = options.workspace.source_worktree_path.as_deref() else {
        return Ok(());
    };
    let target = std::fs::canonicalize(source).map_err(|error| {
        Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    let mut active = lifecycle_store
        .read_active_records()?
        .0
        .into_iter()
        .filter(|record| record.run_id != options.identity.initial_run_id)
        .filter(|record| record.metadata.get("cook_id").is_some())
        .filter_map(|record| {
            let plan = agent_task_lifecycle::load_plan_for_execution_in_store(
                lifecycle_store,
                &record.run_id,
            )
            .ok()?;
            let matches_target = plan.tasks.iter().any(|task| {
                task.workspace
                    .root
                    .as_deref()
                    .and_then(|root| std::fs::canonicalize(root).ok())
                    .as_ref()
                    == Some(&target)
            });
            matches_target.then_some(record)
        })
        .collect::<Vec<_>>();
    active.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    if active.is_empty() {
        return Ok(());
    }
    let run_ids = active
        .iter()
        .map(|record| record.run_id.clone())
        .collect::<Vec<_>>();
    lifecycle_store.record_metadata_value(
        &options.identity.initial_run_id,
        "cook_active_worktree_warning",
        serde_json::json!({
            "schema": "homeboy/cook-active-worktree-warning/v1",
            "canonical_worktree": target,
            "active_run_ids": run_ids,
            "status_commands": active.iter().map(|record| {
                cook_recovery_command_with_prefix(
                    &cook_recovery_command_prefix_for_record(record),
                    &["status", &record.run_id],
                )
            }).collect::<Vec<_>>(),
        }),
    )?;
    Ok(())
}

/// A baseline is immutable provider evidence. Continue provider and controller
/// work from the active task-worktree named by the recipe, never from that
/// temporary baseline path.
pub(super) fn rebind_baseline_continuation_workspace(
    options: &mut CookRequest,
    continuation_plan: &AgentTaskPlan,
) -> Result<()> {
    let baseline_bound_continuation = continuation_plan.tasks.first().is_some_and(|task| {
        task.inputs
            .pointer("/cook_loop/artifact_provenance/source_run_id")
            .is_some()
            || task
                .metadata
                .get("cook_initial_candidate_baseline")
                .is_some()
    });
    if !baseline_bound_continuation {
        if continuation_plan
            .metadata
            .pointer("/cook_provision/action")
            .and_then(Value::as_str)
            == Some("existing")
        {
            if let Some(root) = continuation_plan
                .tasks
                .first()
                .and_then(|task| task.workspace.root.as_deref())
            {
                options.workspace.source_worktree_path = Some(root.into());
            }
        }
        return Ok(());
    }
    if std::path::Path::new(&options.workspace.to_worktree).is_dir() {
        options.workspace.source_worktree_path = Some(options.workspace.to_worktree.clone().into());
    } else if let Some(worktree) =
        homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(
            &options.workspace.to_worktree,
        )?
    {
        options.workspace.source_worktree_path = Some(worktree.path);
    }
    Ok(())
}

/// Canonicalize independently owned workspace paths without turning a candidate
/// source checkout into the destination selected for provider execution.
pub(super) fn canonicalize_cook_provider_workspace(options: &mut CookRequest) -> Result<()> {
    if canonicalize_native_cook_workspace(options)? {
        return Ok(());
    }
    if let Some(source) = options.workspace.source_worktree_path.as_deref() {
        options.workspace.source_worktree_path =
            Some(std::fs::canonicalize(source).map_err(|error| {
                Error::internal_io(error.to_string(), Some(source.display().to_string()))
            })?);
    }
    if Path::new(&options.workspace.to_worktree).is_dir() {
        let target = std::fs::canonicalize(&options.workspace.to_worktree).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(options.workspace.to_worktree.clone()),
            )
        })?;
        options.workspace.to_worktree = target.display().to_string();
    }
    Ok(())
}

pub(super) fn canonicalize_native_cook_workspace(options: &mut CookRequest) -> Result<bool> {
    let path = options
        .workspace
        .source_worktree_path
        .as_deref()
        .or_else(|| {
            std::path::Path::new(&options.workspace.to_worktree)
                .is_dir()
                .then_some(std::path::Path::new(&options.workspace.to_worktree))
        });
    let Some(path) = path else {
        return Ok(false);
    };
    if let Some(target) =
        homeboy_core::worktree_provider::resolve_native_worktree_mutation_target_by_path(path)?
    {
        if !std::path::Path::new(&options.workspace.to_worktree).is_dir()
            && options.workspace.to_worktree != target.handle
        {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                "Cook source path and native worktree handle identify different worktrees",
                Some(options.workspace.to_worktree.clone()),
                None,
            ));
        }
        options.workspace.to_worktree = target.handle;
        options.workspace.source_worktree_path = Some(target.path);
        return Ok(true);
    }
    Ok(false)
}
