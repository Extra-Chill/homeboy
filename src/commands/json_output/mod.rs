use serde_json::Value;

use crate::cli_surface::Commands;
use crate::command_contract::{registered_command_dispatch_family, CommandDispatchFamily};

use super::agent_task_summary::{agent_task_summary_kind, render_agent_task_summary};
use super::output_runtime::{CommandPresentation, JsonCommandRun};
use super::{adapter, runner, GlobalArgs};

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
            let summary_kind = agent_task_summary_kind_for_output(&args);
            let (stdout_result, exit_code) = dispatch(Commands::AgentTask(args), global);
            let summary_stdout = stdout_result.as_ref().ok().and_then(|payload| {
                summary_kind.and_then(|kind| render_agent_task_summary(kind, payload))
            });

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        Commands::Runner(args) => runner::run_command_output(args, global),
        Commands::Bench(args) => {
            let summarize = bench_summary_eligible(&args);
            let (stdout_result, exit_code) = dispatch(Commands::Bench(args), global);
            let summary_stdout = summarize
                .then(|| {
                    stdout_result
                        .as_ref()
                        .ok()
                        .and_then(super::bench_summary::render_bench_summary)
                })
                .flatten();

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        Commands::Runs(args) => {
            let summarize = runs_show_summary_eligible(&args);
            let (stdout_result, exit_code) = dispatch(Commands::Runs(args), global);
            let summary_stdout = summarize
                .then(|| {
                    stdout_result
                        .as_ref()
                        .ok()
                        .and_then(super::runs_summary::render_runs_show_summary)
                })
                .flatten();

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        command => {
            let (stdout_result, exit_code) = dispatch(command, global);
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

/// Whether `homeboy runs show` should render the compact human summary
/// instead of the full JSON envelope (#3260). Suppressed by `--json` and in
/// lab-offload subprocesses whose stdout must remain machine-readable.
fn runs_show_summary_eligible(args: &crate::commands::runs::RunsArgs) -> bool {
    args.show_summary_eligible() && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

/// Whether `homeboy bench` should render the compact human summary instead
/// of dumping the full JSON envelope. The full payload is kept for `--json`,
/// for non-run subcommands, and for lab-offload subprocesses (whose stdout
/// must stay machine-readable for the parent process).
fn bench_summary_eligible(args: &crate::commands::bench::BenchArgs) -> bool {
    args.is_run_invocation()
        && !args.wants_full_json()
        && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

fn agent_task_summary_kind_for_output(
    args: &crate::commands::agent_task::AgentTaskArgs,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    agent_task_summary_kind_for_output_mode(
        args,
        homeboy::core::lab_routing::is_lab_offload_subprocess(),
    )
}

fn agent_task_summary_kind_for_output_mode(
    args: &crate::commands::agent_task::AgentTaskArgs,
    lab_offload_subprocess: bool,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    if lab_offload_subprocess {
        None
    } else {
        agent_task_summary_kind(args)
    }
}

fn dispatch(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    let command = match adapter::command_adapter(
        command,
        crate::command_contract::CommandOutputFileMode::None,
    ) {
        Ok(adapter) => return adapter.execute_json(global),
        Err(command) => command,
    };

    match dispatch_family(&command) {
        CommandDispatchFamily::Quality => quality::dispatch(command, global),
        CommandDispatchFamily::Workspace => workspace::dispatch(command, global),
        CommandDispatchFamily::Ops => ops::dispatch(command, global),
        CommandDispatchFamily::RawOnly => {
            unsupported_raw_command("List command uses raw output mode")
        }
    }
}

fn dispatch_family(command: &Commands) -> CommandDispatchFamily {
    registered_command_dispatch_family(command.top_level_name())
        .expect("top-level command should be registered")
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
    use crate::command_contract::CommandDispatchFamily;
    use crate::commands::agent_task::{AgentTaskArgs, AgentTaskCommand, StatusArgs};

    #[test]
    fn list_json_dispatch_reports_raw_output_mode() {
        let (result, exit_code) = dispatch(Commands::List { json: false }, &GlobalArgs {});

        assert_ne!(exit_code, 0);
        assert!(result
            .expect_err("list should not dispatch as JSON")
            .to_string()
            .contains("raw output mode"));
    }

    #[test]
    fn lab_offload_agent_task_subprocess_keeps_json_stdout() {
        let args = AgentTaskArgs {
            command: AgentTaskCommand::Status(StatusArgs {
                run_id: "run-1".to_string(),
                full: false,
            }),
        };

        assert!(agent_task_summary_kind_for_output_mode(&args, false).is_some());
        assert!(agent_task_summary_kind_for_output_mode(&args, true).is_none());
    }

    #[test]
    fn json_dispatch_family_comes_from_command_registry() {
        assert_eq!(
            dispatch_family(&Commands::List { json: false }),
            CommandDispatchFamily::RawOnly
        );
    }
}
