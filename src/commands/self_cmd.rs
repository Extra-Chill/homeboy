use clap::{Args, Subcommand};
use homeboy::core::build_identity;
use homeboy::core::engine;
use homeboy::core::self_status;
use serde_json::Value;

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
    /// Report the active binary build identity without external probes
    Identity(SelfIdentityArgs),
    /// Plan or delete orphaned Homeboy runtime temp entries
    CleanupRuntimeTmp(SelfCleanupRuntimeTmpArgs),
}

#[derive(Args)]
pub struct SelfStatusArgs {}

#[derive(Args)]
pub struct SelfIdentityArgs {}

#[derive(Args)]
pub struct SelfCleanupRuntimeTmpArgs {
    /// Delete planned temp entries. Without this flag, only reports the plan.
    #[arg(long)]
    pub apply: bool,
    /// Only include entries older than this many days.
    #[arg(long, default_value_t = 7)]
    pub older_than_days: u64,
    /// Only include entries whose directory/file name starts with this prefix.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Maximum temp entries to inspect in one invocation.
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
}

pub fn run(args: SelfArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        SelfCommand::Status(_) => {
            let status = self_status::collect_status();
            let json = serde_json::to_value(status)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        SelfCommand::Identity(_) => {
            let json = serde_json::to_value(build_identity::current())
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        SelfCommand::CleanupRuntimeTmp(args) => {
            let output = engine::temp::cleanup_runtime_tmp(
                args.apply,
                args.older_than_days,
                args.prefix.as_deref(),
                args.limit,
            )?;
            let json = serde_json::to_value(output)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
    }
}
