use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use crate::commands::utils::args::MutationArgs;
use homeboy::core::cleanup;
use homeboy::runner::runners::{
    self as runner, RunnerWorkspaceApplyOutput, RunnerWorkspaceListOutput,
    RunnerWorkspacePruneOutput, RunnerWorkspacePullOutput, RunnerWorkspaceSnapshotFilters,
    RunnerWorkspaceSnapshotsOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOutput,
    RunnerWorkspaceUpdateOptions, RunnerWorkspaceUpdateOutput,
};

use super::CmdResult;

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerWorkspaceOutput {
    List(RunnerWorkspaceListOutput),
    Snapshots(RunnerWorkspaceSnapshotsOutput),
    Sync(RunnerWorkspaceSyncOutput),
    Update(RunnerWorkspaceUpdateOutput),
    Pull(RunnerWorkspacePullOutput),
    Apply(RunnerWorkspaceApplyOutput),
    Prune(RunnerWorkspacePruneOutput),
}

#[derive(Subcommand)]
pub(super) enum RunnerWorkspaceCommand {
    /// List recent runner-side Lab workspaces and reusable exec commands
    List {
        /// Runner ID
        runner_id: String,

        /// Maximum number of workspaces to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Discover metadata-backed runner workspace snapshots by repo, ref, commit, or run
    Snapshots {
        /// Runner ID
        runner_id: String,

        /// Source repository name, normally the local workspace basename before any @slug suffix
        #[arg(long)]
        repo: Option<String>,

        /// Source git ref captured when the snapshot was synced
        #[arg(long)]
        source_ref: Option<String>,

        /// Source git commit captured when the snapshot was synced
        #[arg(long)]
        source_commit: Option<String>,

        /// Agent-task or Lab run id captured in snapshot metadata when available
        #[arg(long = "run")]
        run_id: Option<String>,

        /// Maximum number of snapshots to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Materialize a controller-side worktree into the runner workspace root
    Sync {
        /// Runner ID
        runner_id: String,

        /// Local worktree path to materialize for Lab execution
        #[arg(long)]
        path: String,

        /// Sync mode. snapshot streams source from the controller; snapshot-git also initializes a synthetic git checkout; git is only for clean public/runner-accessible remotes.
        #[arg(long, value_enum, default_value_t = RunnerWorkspaceSyncModeArg::Snapshot)]
        mode: RunnerWorkspaceSyncModeArg,

        /// Permit git sync to overwrite a dirty runner-side workspace.
        #[arg(long)]
        allow_dirty_lab_workspace: bool,
    },
    /// Apply a source delta to a prepared workspace selected by its snapshot lease
    Update {
        /// Runner ID
        runner_id: String,

        /// Local worktree containing the updated source
        #[arg(long)]
        path: String,

        /// Opaque prepared-workspace lease returned by workspace sync or a previous update
        #[arg(long)]
        lease: String,
    },
    /// Copy selected files from a runner workspace back to the controller
    Pull {
        /// Runner ID
        runner_id: String,

        /// Absolute runner-side workspace or snapshot path to pull from
        #[arg(long)]
        remote_path: String,

        /// Relative glob to copy from the remote path. Repeat for multiple globs.
        #[arg(long = "include")]
        includes: Vec<String>,

        /// Local destination directory on the controller
        #[arg(long)]
        to: String,

        /// Validate and print the copy plan without transferring files
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply a Lab-generated patch/delta back to its local source worktree
    Apply {
        /// Lab apply JSON artifact path
        input: String,

        /// Apply even when the local worktree snapshot no longer matches the Lab source snapshot
        #[arg(long)]
        force: bool,
    },
    /// Preview or remove orphaned runner-side Lab workspaces
    Prune {
        /// Runner ID
        runner_id: String,

        // Delete the previewed orphaned workspaces. Without this flag, the
        // command is a dry run. Shared plan-default mutation group (#11139).
        #[command(flatten)]
        mutation: MutationArgs,

        /// Minimum workspace age before it can be considered orphaned.
        /// Defaults to the shared runner age floor
        /// (`cleanup::RUNNER_MIN_AGE_HOURS`).
        #[arg(long)]
        min_age_hours: Option<u64>,

        /// Maximum number of orphan candidates to report or remove per pass.
        /// Defaults to the shared page size
        /// (`cleanup::RUNNER_WORKSPACE_PAGE_LIMIT`).
        #[arg(long)]
        limit: Option<usize>,

        /// Maximum apply passes to run. Each pass re-scans and removes at most --limit candidates.
        #[arg(long, default_value_t = 1)]
        passes: usize,

        /// Persist each apply page and converge through the bounded pass budget.
        #[arg(long)]
        converge: bool,

        /// Resume the durable convergence receipt for this runner and policy.
        #[arg(long, requires = "converge")]
        resume: bool,

        /// Stop convergence after this many seconds, preserving an exact resume receipt.
        #[arg(long, requires = "converge")]
        max_wall_time_seconds: Option<u64>,

        /// Opaque continuation cursor returned by an incomplete workspace-prune scan.
        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(super) enum RunnerWorkspaceSyncModeArg {
    #[default]
    Snapshot,
    SnapshotGit,
    Git,
}

pub(super) fn run(command: RunnerWorkspaceCommand) -> CmdResult<RunnerWorkspaceOutput> {
    match command {
        RunnerWorkspaceCommand::List { runner_id, limit } => {
            runner::list_workspaces(&runner_id, limit)
                .map(|(output, exit_code)| (RunnerWorkspaceOutput::List(output), exit_code))
        }
        RunnerWorkspaceCommand::Snapshots {
            runner_id,
            repo,
            source_ref,
            source_commit,
            run_id,
            limit,
        } => runner::workspace_snapshots(
            &runner_id,
            RunnerWorkspaceSnapshotFilters {
                repo,
                source_ref,
                source_commit,
                run_id,
                limit,
            },
        )
        .map(|(output, exit_code)| (RunnerWorkspaceOutput::Snapshots(output), exit_code)),
        RunnerWorkspaceCommand::Sync {
            runner_id,
            path,
            mode,
            allow_dirty_lab_workspace,
        } => sync(&runner_id, path, mode, allow_dirty_lab_workspace)
            .map(|(output, exit_code)| (RunnerWorkspaceOutput::Sync(output), exit_code)),
        RunnerWorkspaceCommand::Update {
            runner_id,
            path,
            lease,
        } => runner::update_workspace(&runner_id, RunnerWorkspaceUpdateOptions { path, lease })
            .map(|(output, exit_code)| (RunnerWorkspaceOutput::Update(output), exit_code)),
        RunnerWorkspaceCommand::Pull {
            runner_id,
            remote_path,
            includes,
            to,
            dry_run,
        } => runner::pull_workspace(
            &runner_id,
            runner::RunnerWorkspacePullOptions {
                remote_path,
                includes,
                to,
                dry_run,
            },
        )
        .map(|(output, exit_code)| (RunnerWorkspaceOutput::Pull(output), exit_code)),
        RunnerWorkspaceCommand::Apply { input, force } => {
            runner::apply_workspace_patch(runner::RunnerWorkspaceApplyOptions { input, force })
                .map(|(output, exit_code)| (RunnerWorkspaceOutput::Apply(output), exit_code))
        }
        RunnerWorkspaceCommand::Prune {
            runner_id,
            mutation,
            min_age_hours,
            limit,
            passes,
            converge,
            resume,
            max_wall_time_seconds,
            cursor,
        } => runner::prune_workspaces(
            &runner_id,
            runner::RunnerWorkspacePruneOptions {
                apply: mutation.is_apply(),
                // One named age floor shared with `homeboy cleanup --include
                // remote-lab-workspaces`, which used to carry its own literal
                // `24` beside this command's `default_value_t = 24` (#10316).
                min_age_hours: min_age_hours.unwrap_or(cleanup::RUNNER_MIN_AGE_HOURS),
                limit: limit.unwrap_or(cleanup::RUNNER_WORKSPACE_PAGE_LIMIT),
                passes,
                cursor,
                converge,
                resume,
                max_wall_time_seconds,
            },
        )
        .map(|(output, exit_code)| (RunnerWorkspaceOutput::Prune(output), exit_code)),
    }
}

impl From<RunnerWorkspaceSyncModeArg> for RunnerWorkspaceSyncMode {
    fn from(value: RunnerWorkspaceSyncModeArg) -> Self {
        match value {
            RunnerWorkspaceSyncModeArg::Snapshot => RunnerWorkspaceSyncMode::Snapshot,
            RunnerWorkspaceSyncModeArg::SnapshotGit => RunnerWorkspaceSyncMode::SnapshotGit,
            RunnerWorkspaceSyncModeArg::Git => RunnerWorkspaceSyncMode::Git,
        }
    }
}

fn sync(
    runner_id: &str,
    path: String,
    mode: RunnerWorkspaceSyncModeArg,
    allow_dirty_lab_workspace: bool,
) -> CmdResult<RunnerWorkspaceSyncOutput> {
    runner::sync_workspace(
        runner_id,
        runner::RunnerWorkspaceSyncOptions {
            path,
            mode: RunnerWorkspaceSyncMode::from(mode),
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace,
            run_isolation_token: None,
        },
    )
}
