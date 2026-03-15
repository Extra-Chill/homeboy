use homeboy::engine::cli_tool::{self, CliToolResult};
use serde::Serialize;

use super::CmdResult;

pub struct CliArgs {
    pub tool: String,
    pub identifier: String,
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct CliOutput {
    pub command: String,
    #[serde(flatten)]
    pub result: CliToolResult,
}

pub fn run(args: CliArgs, _global: &super::GlobalArgs) -> CmdResult<CliOutput> {
    let result = cli_tool::run(&args.tool, &args.identifier, &args.args)?;
    let exit_code = result.exit_code;

    Ok((
        CliOutput {
            command: "cli.run".to_string(),
            result,
        },
        exit_code,
    ))
}
