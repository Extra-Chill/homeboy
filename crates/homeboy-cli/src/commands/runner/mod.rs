use homeboy::runner::runners::{self as runner};

use super::CmdResult;

use types::RunnerOutput;

pub mod doctor;
mod policy;
mod refresh_plan;
mod workspace;

mod broker;
mod cli;
mod controller_ancestry;
mod dispatch;
mod env;
mod exec;
mod jobs;
mod lifecycle;
mod log_projection;
mod recipe_run;
mod registry;
mod status;
mod types;

#[cfg(test)]
mod tests;

pub use cli::RunnerArgs;
pub(crate) use dispatch::{run, run_command_output};
pub(crate) use status::declared_tool_diagnostics;
pub use types::RunnerToolDiagnostics;

pub(crate) fn is_compact_exec_stdout(args: &RunnerArgs) -> bool {
    args.compact_exec_stdout()
}

pub(crate) fn is_compact_doctor_stdout(args: &RunnerArgs) -> bool {
    args.compact_doctor_stdout()
}

pub(crate) fn refresh_homeboy_uses_bounded_output(args: &RunnerArgs) -> bool {
    matches!(
        &args.command,
        cli::RunnerCommand::RefreshHomeboy { full: false, .. }
    ) && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

pub(crate) fn run_plain_text_raw(args: RunnerArgs) -> super::output_runtime::CommandRun {
    match args.command {
        cli::RunnerCommand::Exec {
            id,
            cwd,
            sync_workspace,
            workspace_ref,
            hydrate_deps,
            workspace_sync_timeout,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            secret_env,
            secret_env_plan,
            secret_env_plan_file,
            extension_env_providers,
            dry_run,
            run_id,
            artifact_outputs,
            artifact_dir_outputs,
            summary_outputs,
            read_only_artifact,
            json: false,
            raw: false,
            command,
        } => dispatch::run_exec_command(
            exec::RunnerExecInput {
                runner_id: id,
                command,
                cwd,
                sync_workspace,
                workspace_ref,
                hydrate_deps,
                workspace_sync_timeout,
                project_id: project,
                allow_diagnostic_ssh: ssh,
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
                raw: false,
                extension_env_providers,
            },
            false,
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
