use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::runners::{
    self as runner, RunnerWorkspaceApplyOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOutput,
};

use super::CmdResult;

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerWorkspaceOutput {
    Sync(RunnerWorkspaceSyncOutput),
    Apply(RunnerWorkspaceApplyOutput),
}

#[derive(Subcommand)]
pub(super) enum RunnerWorkspaceCommand {
    /// Materialize a controller-side worktree into the runner workspace root
    Sync {
        /// Runner ID
        runner_id: String,

        /// Local worktree path to materialize for Lab execution
        #[arg(long)]
        path: String,

        /// Sync mode. snapshot streams source from the controller; git is only for public/runner-accessible remotes.
        #[arg(long, value_enum, default_value_t = RunnerWorkspaceSyncModeArg::Snapshot)]
        mode: RunnerWorkspaceSyncModeArg,

        /// Permit git sync to overwrite a dirty runner-side workspace.
        #[arg(long)]
        allow_dirty_lab_workspace: bool,
    },
    /// Apply a Lab-generated patch/delta back to its local source worktree
    Apply {
        /// Lab apply JSON artifact path
        input: String,

        /// Apply even when the local worktree snapshot no longer matches the Lab source snapshot
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(super) enum RunnerWorkspaceSyncModeArg {
    #[default]
    Snapshot,
    Git,
}

pub(super) fn run(command: RunnerWorkspaceCommand) -> CmdResult<RunnerWorkspaceOutput> {
    match command {
        RunnerWorkspaceCommand::Sync {
            runner_id,
            path,
            mode,
            allow_dirty_lab_workspace,
        } => sync(&runner_id, path, mode, allow_dirty_lab_workspace)
            .map(|(output, exit_code)| (RunnerWorkspaceOutput::Sync(output), exit_code)),
        RunnerWorkspaceCommand::Apply { input, force } => {
            runner::apply_workspace_patch(runner::RunnerWorkspaceApplyOptions { input, force })
                .map(|(output, exit_code)| (RunnerWorkspaceOutput::Apply(output), exit_code))
        }
    }
}

impl From<RunnerWorkspaceSyncModeArg> for RunnerWorkspaceSyncMode {
    fn from(value: RunnerWorkspaceSyncModeArg) -> Self {
        match value {
            RunnerWorkspaceSyncModeArg::Snapshot => RunnerWorkspaceSyncMode::Snapshot,
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
        },
    )
}
