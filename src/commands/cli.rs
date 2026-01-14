use homeboy::cli_tool::{self, CliToolResult};
use serde::Serialize;

use super::CmdResult;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliOutput {
    pub command: String,
    #[serde(flatten)]
    pub result: CliToolResult,
}

pub fn run(
    tool: &str,
    identifier: &str,
    args: Vec<String>,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<CliOutput> {
    let result = cli_tool::run(tool, identifier, &args)?;
    let exit_code = result.exit_code;

    Ok((
        CliOutput {
            command: "cli.run".to_string(),
            result,
        },
        exit_code,
    ))
}
