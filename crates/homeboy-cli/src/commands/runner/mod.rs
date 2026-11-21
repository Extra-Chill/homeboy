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
mod exec_args;
mod jobs;
mod lifecycle;
mod log_projection;
mod recipe_run;
mod registry;
mod selection;
mod status;
mod types;

#[cfg(test)]
mod tests;

pub use cli::RunnerArgs;
pub(crate) use dispatch::{run, run_command_output};
pub(crate) use status::reconciliation_outcome;
pub use types::RunnerToolDiagnostics;

pub(crate) fn is_compact_doctor_stdout(args: &RunnerArgs) -> bool {
    args.compact_doctor_stdout()
}

pub(crate) fn refresh_homeboy_uses_bounded_output(args: &RunnerArgs) -> bool {
    matches!(
        &args.command,
        cli::RunnerCommand::RefreshHomeboy { full: false, .. }
    ) && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

/// A runner invocation whose default stdout is compact plain text: `runner
/// exec` without `--json`/`--raw`, and `runner job list` without `--json`.
pub(crate) enum PlainTextRunner {
    JobList(RunnerArgs),
    Exec(exec_args::RunnerExecArgs),
}

/// Split compact plain-text runner invocations from every other runner
/// invocation (JSON envelope).
pub(crate) fn into_plain_text(args: RunnerArgs) -> Result<PlainTextRunner, RunnerArgs> {
    match args.command {
        command @ cli::RunnerCommand::Job {
            command: cli::RunnerJobCommand::List { json: false, .. },
        } => Ok(PlainTextRunner::JobList(RunnerArgs { command })),
        cli::RunnerCommand::Exec(args) if !args.json && !args.raw => {
            Ok(PlainTextRunner::Exec(args))
        }
        command => Err(RunnerArgs { command }),
    }
}

impl PlainTextRunner {
    pub(crate) fn run(self) -> super::output_runtime::CommandRun {
        match self {
            Self::JobList(args) => {
                let (stdout_result, exit_code) =
                    crate::commands::utils::response::map_cmd_result_to_json(dispatch::run(args));
                jobs::compact_list_command_run(stdout_result, exit_code)
            }
            Self::Exec(args) => dispatch::run_exec_command(args.into(), false),
        }
    }
}
