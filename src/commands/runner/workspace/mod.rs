use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::runners::{
    self as runner, RunnerWorkspaceApplyOutput, RunnerWorkspaceListOutput,
    RunnerWorkspacePruneOutput, RunnerWorkspacePullOutput, RunnerWorkspaceSnapshotFilters,
    RunnerWorkspaceSnapshotsOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOutput,
};

use super::CmdResult;

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerWorkspaceOutput {
    List(RunnerWorkspaceListOutput),
    Snapshots(RunnerWorkspaceSnapshotsOutput),
    Sync(RunnerWorkspaceSyncOutput),
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

        /// Delete the previewed orphaned workspaces. Without this flag, the command is a dry run.
        #[arg(long)]
        apply: bool,

        /// Minimum workspace age before it can be considered orphaned.
        #[arg(long, default_value_t = 24)]
        min_age_hours: u64,

        /// Maximum number of orphan candidates to report or remove.
        #[arg(long, default_value_t = 25)]
        limit: usize,
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
            apply,
            min_age_hours,
            limit,
        } => runner::prune_workspaces(
            &runner_id,
            runner::RunnerWorkspacePruneOptions {
                apply,
                min_age_hours,
                limit,
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
