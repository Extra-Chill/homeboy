use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;
use homeboy::changelog::{self, ShowOutput};

#[derive(Args)]
pub struct ChangelogArgs {
    /// Show Homeboy's own changelog (release notes)
    #[arg(long = "self")]
    pub show_self: bool,

    #[command(subcommand)]
    pub command: Option<ChangelogCommand>,
}

#[derive(Subcommand)]
pub enum ChangelogCommand {
    /// Show a changelog (Homeboy's own if no component specified)
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
    match (&args.command, args.show_self) {
        (None, true) => show_homeboy_markdown(),
        (Some(ChangelogCommand::Show { component_id: None }), _) => show_homeboy_markdown(),
        (
            Some(ChangelogCommand::Show {
                component_id: Some(id),
            }),
            _,
        ) => {
            let output = changelog::show(id)?;
            Ok((output.content, 0))
        }
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use 'show' or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog show".to_string(),
                "homeboy changelog show <component_id>".to_string(),
            ]),
        )),
    }
}

pub fn is_show_markdown(args: &ChangelogArgs) -> bool {
    matches!(args.command, Some(ChangelogCommand::Show { .. }))
        || (args.command.is_none() && args.show_self)
}

pub fn run(
    args: ChangelogArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangelogOutput> {
    match (&args.command, args.show_self) {
        (None, true) => {
            let (out, code) = show_homeboy_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        (Some(ChangelogCommand::Show { component_id: None }), _) => {
            let (out, code) = show_homeboy_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        (
            Some(ChangelogCommand::Show {
                component_id: Some(id),
            }),
            _,
        ) => {
            let output = changelog::show(id)?;
            Ok((ChangelogOutput::ShowComponent(output), 0))
        }
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use 'show' or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog show".to_string(),
                "homeboy changelog show <component_id>".to_string(),
            ]),
        )),
    }
}

// Homeboy's own changelog is embedded separately from the docs system
// to avoid collision with docs/commands/changelog.md command docs.
const HOMEBOY_CHANGELOG: &str = include_str!("../../docs/changelog.md");

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
