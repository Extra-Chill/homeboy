//! Workspace sync mode selection and source-checkout preflight for Lab offload.

use std::path::Path;

use crate::core::{Error, Result};

use super::super::lab_offload_changed_since_ref;
use super::super::source_materialization::{
    requires_controller_routed_workspace_sync_with_policy, SourceMaterializationPolicy,
};
use super::super::{rig_materialization, RunnerWorkspaceSyncMode};
use super::offload::LabOffloadWorkspaceModePolicy;

use super::super::lab_args::lab_offload_source_path;

fn requested_lab_workspace_sync_mode(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
) -> RunnerWorkspaceSyncMode {
    match policy {
        LabOffloadWorkspaceModePolicy::Git | LabOffloadWorkspaceModePolicy::GitCheckoutRequired => {
            RunnerWorkspaceSyncMode::Git
        }
        LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            if lab_offload_changed_since_ref(args).is_some() {
                RunnerWorkspaceSyncMode::Git
            } else {
                RunnerWorkspaceSyncMode::Snapshot
            }
        }
        LabOffloadWorkspaceModePolicy::RunnerResident => RunnerWorkspaceSyncMode::Snapshot,
    }
}

pub(super) fn lab_workspace_sync_mode(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
    source_path: &Path,
) -> Result<RunnerWorkspaceSyncMode> {
    let source_policy = SourceMaterializationPolicy::from_env();
    lab_workspace_sync_mode_with_source_policy(policy, args, source_path, &source_policy)
}

pub(super) fn lab_workspace_sync_mode_with_source_policy(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
    source_path: &Path,
    source_policy: &SourceMaterializationPolicy,
) -> Result<RunnerWorkspaceSyncMode> {
    let requested = requested_lab_workspace_sync_mode(policy, args);
    if requested != RunnerWorkspaceSyncMode::Git {
        return Ok(requested);
    }

    if policy == LabOffloadWorkspaceModePolicy::GitCheckoutRequired {
        return Ok(RunnerWorkspaceSyncMode::Git);
    }

    let remote_url = super::super::workspace::git_output(
        source_path,
        &["config", "--get", "remote.origin.url"],
    )?;
    if requires_controller_routed_workspace_sync_with_policy(&remote_url, source_policy) {
        return Ok(RunnerWorkspaceSyncMode::Snapshot);
    }

    Ok(requested)
}

pub(super) fn preflight_required_git_checkout_workspace(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
) -> Result<()> {
    if policy != LabOffloadWorkspaceModePolicy::GitCheckoutRequired {
        return Ok(());
    }

    let source_path = rig_materialization::lab_offload_rig_component_checkout_root(args)?
        .unwrap_or(lab_offload_source_path(args)?);
    preflight_patch_provider_git_checkout(&source_path)
}

pub(super) fn preflight_patch_provider_git_checkout(source_path: &Path) -> Result<()> {
    let path = source_path.display().to_string();
    let unsupported = |message: &str, hints: Vec<String>| {
        Error::validation_invalid_argument(
            "cwd",
            message.to_string(),
            Some(path.clone()),
            Some(hints),
        )
    };

    let inside_work_tree =
        super::super::workspace::git_output(source_path, &["rev-parse", "--is-inside-work-tree"])
            .map(|value| value == "true")
            .unwrap_or(false);
    if !inside_work_tree {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to be a git checkout so generated files can be returned as a patch artifact",
            vec![
                "Use a Homeboy worktree or another existing git checkout for --cwd.".to_string(),
                "Initialize the target as a git checkout before using Lab offload with a patch-producing provider.".to_string(),
                "Use a provider without a git-checkout materialization requirement only if it has an explicit non-git apply-back artifact contract.".to_string(),
            ],
        ));
    }

    let remote_url =
        super::super::workspace::git_output(source_path, &["config", "--get", "remote.origin.url"])
            .unwrap_or_default();
    if remote_url.trim().is_empty() {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to have remote.origin.url so the runner can materialize a real git checkout",
            vec![
                "Set remote.origin.url on the source checkout before retrying Lab offload.".to_string(),
                "Use a Homeboy worktree or another checkout cloned from the canonical remote.".to_string(),
            ],
        ));
    }

    let status = super::super::workspace::git_output(source_path, &["status", "--porcelain=v1"])
        .unwrap_or_default();
    if !status.trim().is_empty() {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to be a clean git checkout before runner-side patch capture",
            dirty_patch_provider_checkout_hints(source_path, &status),
        ));
    }

    Ok(())
}

fn dirty_patch_provider_checkout_hints(source_path: &Path, status: &str) -> Vec<String> {
    let dirty_entries: Vec<&str> = status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let dirty_count = dirty_entries.len();
    let dirty_sample = dirty_entries
        .iter()
        .take(5)
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(", ");
    let mut hints = vec![
        "Create or select a clean task worktree, then rerun the Lab command with --cwd <worktree>.".to_string(),
        "Leave unrelated local changes in this checkout; Lab patch capture should run from a separate clean worktree.".to_string(),
        format!(
            "Inspect the dirty checkout with: git -C {} status --short",
            source_path.display()
        ),
    ];

    if dirty_count > 0 {
        let suffix = if dirty_count > 5 {
            format!(" and {} more", dirty_count - 5)
        } else {
            String::new()
        };
        hints.push(format!(
            "Dirty checkout summary ({dirty_count}): {dirty_sample}{suffix}"
        ));
    }

    hints
}
