use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::CmdResult;
use homeboy::core::release::changelog::{self, ShowOutput};

#[derive(Args)]
pub struct ChangelogArgs {
    #[command(subcommand)]
    pub command: Option<ChangelogCommand>,
}

#[derive(Subcommand)]
pub enum ChangelogCommand {
    /// Show Homeboy's changelog, or a component changelog when an ID is provided
    Show {
        /// Component ID to show changelog for
        component_id: Option<String>,
    },
}

#[derive(Serialize)]

pub struct ChangelogShowOutput {
    pub topic_label: String,
    pub content: String,
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum ChangelogOutput {
    Show(ChangelogShowOutput),

    ShowComponent(ShowOutput),
}

pub fn run_markdown(args: ChangelogArgs) -> CmdResult<String> {
    match &args.command {
        None => show_homeboy_markdown(),
        Some(ChangelogCommand::Show { component_id: None }) => show_homeboy_markdown(),
        Some(ChangelogCommand::Show {
            component_id: Some(id),
        }) => {
            let output = changelog::show(id)?;
            Ok((output.content, 0))
        }
    }
}

pub fn is_show_markdown(args: &ChangelogArgs) -> bool {
    matches!(args.command, Some(ChangelogCommand::Show { .. })) || args.command.is_none()
}

pub fn run(
    args: ChangelogArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangelogOutput> {
    match &args.command {
        None => {
            let (out, code) = show_homeboy_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        Some(ChangelogCommand::Show { component_id: None }) => {
            let (out, code) = show_homeboy_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        Some(ChangelogCommand::Show {
            component_id: Some(id),
        }) => {
            let output = changelog::show(id)?;
            Ok((ChangelogOutput::ShowComponent(output), 0))
        }
    }
}

// Homeboy's own changelog is embedded separately from the docs system
// to avoid collision with command docs.
const HOMEBOY_CHANGELOG: &str = include_str!("../../../../../docs/changelog.md");

fn show_homeboy_markdown() -> CmdResult<String> {
    Ok((HOMEBOY_CHANGELOG.to_string(), 0))
}

fn show_homeboy_json() -> CmdResult<ChangelogShowOutput> {
    Ok((
        ChangelogShowOutput {
            topic_label: "changelog".to_string(),
            content: HOMEBOY_CHANGELOG.to_string(),
        },
        0,
    ))
}
