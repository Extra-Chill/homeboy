use clap::Args;
use serde::Serialize;

use crate::docs;

use super::CmdResult;

#[derive(Args)]
pub struct DocsArgs {
    /// Topic to filter (e.g., 'deploy', 'project set')
    #[arg(trailing_var_arg = true)]
    topic: Vec<String>,
}

#[derive(Serialize)]
pub struct DocsOutput {
    pub topic: String,
    pub topic_label: String,
    pub content: String,
    pub available_topics: String,
}

pub fn run(args: DocsArgs) -> CmdResult<DocsOutput> {
    let (topic_label, content) = docs::resolve(&args.topic);

    if content.is_empty() {
        return Err(homeboy_core::Error::Other(format!(
            "No documentation found for '{}' (available: {})",
            args.topic.join(" "),
            docs::available_topics()
        )));
    }

    Ok((
        DocsOutput {
            topic: args.topic.join(" "),
            topic_label,
            content,
            available_topics: docs::available_topics().to_string(),
        },
        0,
    ))
}
