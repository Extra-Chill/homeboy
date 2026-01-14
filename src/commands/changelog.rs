use clap::{Args, Subcommand};
use serde::Serialize;

use crate::docs;

use super::CmdResult;
use homeboy::changelog::{self, AddItemsOutput};

#[derive(Args)]
pub struct ChangelogArgs {
    #[command(subcommand)]
    pub command: Option<ChangelogCommand>,
}

#[derive(Subcommand)]
pub enum ChangelogCommand {
    /// Add changelog items to the configured "next" section
    Add {
        /// Use current working directory (ad-hoc mode with auto-detection)
        #[arg(long)]
        cwd: bool,

        /// JSON input spec for batch operations.
        ///
        /// Use "-" to read from stdin, "@file.json" to read from a file, or an inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Changelog item content (non-JSON mode)
        message: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogShowOutput {
    pub topic_label: String,
    pub content: String,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum ChangelogOutput {
    #[serde(rename_all = "camelCase")]
    Show(ChangelogShowOutput),
    #[serde(rename_all = "camelCase")]
    Add(AddItemsOutput),
}

pub fn run_markdown(args: ChangelogArgs) -> CmdResult<String> {
    match args.command {
        None => show_markdown(),
        Some(ChangelogCommand::Add { .. }) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "Markdown output is only supported for 'changelog'",
            None,
            None,
        )),
    }
}

pub fn is_show_markdown(args: &ChangelogArgs) -> bool {
    args.command.is_none()
}

pub fn run(
    args: ChangelogArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangelogOutput> {
    match args.command {
        None => {
            let (out, code) = show_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        Some(ChangelogCommand::Add {
            cwd,
            json,
            component_id,
            message,
        }) => {
            // Priority: --cwd > --json > component_id (auto-detects JSON)
            let messages: Vec<String> = message.into_iter().collect();

            if cwd {
                let output = changelog::add_items_cwd(&messages)?;
                return Ok((ChangelogOutput::Add(output), 0));
            }

            // Explicit --json takes precedence
            if let Some(spec) = json.as_deref() {
                let output = changelog::add_items_bulk(spec)?;
                return Ok((ChangelogOutput::Add(output), 0));
            }

            // Core handles auto-detection of JSON in component_id
            let output = changelog::add_items(component_id.as_deref(), &messages)?;
            Ok((ChangelogOutput::Add(output), 0))
        }
    }
}

fn show_markdown() -> CmdResult<String> {
    let resolved = docs::resolve(&["changelog".to_string()])?;

    Ok((resolved.content, 0))
}

fn show_json() -> CmdResult<ChangelogShowOutput> {
    let resolved = docs::resolve(&["changelog".to_string()])?;

    Ok((
        ChangelogShowOutput {
            topic_label: resolved.topic_label,
            content: resolved.content,
        },
        0,
    ))
}
