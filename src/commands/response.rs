use crate::cli_surface::Commands;

use super::GlobalArgs;

pub fn run(command: Commands, global: &GlobalArgs, output_file: Option<&str>) -> i32 {
    let plan = command.response_plan(output_file.is_some());

    match super::raw_output::prepare_command_run(command, global, plan.stdout) {
        super::raw_output::CommandRunPreparation::Handled(exit_code) => exit_code,
        super::raw_output::CommandRunPreparation::Json(command) => {
            super::output_artifact::run_and_print(*command, global, plan.output_file, output_file)
        }
        super::raw_output::CommandRunPreparation::Raw(raw_run) => {
            let exit_code = raw_run.exit_code;
            let output_file_result = match raw_run.output_file_result {
                Some(result) => result,
                None => match raw_run.stdout_result.as_ref() {
                    Ok(content) => Ok(serde_json::Value::String(content.clone())),
                    Err(err) => Err(err.clone()),
                },
            };
            let json_run = super::output_artifact::JsonCommandRun {
                stdout_result: output_file_result,
                exit_code,
                output_file_result: None,
            };

            if let Some(path) = output_file {
                super::output_artifact::write_to_file(&json_run, plan.output_file, path);
            }

            match raw_run.stdout_result {
                Ok(content) => print!("{}", content),
                Err(err) => {
                    super::utils::response::print_result::<serde_json::Value>(Err(err)).ok();
                }
            }

            exit_code
        }
    }
}
