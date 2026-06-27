use crate::cli_surface::Commands;

use crate::commands::GlobalArgs;

pub fn run(command: Commands, global: &GlobalArgs, output_file: Option<&str>) -> i32 {
    super::output_runtime::run_command(command, global, output_file)
}
