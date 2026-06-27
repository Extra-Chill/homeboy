use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, WorktreeAdoptOptions, WorktreeAdoptOutput, WorktreeCleanupOutput,
    WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput, WorktreeQueueCreateOptions,
    WorktreeQueueCreateOutput, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeStatusOutput,
};

use super::CmdResult;

#[derive(Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
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
    /// Create multiple DMC worktrees one-at-a-time with lock-aware queue status JSON
    QueueCreate {
        /// DMC workspace repo handle, e.g. homeboy
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
        /// Short task reference recorded by DMC, e.g. Extra-Chill/homeboy#5786
        #[arg(long)]
        task_ref: Option<String>,
        /// Print the queue plan/status without creating worktrees
        #[arg(long)]
        dry_run: bool,
        /// Suggested orchestrator wait when DMC reports an active lock but no retry-after value
        #[arg(long, default_value_t = 60)]
        retry_after_seconds: u64,
        /// Executable used for DMC calls. Defaults to `studio`.
        #[arg(long, default_value = "studio")]
        dmc_bin: String,
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
    },
    /// Remove cleanup-eligible task worktrees after safety checks
    Cleanup {
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Skip the automatic rebuildable artifact cleanup pass.
        #[arg(long)]
        skip_artifact_cleanup: bool,
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
            dmc_bin,
        } => WorktreeOutput::QueueCreate(worktree::queue_create(WorktreeQueueCreateOptions {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
            dmc_bin,
        })?),
        WorktreeCommand::List => WorktreeOutput::List(worktree::list()?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
        WorktreeCommand::Remove { id, force } => {
            WorktreeOutput::Remove(worktree::remove(WorktreeRemoveOptions { id, force })?)
        }
        WorktreeCommand::Cleanup {
            force,
            skip_artifact_cleanup,
        } => {
            let worktrees = worktree::cleanup(force)?;
            let artifact_cleanup = if skip_artifact_cleanup {
                None
            } else {
                Some(artifact_cleanup::cleanup_artifacts(
                    ArtifactCleanupOptions {
                        path: None,
                        apply: true,
                        self_artifacts: true,
                        temp_roots: Vec::new(),
                        merged_only: false,
                    },
                )?)
            };
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                artifact_cleanup,
            })
        }
    };
    Ok((output, 0))
}
