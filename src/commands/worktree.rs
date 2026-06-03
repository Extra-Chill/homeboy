use clap::{Args, Subcommand, ValueEnum};

use homeboy::core::worktree::{
    self, CleanupPolicy, WorktreeCleanupOutput, WorktreeCreateOptions, WorktreeCreateOutput,
    WorktreeListOutput, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeStatusOutput,
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

#[derive(serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorktreeOutput {
    Create(WorktreeCreateOutput),
    List(WorktreeListOutput),
    Status(WorktreeStatusOutput),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupOutput),
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
        WorktreeCommand::List => WorktreeOutput::List(worktree::list()?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
        WorktreeCommand::Remove { id, force } => {
            WorktreeOutput::Remove(worktree::remove(WorktreeRemoveOptions { id, force })?)
        }
        WorktreeCommand::Cleanup { force } => WorktreeOutput::Cleanup(worktree::cleanup(force)?),
    };
    Ok((output, 0))
}
