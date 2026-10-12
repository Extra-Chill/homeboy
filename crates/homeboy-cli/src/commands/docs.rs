use clap::Args;

use crate::help_topics;

use super::CmdResult;

#[derive(Args)]
pub struct DocsArgs {
    /// Topic path (e.g., 'commands/deploy') or 'list' to show available topics
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,
}

/// Render a documentation topic (or the topic list) as markdown.
pub(crate) fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    let topic = args.topic.as_deref().unwrap_or("index");

    if topic == "list" {
        let topics = help_topics::available_topics();
        return Ok((topics.join("\n"), 0));
    }

    let topic_vec = vec![topic.to_string()];
    let resolved = help_topics::resolve(&topic_vec)?;
    Ok((resolved.content, 0))
}
