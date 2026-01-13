use clap::Args;
use homeboy::context;

use super::CmdResult;

#[derive(Args)]
pub struct ContextArgs {}

pub fn run(_args: ContextArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<context::ContextOutput> {
    context::run(None)
}
