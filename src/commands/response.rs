use crate::cli_surface::{CommandResponseMode, Commands};

use super::GlobalArgs;

pub fn run(command: Commands, global: &GlobalArgs, output_file: Option<&str>) -> i32 {
    let mode = command.response_mode(output_file.is_some());
    let output_artifact_policy = command.output_artifact_policy(output_file.is_some());

    let command = if let CommandResponseMode::Raw(raw_mode) = mode {
        match super::raw_output::run_and_print(command, global, raw_mode) {
            super::raw_output::RawExecution::Handled(exit_code) => return exit_code,
            super::raw_output::RawExecution::Continue(command) => *command,
        }
    } else {
        command
    };

    super::output_artifact::run_and_print(
        command,
        global,
        output_artifact_policy,
        output_file,
        mode,
    )
}
