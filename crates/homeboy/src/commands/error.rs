use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct ErrorArgs {
    #[command(subcommand)]
    command: ErrorCommand,
}

#[derive(Subcommand)]
enum ErrorCommand {
    /// List available Homeboy error codes
    Codes,
    /// Explain an error code
    Explain {
        /// Error code (example: `ssh.auth_failed`)
        code: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorCodesOutput {
    pub command: String,
    pub codes: Vec<homeboy_error::ErrorHelpSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorExplainOutput {
    pub command: String,
    pub help: homeboy_error::ErrorHelp,
}

pub fn run(args: ErrorArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<serde_json::Value> {
    match args.command {
        ErrorCommand::Codes => {
            let codes = homeboy_error::list();
            let output = ErrorCodesOutput {
                command: "error.codes".to_string(),
                codes,
            };
            Ok((serde_json::to_value(output).unwrap(), 0))
        }
        ErrorCommand::Explain { code } => {
            let Some(code_enum) = homeboy_error::parse_code(&code) else {
                return Err(homeboy_error::validation_unknown_error_code(code));
            };

            let help = homeboy_error::explain(code_enum);
            let output = ErrorExplainOutput {
                command: "error.explain".to_string(),
                help,
            };

            Ok((serde_json::to_value(output).unwrap(), 0))
        }
    }
}
