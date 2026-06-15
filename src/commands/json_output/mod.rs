use serde_json::Value;

use crate::cli_surface::Commands;
use crate::command_contract::CommandJsonFamily;

use super::agent_task_summary::{agent_task_summary_kind, render_agent_task_summary};
use super::output_runtime::JsonCommandRun;
use super::GlobalArgs;

mod ops;
mod quality;
mod workspace;

type JsonRun = (homeboy::core::Result<Value>, i32);

/// Dispatch a command to its handler and map the structured result to JSON.
pub fn run(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    dispatch(command, global)
}

pub fn run_command_output(command: Commands, global: &GlobalArgs) -> JsonCommandRun {
    crate::commands::utils::tty::status("homeboy is working...");

    match command {
        Commands::AgentTask(args) => {
            let summary_kind = agent_task_summary_kind(&args);
            let (stdout_result, exit_code) = dispatch(Commands::AgentTask(args), global);
            let human_stdout = stdout_result.as_ref().ok().and_then(|payload| {
                summary_kind.and_then(|kind| render_agent_task_summary(kind, payload))
            });

            JsonCommandRun {
                stdout_result,
                exit_code,
                output_file_result: None,
                human_stdout,
            }
        }
        command => {
            let (stdout_result, exit_code) = dispatch(command, global);
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

fn dispatch(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    match command.output_descriptor(false).json_family {
        CommandJsonFamily::Quality => quality::dispatch(command, global),
        CommandJsonFamily::Workspace => workspace::dispatch(command, global),
        CommandJsonFamily::Ops => ops::dispatch(command, global),
        CommandJsonFamily::RawOnly => unsupported_raw_command("List command uses raw output mode"),
    }
}

fn map<T: serde::Serialize>(result: super::CmdResult<T>) -> JsonRun {
    crate::commands::utils::response::map_cmd_result_to_json(result)
}

fn unsupported_raw_command(message: &'static str) -> JsonRun {
    let err = homeboy::core::Error::validation_invalid_argument("output_mode", message, None, None);
    crate::commands::utils::response::map_cmd_result_to_json::<Value>(Err(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_dispatch_reports_raw_output_mode() {
        let (result, exit_code) = dispatch(Commands::List, &GlobalArgs {});

        assert_ne!(exit_code, 0);
        assert!(result
            .expect_err("list should not dispatch as JSON")
            .to_string()
            .contains("raw output mode"));
    }
}
