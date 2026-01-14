use clap::Args;
use homeboy::context;
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct ContextArgs {
    /// Discover git repositories in subdirectories
    #[arg(long)]
    pub discover: bool,

    /// Max depth for --discover (default: 2)
    #[arg(long, default_value = "2")]
    pub depth: usize,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ContextCommandOutput {
    Context(context::ContextOutput),
    Discover(context::DiscoverOutput),
}

pub fn run(
    args: ContextArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ContextCommandOutput> {
    if args.discover {
        let (output, code) = context::discover(None, args.depth)?;
        return Ok((ContextCommandOutput::Discover(output), code));
    }

    let (output, code) = context::run(None)?;
    Ok((ContextCommandOutput::Context(output), code))
}
