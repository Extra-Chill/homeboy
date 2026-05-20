use crate::cli_surface::Commands;

use super::GlobalArgs;

pub fn run(command: Commands, global: &GlobalArgs, output_file: Option<&str>) -> i32 {
    let plan = command.response_plan(output_file.is_some());

    let command = match super::raw_output::prepare_json_command(command, global, plan.mode) {
        super::raw_output::JsonCommandPreparation::Handled(exit_code) => return exit_code,
        super::raw_output::JsonCommandPreparation::Continue(command) => *command,
    };

    super::output_artifact::run_and_print(
        command,
        global,
        plan.output_artifact_policy,
        output_file,
        plan.print_json,
    )
}
