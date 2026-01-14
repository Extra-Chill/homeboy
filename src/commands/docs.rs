use clap::Args;
use serde::Serialize;

use crate::docs;

use super::CmdResult;

/// Strip --format flag from topic args before passing to docs lookup.
/// This is CLI-specific preprocessing (clap passes trailing_var_arg including global flags).
fn strip_format_flag(topic: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    let mut skip_next = false;

    for arg in topic {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--format" {
            skip_next = true;
            continue;
        }
        cleaned.push(arg.clone());
    }
    cleaned
}

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
    pub source: String,
    pub available_topics: Vec<String>,
}

#[derive(Serialize)]
pub struct DocsListOutput {
    pub available_topics: Vec<String>,
}

pub fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    if args.list {
        return Err(homeboy::Error::validation_invalid_argument(
            "list",
            "Cannot use --list with markdown output",
            None,
            None,
        ));
    }

    let topic = strip_format_flag(&args.topic);
    let resolved = docs::resolve(&topic)?;

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

    let topic = strip_format_flag(&args.topic);
    let resolved = docs::resolve(&topic)?;

    let topic_str = topic.join(" ");
    let slug = resolved
        .segments
        .last()
        .cloned()
        .unwrap_or_else(|| "index".to_string());

    Ok((
        DocsOutput::Content(DocsContentOutput {
            topic: topic_str,
            topic_label: resolved.topic_label,
            resolved_key: resolved.key,
            segments: resolved.segments,
            slug,
            content: resolved.content,
            source: resolved.source,
            available_topics: docs::available_topics(),
        }),
        0,
    ))
}
