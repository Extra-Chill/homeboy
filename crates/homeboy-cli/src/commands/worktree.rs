use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput, ArtifactCleanupSort,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, WorktreeAdoptOptions, WorktreeAdoptOutput, WorktreeCleanupOptions,
    WorktreeCleanupOutput, WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput,
    WorktreeQueueCreateOptions, WorktreeQueueCreateOutput, WorktreeRemoveOptions,
    WorktreeRemoveOutput, WorktreeStatusOutput,
};

use crate::command_contract::{LabCommandContract, WORKTREE_CLEANUP_LAB_LABEL};

use super::CmdResult;

#[derive(Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}

impl WorktreeArgs {
    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        match self.command {
            WorktreeCommand::Cleanup { .. } => Some(LabCommandContract::runner_resident(
                WORKTREE_CLEANUP_LAB_LABEL,
            )),
            _ => None,
        }
    }
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a task worktree from a registered component checkout
    Create {
        /// Component ID to use as the source checkout
        component_id: String,
        /// Branch to create in the task worktree
        #[arg(long)]
        branch: String,
        /// Base ref for the new worktree branch
        #[arg(long = "from")]
        from: Option<String>,
        /// Task or issue URL associated with this worktree
        #[arg(long)]
        task_url: Option<String>,
        /// Agent-task run ID associated with this worktree
        #[arg(long)]
        run_id: Option<String>,
        /// Cleanup policy for lifecycle cleanup
        #[arg(long, value_enum)]
        cleanup_policy: Option<CliCleanupPolicy>,
    },
    /// Adopt an existing local workspace path for @workspace:<handle> refs
    Adopt {
        /// Workspace handle resolved by @workspace:<handle>
        handle: String,
        /// Existing local directory to resolve for this handle
        path: String,
        /// Optional generic kind label recorded as provenance
        #[arg(long)]
        kind: Option<String>,
        /// Optional JSON provenance payload recorded with the adopted path
        #[arg(long)]
        provenance_json: Option<String>,
    },
    /// Create multiple task worktrees one-at-a-time with queue status JSON
    QueueCreate {
        /// Registered component/repo handle, e.g. homeboy
        repo: String,
        /// Branch to create. Repeat for fanout batches.
        #[arg(long = "branch", value_name = "BRANCH", required = true)]
        branches: Vec<String>,
        /// Base ref for each worktree branch
        #[arg(long = "from", default_value = "origin/main")]
        from: String,
        /// Task or issue URL associated with these worktrees
        #[arg(long)]
        task_url: Option<String>,
        /// Short task reference associated with these worktrees, e.g. Extra-Chill/homeboy#5786
        #[arg(long)]
        task_ref: Option<String>,
        /// Print the queue plan/status without creating worktrees
        #[arg(long)]
        dry_run: bool,
        /// Suggested orchestrator wait when queueing is blocked but no retry-after value is available
        #[arg(long, default_value_t = 60)]
        retry_after_seconds: u64,
    },
    /// List persisted task worktrees
    List,
    /// Inspect one task worktree and its safety gates
    Status {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
    },
    /// Remove one task worktree after safety checks
    Remove {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Delete the local task branch after removing the worktree when branch safety allows it.
        #[arg(long)]
        cleanup_branch: bool,
        /// Permit deleting an unmerged task branch. Requires --cleanup-branch.
        #[arg(long, requires = "cleanup_branch")]
        allow_unmerged_branch: bool,
    },
    /// Report cleanup-eligible task worktrees; pass --apply to remove them
    Cleanup {
        /// Apply cleanup by removing cleanup-eligible task worktrees and any
        /// selected artifacts. Without this flag, the command is a dry run.
        #[arg(long)]
        apply: bool,
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Deprecated no-op retained for one release: cleanup is already
        /// plan-only unless --apply is passed.
        #[arg(long, hide = true, conflicts_with = "apply")]
        dry_run: bool,
        /// Also remove declared rebuildable artifacts from the Homeboy checkout that built this binary.
        #[arg(long)]
        cleanup_artifacts: bool,
        /// Delete merged task branches for removed cleanup candidates.
        #[arg(long)]
        cleanup_branches: bool,
        /// Permit deleting unmerged task branches. Requires --cleanup-branches.
        #[arg(long, requires = "cleanup_branches")]
        allow_unmerged_branches: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum CliCleanupPolicy {
    RemoveWhenSafe,
    PreserveOnFailure,
}

impl From<CliCleanupPolicy> for CleanupPolicy {
    fn from(value: CliCleanupPolicy) -> Self {
        match value {
            CliCleanupPolicy::RemoveWhenSafe => CleanupPolicy::RemoveWhenSafe,
            CliCleanupPolicy::PreserveOnFailure => CleanupPolicy::PreserveOnFailure,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorktreeOutput {
    Create(WorktreeCreateOutput),
    Adopt(WorktreeAdoptOutput),
    QueueCreate(WorktreeQueueCreateOutput),
    List(WorktreeListOutput),
    Status(WorktreeStatusOutput),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupCommandOutput),
}

#[derive(Serialize)]
pub struct WorktreeCleanupCommandOutput {
    pub worktrees: WorktreeCleanupOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_cleanup: Option<ArtifactCleanupOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Soft deprecation note emitted when a caller still passes the legacy
/// `--dry-run` flag. Cleanup is plan-only by default now, so the flag is a
/// no-op kept for one release so existing scripts keep parsing.
const DRY_RUN_DEPRECATION_NOTICE: &str = "`--dry-run` is deprecated and has no effect: `homeboy worktree cleanup` is plan-only by default. Pass --apply to remove worktrees.";

/// Resolve the mutation gate for `worktree cleanup`.
///
/// Removal only happens when `--apply` is passed. `--dry-run` is a deprecated
/// no-op alias; it can never turn the command into a mutation.
fn cleanup_apply_mode(apply: bool, dry_run: bool) -> (bool, Vec<String>) {
    let warnings = if dry_run {
        vec![DRY_RUN_DEPRECATION_NOTICE.to_string()]
    } else {
        Vec::new()
    };

    (apply && !dry_run, warnings)
}

pub fn run(args: WorktreeArgs, _global: &super::GlobalArgs) -> CmdResult<WorktreeOutput> {
    let output = match args.command {
        WorktreeCommand::Create {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy,
        } => WorktreeOutput::Create(worktree::create(WorktreeCreateOptions {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy: cleanup_policy.map(Into::into),
        })?),
        WorktreeCommand::Adopt {
            handle,
            path,
            kind,
            provenance_json,
        } => {
            let provenance = provenance_json
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|err| {
                    homeboy::core::Error::validation_invalid_json(
                        err,
                        Some("provenance_json".to_string()),
                        None,
                    )
                })?;
            WorktreeOutput::Adopt(worktree::adopt(WorktreeAdoptOptions {
                handle,
                path,
                kind,
                provenance,
            })?)
        }
        WorktreeCommand::QueueCreate {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
        } => WorktreeOutput::QueueCreate(worktree::queue_create(WorktreeQueueCreateOptions {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
        })?),
        WorktreeCommand::List => WorktreeOutput::List(worktree::list()?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
        WorktreeCommand::Remove {
            id,
            force,
            cleanup_branch,
            allow_unmerged_branch,
        } => WorktreeOutput::Remove(worktree::remove(WorktreeRemoveOptions {
            id,
            force,
            cleanup_branch,
            allow_unmerged_branch,
        })?),
        WorktreeCommand::Cleanup {
            apply,
            force,
            dry_run,
            cleanup_artifacts,
            cleanup_branches,
            allow_unmerged_branches,
        } => {
            let (apply, warnings) = cleanup_apply_mode(apply, dry_run);
            let worktrees = worktree::cleanup(WorktreeCleanupOptions {
                force,
                dry_run: !apply,
                cleanup_branches,
                allow_unmerged_branches,
            })?;
            let artifact_cleanup = if cleanup_artifacts {
                Some(artifact_cleanup::cleanup_artifacts(
                    ArtifactCleanupOptions {
                        path: None,
                        apply,
                        self_artifacts: true,
                        temp_roots: Vec::new(),
                        sort: ArtifactCleanupSort::Discovery,
                        limit: None,
                        merged_only: false,
                        min_age_days: None,
                        include_active_worktrees: false,
                    },
                )?)
            } else {
                None
            };
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                artifact_cleanup,
                warnings,
            })
        }
    };
    Ok((output, 0))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli_surface::{Cli, Commands};

    use super::{cleanup_apply_mode, WorktreeCommand, DRY_RUN_DEPRECATION_NOTICE};

    /// Parse `worktree cleanup` and return its `(apply, dry_run)` gate flags.
    fn parse_cleanup_gate(args: &[&str]) -> (bool, bool) {
        let cli = Cli::parse_from(args);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup { apply, dry_run, .. } = args.command else {
            panic!("expected worktree cleanup command");
        };

        (apply, dry_run)
    }

    /// The mutation gate for every other cleanup surface is `--apply`. The bare
    /// command must stay plan-only so an operator or agent that generalizes
    /// "append --apply to make it real" cannot delete worktrees on the preview
    /// step.
    #[test]
    fn worktree_cleanup_is_plan_only_without_apply() {
        let (apply, dry_run) = parse_cleanup_gate(&["homeboy", "worktree", "cleanup"]);

        assert!(!apply);
        assert!(!dry_run);
        assert_eq!(cleanup_apply_mode(apply, dry_run), (false, Vec::new()));
    }

    #[test]
    fn worktree_cleanup_mutates_only_with_apply() {
        let (apply, dry_run) = parse_cleanup_gate(&["homeboy", "worktree", "cleanup", "--apply"]);

        assert!(apply);
        assert!(!dry_run);
        assert_eq!(cleanup_apply_mode(apply, dry_run), (true, Vec::new()));
    }

    /// `--dry-run` is a deprecated no-op alias kept for one release so existing
    /// scripts keep parsing. It must never mutate.
    #[test]
    fn worktree_cleanup_dry_run_still_parses_and_does_not_mutate() {
        let (apply, dry_run) = parse_cleanup_gate(&["homeboy", "worktree", "cleanup", "--dry-run"]);

        assert!(!apply);
        assert!(dry_run);
        assert_eq!(
            cleanup_apply_mode(apply, dry_run),
            (false, vec![DRY_RUN_DEPRECATION_NOTICE.to_string()])
        );
    }

    #[test]
    fn worktree_cleanup_rejects_dry_run_with_apply() {
        assert!(
            Cli::try_parse_from(["homeboy", "worktree", "cleanup", "--dry-run", "--apply"])
                .is_err()
        );
    }

    /// Defense in depth: even if the conflict gate were relaxed, the deprecated
    /// alias can only ever downgrade to plan-only.
    #[test]
    fn worktree_cleanup_dry_run_cannot_upgrade_to_apply() {
        assert_eq!(
            cleanup_apply_mode(true, true),
            (false, vec![DRY_RUN_DEPRECATION_NOTICE.to_string()])
        );
    }

    #[test]
    fn worktree_cleanup_does_not_cleanup_artifacts_by_default() {
        let cli = Cli::parse_from(["homeboy", "worktree", "cleanup"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup {
            cleanup_artifacts, ..
        } = args.command
        else {
            panic!("expected worktree cleanup command");
        };

        assert!(!cleanup_artifacts);
    }

    #[test]
    fn worktree_cleanup_artifact_cleanup_requires_explicit_flag() {
        let cli = Cli::parse_from(["homeboy", "worktree", "cleanup", "--cleanup-artifacts"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup {
            cleanup_artifacts, ..
        } = args.command
        else {
            panic!("expected worktree cleanup command");
        };

        assert!(cleanup_artifacts);
    }
}
