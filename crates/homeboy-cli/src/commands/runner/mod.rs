use homeboy::runner::runners::{self as runner};

use super::CmdResult;

use types::RunnerOutput;

pub mod doctor;
mod policy;
mod refresh_plan;
mod workspace;

mod broker;
mod cli;
mod dispatch;
mod env;
mod exec;
mod jobs;
mod lifecycle;
mod log_projection;
mod registry;
mod status;
mod types;

#[cfg(test)]
mod tests;

pub use cli::RunnerArgs;
pub use dispatch::{run, run_command_output};
pub(crate) use status::declared_tool_diagnostics;
pub use types::RunnerToolDiagnostics;

pub fn is_compact_exec_stdout(args: &RunnerArgs) -> bool {
    args.compact_exec_stdout()
}

pub fn run_plain_text_raw(
    args: RunnerArgs,
    _global: &super::GlobalArgs,
) -> super::output_runtime::CommandRun {
    match args.command {
        cli::RunnerCommand::Exec {
            id,
            cwd,
            sync_workspace,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            secret_env,
            secret_env_plan,
            secret_env_plan_file,
            dry_run,
            run_id,
            artifact_outputs,
            artifact_dir_outputs,
            summary_outputs,
            read_only_artifact,
            json: false,
            raw: false,
            command,
        } => dispatch::run_compact_exec(
            id,
            cwd,
            sync_workspace,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            secret_env,
            secret_env_plan,
            secret_env_plan_file,
            dry_run,
            run_id,
            artifact_outputs,
            artifact_dir_outputs,
            summary_outputs,
            read_only_artifact,
            command,
        ),
        _ => super::output_runtime::CommandRun::from_raw_stdout(
            "runner",
            Err(homeboy::core::Error::validation_invalid_argument(
                "output_mode",
                "runner command does not support plain text output",
                None,
                None,
            )),
            2,
            None,
        ),
    }
}
