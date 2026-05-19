use crate::cli_surface::{
    CommandOutputArtifactPolicy, CommandRawOutputMode, CommandResponseMode, Commands,
};

use super::utils::{response as output, tty};
use super::GlobalArgs;

pub fn run(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandResponseMode,
    output_artifact_policy: CommandOutputArtifactPolicy,
    output_file: Option<&str>,
) -> i32 {
    match mode {
        CommandResponseMode::Json => {}
        CommandResponseMode::Raw(CommandRawOutputMode::InteractivePassthrough) => {
            if !tty::require_tty_for_interactive() {
                let err = homeboy::core::Error::validation_invalid_argument(
                    "tty",
                    "This command requires an interactive TTY. For non-interactive usage, run: homeboy ssh <target> -- <command...>",
                    None,
                    None,
                );
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return 2;
            }
        }
        CommandResponseMode::Raw(CommandRawOutputMode::Markdown) => {}
        CommandResponseMode::Raw(CommandRawOutputMode::PlainText) => {}
    }

    if let CommandResponseMode::Raw(
        raw_mode @ (CommandRawOutputMode::Markdown | CommandRawOutputMode::PlainText),
    ) = mode
    {
        let raw_result = super::raw_output::run(command, global, raw_mode)
            .expect("markdown and plain-text modes should return raw output");
        match raw_result {
            Ok((content, exit_code)) => {
                print!("{}", content);
                return exit_code;
            }
            Err(err) => {
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return 1;
            }
        }
    }

    let json_run = super::output_artifact::run_json(command, global, output_artifact_policy);

    if let Some(path) = output_file {
        super::output_artifact::write_to_file(&json_run, output_artifact_policy, path);
    }

    if let CommandResponseMode::Json = mode {
        output::print_json_result(json_run.stdout_result, json_run.exit_code).ok();
    }

    json_run.exit_code
}
