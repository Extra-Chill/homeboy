use clap::Args;

use crate::docs;

use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {}

pub fn run_markdown(_args: InitArgs) -> CmdResult<String> {
    let topic = vec!["commands/homeboy-init".to_string()];
    let resolved = docs::resolve(&topic);

    if resolved.content.is_empty() {
        let available_topics = docs::available_topics();
        return Err(homeboy_core::Error::other(format!(
            "No documentation found for '{}' (available: {})",
            topic.join(" "),
            available_topics.join("\n")
        )));
    }

    Ok((resolved.content, 0))
}
