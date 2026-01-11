use clap::Args;
use serde::Serialize;

use crate::docs;

use super::CmdResult;

#[derive(Args)]
pub struct DocsArgs {
    /// List available topics and exit
    #[arg(long)]
    pub list: bool,

    /// Topic to filter (e.g., 'deploy', 'project set')
    #[arg(trailing_var_arg = true)]
    pub topic: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DocsOutput {
    Content(DocsContentOutput),
    List(DocsListOutput),
}

#[derive(Serialize)]
pub struct DocsContentOutput {
    pub topic: String,
    pub topic_label: String,
    pub resolved_key: String,
    pub segments: Vec<String>,
    pub slug: String,
    pub content: String,
    pub available_topics: Vec<String>,
}

#[derive(Serialize)]
pub struct DocsListOutput {
    pub available_topics: Vec<String>,
}

pub fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    if args.list {
        return Err(homeboy_core::Error::validation_invalid_argument(
            "list",
            "Cannot use --list with markdown output",
            None,
            None,
        ));
    }

    let resolved = docs::resolve(&args.topic);

    if resolved.content.is_empty() {
        let available_topics = docs::available_topics();

        return Err(homeboy_core::Error::other(format!(
            "No documentation found for '{}' (available: {})",
            args.topic.join(" "),
            available_topics.join("\n")
        )));
    }

    Ok((resolved.content, 0))
}

pub fn run(args: DocsArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DocsOutput> {
    if args.list {
        return Ok((
            DocsOutput::List(DocsListOutput {
                available_topics: docs::available_topics(),
            }),
            0,
        ));
    }

    let resolved = docs::resolve(&args.topic);

    if resolved.content.is_empty() {
        let available_topics = docs::available_topics();

        return Err(homeboy_core::Error::other(format!(
            "No documentation found for '{}' (available: {})",
            args.topic.join(" "),
            available_topics.join("\n")
        )));
    }

    let topic = args.topic.join(" ");
    let slug = resolved
        .segments
        .last()
        .cloned()
        .unwrap_or_else(|| "index".to_string());

    Ok((
        DocsOutput::Content(DocsContentOutput {
            topic,
            topic_label: resolved.topic_label,
            resolved_key: resolved.key,
            segments: resolved.segments,
            slug,
            content: resolved.content,
            available_topics: docs::available_topics(),
        }),
        0,
    ))
}
