//! Re-cook admission for a destination that still holds a prior failed
//! attempt's uncommitted candidate (#15775).
//!
//! A Cook whose gate fails leaves its candidate uncommitted in the native task
//! worktree it provisioned (`PreserveOnFailure`). A later Cook for the same
//! issue derives the same worktree handle and, before this module existed,
//! shipped that leftover dirt to its provider. Lab then refused the dispatch in
//! `committed_harvest_preflight` as "a dirty source checkout", so a re-cook
//! could never start without manual cleanup.
//!
//! The leftover is Homeboy's own output, and the native worktree registry
//! records which run provisioned the worktree. When that owner is a different,
//! terminal, unsuccessful run and this Cook has not executed a provider yet,
//! the dirt is preserved as a Git commit under a dedicated ref and the
//! worktree is reset to its HEAD. The new attempt then starts from a clean
//! base, and the prior candidate stays recoverable with one command.
//!
//! Every other dirty destination (operator edits, explicit CWDs, this Cook's
//! own candidate, an owner that is still running, or an owner without a
//! durable record) keeps its existing admission behavior.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_task_lifecycle::{self, AgentTaskLifecycleStore, AgentTaskRunState};
use homeboy_core::{Error, Result};

use super::cook_baseline::{git_output, git_output_with_env};
use super::CookRequest;

pub(crate) const PRIOR_ATTEMPT_CANDIDATE_SCHEMA: &str =
    "homeboy/cook-prior-attempt-candidate-preservation/v1";
pub(crate) const PRIOR_ATTEMPT_CANDIDATE_METADATA_KEY: &str =
    "cook_prior_attempt_candidate_preservation";
pub(crate) const PRESERVED_CANDIDATE_REF_PREFIX: &str = "refs/homeboy/preserved-candidates";
const MAX_RECORDED_PATHS: usize = 50;

/// The terminal, unsuccessful run that provisioned the destination worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PriorAttemptOwner {
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub handle: String,
    pub path: PathBuf,
}

/// Classify the destination's native lifecycle owner. Returns `None` unless
/// the worktree is a registry-owned task worktree provisioned by a different
/// Cook's run that is durably terminal and did not succeed.
pub(crate) fn prior_failed_attempt_owner(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
) -> Result<Option<PriorAttemptOwner>> {
    if super::cook::cook_uses_explicit_cwd_workspace(options) {
        // Explicit CWDs are caller-owned checkouts; never mutate them.
        return Ok(None);
    }
    let handle = options.workspace.to_worktree.as_str();
    let Some(homeboy_core::worktree::WorkspaceRefRecord::Task(record)) =
        homeboy_core::worktree::resolve_workspace_ref_if_present(handle)?
    else {
        return Ok(None);
    };
    if record.id != handle || record.state != homeboy_core::worktree::TaskWorktreeState::Active {
        return Ok(None);
    }
    let Some(owner) = record
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|owner| !owner.is_empty())
    else {
        return Ok(None);
    };
    if owner == options.identity.initial_run_id {
        return Ok(None);
    }
    if let Ok(index) = lifecycle_store.read_cook_index(&options.identity.cook_id) {
        if index.attempts.iter().any(|attempt| attempt.run_id == owner) {
            // This Cook's own attempt owns the candidate; continuation and
            // adoption paths remain authoritative for it.
            return Ok(None);
        }
    }
    let Ok(owner_record) = lifecycle_store.read_record(owner) else {
        // Without durable owner evidence the dirt cannot be attributed.
        return Ok(None);
    };
    if !owner_record.state.is_terminal() || owner_record.state == AgentTaskRunState::Succeeded {
        return Ok(None);
    }
    let Ok(path) = std::fs::canonicalize(&record.worktree_path) else {
        return Ok(None);
    };
    if let Some(source) = options.workspace.source_worktree_path.as_deref() {
        // The admitted source must be the exact registry-owned checkout.
        if std::fs::canonicalize(source).ok().as_deref() != Some(path.as_path()) {
            return Ok(None);
        }
    }
    let path =
        homeboy_core::worktree_provider::require_linked_worktree_mutation_path(&path, handle)?;
    Ok(Some(PriorAttemptOwner {
        run_id: owner.to_string(),
        state: owner_record.state,
        handle: handle.to_string(),
        path,
    }))
}

/// Preserve a prior failed attempt's uncommitted candidate and reset the
/// destination to its HEAD. Returns the durable evidence when something was
/// preserved, and records it on this Cook's run.
pub(crate) fn preserve_prior_attempt_candidate(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
) -> Result<Option<Value>> {
    let Some(owner) = prior_failed_attempt_owner(lifecycle_store, options)? else {
        return Ok(None);
    };
    let paths = dirty_paths(&owner.path)?;
    if paths.is_empty() {
        return Ok(None);
    }
    let head = git_output(&owner.path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let head_tree = git_output(&owner.path, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    // A private index keeps the snapshot independent of the user-visible
    // index and of any concurrent stash in the shared repository.
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create prior Cook candidate preservation index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    let index_env: &[(&str, &str)] = &[("GIT_INDEX_FILE", index_path.as_str())];
    git_output_with_env(&owner.path, &["read-tree", &head], index_env)?;
    git_output_with_env(&owner.path, &["add", "--all"], index_env)?;
    let tree = git_output_with_env(&owner.path, &["write-tree"], index_env)?;
    if tree == head_tree {
        // Nothing content-bearing to preserve; leave existing admission alone.
        return Ok(None);
    }
    let message = format!(
        "homeboy: preserve prior Cook candidate from {} before {}",
        owner.run_id, options.identity.initial_run_id
    );
    let commit = git_output(
        &owner.path,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-m",
            &message,
        ],
    )?;
    let reference = preserved_candidate_ref(&owner.run_id, &commit);
    git_output(
        &owner.path,
        &["update-ref", "-m", &message, &reference, &commit],
    )?;
    // The candidate is now reachable from a durable ref, so resetting the
    // worktree discards nothing that cannot be restored.
    git_output(&owner.path, &["reset", "--hard", "--quiet", &head])?;
    git_output(&owner.path, &["clean", "-fd", "--quiet"])?;
    let remaining = dirty_paths(&owner.path)?;
    let recovery_command = format!(
        "git -C {} cherry-pick --no-commit {}",
        homeboy_engine_primitives::shell::quote_arg(&owner.path.display().to_string()),
        reference
    );
    if !remaining.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            "Cook preserved the prior attempt's candidate but could not reset the destination to a clean base",
            Some(owner.handle.clone()),
            Some(vec![format!(
                "Inspect the remaining paths, clean the worktree, and rerun Cook. The prior candidate is recoverable with `{recovery_command}`."
            )]),
        );
        error.details["prior_attempt_candidate"] = serde_json::json!({
            "ref": reference,
            "commit": commit,
            "remaining_paths": remaining,
        });
        return Err(error);
    }
    let truncated = paths.len() > MAX_RECORDED_PATHS;
    let evidence = serde_json::json!({
        "schema": PRIOR_ATTEMPT_CANDIDATE_SCHEMA,
        "action": "preserved_and_reset",
        "prior_run_id": owner.run_id,
        "prior_run_state": owner.state,
        "handle": owner.handle,
        "workspace": owner.path,
        "base_head": head,
        "commit": commit,
        "tree": tree,
        "ref": reference,
        "changed_paths": paths.iter().take(MAX_RECORDED_PATHS).collect::<Vec<_>>(),
        "changed_path_count": paths.len(),
        "changed_paths_truncated": truncated,
        "recovery_command": recovery_command,
    });
    // The prior run is terminal, so its record is the durable place an
    // operator inspecting the failed attempt finds where its candidate went.
    // The new run carries the same evidence until a detached handoff
    // re-projects its record; the ref itself is the recovery authority.
    let mut preserved_by = evidence.clone();
    preserved_by["preserved_by_run_id"] = Value::String(options.identity.initial_run_id.clone());
    preserved_by["preserved_by_cook_id"] = Value::String(options.identity.cook_id.clone());
    agent_task_lifecycle::record_metadata_value_in_store(
        lifecycle_store,
        &owner.run_id,
        PRIOR_ATTEMPT_CANDIDATE_METADATA_KEY,
        preserved_by,
    )?;
    if lifecycle_store.record_exists(&options.identity.initial_run_id)? {
        agent_task_lifecycle::record_metadata_value_in_store(
            lifecycle_store,
            &options.identity.initial_run_id,
            PRIOR_ATTEMPT_CANDIDATE_METADATA_KEY,
            evidence.clone(),
        )?;
    }
    Ok(Some(evidence))
}

/// One ref per preserved snapshot: the commit suffix keeps an interrupted and
/// retried preservation from overwriting an earlier, different candidate.
fn preserved_candidate_ref(prior_run_id: &str, commit: &str) -> String {
    let run: String = prior_run_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let short = &commit[..commit.len().min(12)];
    format!("{PRESERVED_CANDIDATE_REF_PREFIX}/{run}/{short}")
}

/// Porcelain `-z` entries are `XY <path>`; a rename or copy is followed by
/// its source path as a separate record. The status text must not be trimmed:
/// a leading space is the `X` column of an unstaged change.
fn dirty_paths(path: &Path) -> Result<Vec<String>> {
    let status = homeboy_core::git::run_git(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "inspect prior Cook candidate in destination",
    )?;
    let mut paths = Vec::new();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let Some(changed) = entry.get(3..) else {
            continue;
        };
        paths.push(changed.to_string());
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    Ok(paths)
}
