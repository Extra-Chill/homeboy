use clap::Args;

use crate::docs;

use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {}

pub fn run_markdown(_args: InitArgs) -> CmdResult<String> {
    let topic = vec!["commands/homeboy-init".to_string()];
    let resolved = docs::resolve(&topic)?;

    Ok((resolved.content, 0))
}
