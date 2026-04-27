use clap::{Args, Subcommand};
use homeboy::self_status;
use serde_json::Value;

use crate::commands::utils::args::HiddenJsonArgs;
use crate::commands::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct SelfArgs {
    #[command(subcommand)]
    pub command: SelfCommand,
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Report active binary, version, and nearby install/update signals
    Status(SelfStatusArgs),
}

#[derive(Args)]
pub struct SelfStatusArgs {
    #[command(flatten)]
    _json: HiddenJsonArgs,
}

pub fn run(args: SelfArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        SelfCommand::Status(_) => {
            let status = self_status::collect_status();
            let json = serde_json::to_value(status)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
    }
}
