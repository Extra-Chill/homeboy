use crate::cli_surface::Commands;
use crate::command_contract::{CommandRawOutputMode, CommandStdoutMode};

use super::output_runtime::CommandRun;
use super::utils::{response as output, tty};
use super::{file, release, report, review, runner, runs, runtime, self_cmd, trace, GlobalArgs};

pub enum RawExecution {
    Handled(i32),
    Continue(Box<Commands>),
}

pub enum CommandRunPreparation {
    Handled(i32),
    Json(Box<Commands>),
    Raw(CommandRun),
}

pub fn prepare_command_run(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandStdoutMode,
) -> CommandRunPreparation {
    match mode {
        CommandStdoutMode::JsonEnvelope => CommandRunPreparation::Json(Box::new(command)),
        CommandStdoutMode::Raw(CommandRawOutputMode::InteractivePassthrough) => {
            match validate_interactive_tty(command) {
                RawExecution::Handled(exit_code) => CommandRunPreparation::Handled(exit_code),
                RawExecution::Continue(command) => CommandRunPreparation::Json(command),
            }
        }
        CommandStdoutMode::Raw(raw_mode) => {
            let raw_run = run(command, global, raw_mode)
                .expect("markdown and plain-text modes should return raw output");
            CommandRunPreparation::Raw(raw_run)
        }
    }
}

pub fn run_and_print(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandRawOutputMode,
) -> RawExecution {
    if let CommandRawOutputMode::InteractivePassthrough = mode {
        return validate_interactive_tty(command);
    }

    let raw_run =
        run(command, global, mode).expect("markdown and plain-text modes should return raw output");

    RawExecution::Handled(match raw_run.raw_stdout.expect("raw mode has raw stdout") {
        Ok(content) => {
            print!("{}", content);
            raw_run.exit_code
        }
        Err(err) => {
            output::print_result::<serde_json::Value>(Err(err)).ok();
            1
        }
    })
}

pub fn run(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandRawOutputMode,
) -> Option<CommandRun> {
    match mode {
        CommandRawOutputMode::InteractivePassthrough => None,
        CommandRawOutputMode::Markdown => Some(run_markdown(command, global)),
        CommandRawOutputMode::PlainText => Some(run_plain_text(command, global)),
    }
}

fn run_markdown(command: Commands, global: &GlobalArgs) -> CommandRun {
    match command {
        Commands::SelfCmd(args) => raw_stdout_only(self_cmd::run_docs_markdown(args)),
        Commands::Release(args) => match args.markdown_changelog_args() {
            Some(changelog_args) => {
                raw_stdout_only(release::changelog::run_markdown(changelog_args))
            }
            None => raw_stdout_only(unsupported_output("markdown")),
        },
        Commands::Review(args) => review::raw_output::run_markdown_with_json(args, global),
        Commands::Trace(args) => trace::run_markdown_with_json_artifact(args, global),
        Commands::Runs(args) => raw_stdout_only(runs::run_markdown(args, global)),
        Commands::Report(args) => raw_stdout_only(report::run_markdown(args)),
        _ => raw_stdout_only(unsupported_output("markdown")),
    }
}

fn run_plain_text(command: Commands, global: &GlobalArgs) -> CommandRun {
    match command {
        Commands::File(args) => raw_stdout_only(match file::run(args, global) {
            Ok((file::FileCommandOutput::Raw(content), exit_code)) => Ok((content, exit_code)),
            Ok(_) => Err(homeboy::core::Error::internal_unexpected(
                "Unexpected output type for raw mode",
            )),
            Err(err) => Err(err),
        }),
        Commands::Runner(args) if runner::is_compact_exec_stdout(&args) => {
            runner_compact_exec(args, global)
        }
        Commands::Runtime(args) => raw_stdout_only(runtime::run_plain_text(args)),
        _ => raw_stdout_only(unsupported_output("plain text")),
    }
}

fn runner_compact_exec(
    args: crate::commands::runner::RunnerArgs,
    global: &GlobalArgs,
) -> CommandRun {
    runner::run_plain_text_raw(args, global)
}

pub fn raw_stdout_only(result: homeboy::core::Result<(String, i32)>) -> CommandRun {
    match result {
        Ok((content, exit_code)) => {
            CommandRun::from_raw_stdout("unknown", Ok(content), exit_code, None)
        }
        Err(err) => CommandRun::from_raw_stdout("unknown", Err(err), 1, None),
    }
}

fn validate_interactive_tty(command: Commands) -> RawExecution {
    if tty::require_tty_for_interactive() {
        return RawExecution::Continue(Box::new(command));
    }

    let err = homeboy::core::Error::validation_invalid_argument(
        "tty",
        "This command requires an interactive TTY. For non-interactive usage, run: homeboy ssh <target> -- <command...>",
        None,
        None,
    );
    output::print_result::<serde_json::Value>(Err(err)).ok();
    RawExecution::Handled(2)
}

fn unsupported_output(mode: &str) -> homeboy::core::Result<(String, i32)> {
    Err(homeboy::core::Error::validation_invalid_argument(
        "output_mode",
        format!("Command does not support {mode} output"),
        None,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_passthrough_has_no_raw_text_result() {
        let result = run(
            Commands::Contract(crate::commands::contract::ContractArgs {
                command: crate::commands::contract::ContractCommand::Manifest(
                    crate::commands::manifest::ManifestArgs {},
                ),
            }),
            &GlobalArgs {},
            CommandRawOutputMode::InteractivePassthrough,
        );

        assert!(result.is_none());
    }
}
