use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;
use homeboy::changelog::{self, AddItemsOutput, InitOutput, ShowOutput};

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

    /// Add changelog items to the configured "next" section
    Add {
        /// JSON input spec for batch operations.
        ///
        /// Use "-" to read from stdin, "@file.json" to read from a file, or an inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Changelog item content (positional, for backward compatibility)
        positional_message: Option<String>,

        /// Changelog message (repeatable: -m "first" -m "second")
        #[arg(short = 'm', long = "message", action = clap::ArgAction::Append)]
        messages: Vec<String>,

        /// Changelog subsection type (Added, Changed, Deprecated, Removed, Fixed, Security, Refactored)
        #[arg(short = 't', long = "type")]
        entry_type: Option<String>,
    },

    /// Initialize a new changelog file
    Init {
        /// Path for the changelog file (relative to component)
        #[arg(long)]
        path: Option<String>,

        /// Also update component config to add changelogTargets
        #[arg(long)]
        configure: bool,

        /// Component ID
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

    Add(AddItemsOutput),

    Init(InitOutput),
}

pub fn run_markdown(args: ChangelogArgs) -> CmdResult<String> {
    match (&args.command, args.show_self) {
        (None, true) => show_homeboy_markdown(),
        (Some(ChangelogCommand::Show { component_id: None }), _) => show_homeboy_markdown(),
        (Some(ChangelogCommand::Show { component_id: Some(id) }), _) => {
            let output = changelog::show(id)?;
            Ok((output.content, 0))
        }
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use a subcommand (add, init, show) or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog add <component_id> <message>".to_string(),
                "homeboy changelog init <component_id>".to_string(),
                "homeboy changelog show".to_string(),
                "homeboy changelog show <component_id>".to_string(),
            ]),
        )),
        (Some(ChangelogCommand::Add { .. }) | Some(ChangelogCommand::Init { .. }), _) => {
            Err(homeboy::Error::validation_invalid_argument(
                "command",
                "Markdown output is only supported for 'changelog show'",
                None,
                None,
            ))
        }
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
        (Some(ChangelogCommand::Show { component_id: Some(id) }), _) => {
            let output = changelog::show(id)?;
            Ok((ChangelogOutput::ShowComponent(output), 0))
        }
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use a subcommand (add, init, show) or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog add <component_id> <message>".to_string(),
                "homeboy changelog init <component_id>".to_string(),
                "homeboy changelog show".to_string(),
                "homeboy changelog show <component_id>".to_string(),
            ]),
        )),
        (Some(ChangelogCommand::Add {
            json,
            component_id,
            positional_message,
            messages,
            entry_type,
        }), _) => {
            // Priority: --json > component_id (auto-detects JSON)
            // Merge positional message with -m flags (positional goes first)
            let mut all_messages: Vec<String> = Vec::new();
            if let Some(msg) = positional_message {
                all_messages.push(msg.clone());
            }
            all_messages.extend(messages.iter().cloned());

            // Explicit --json takes precedence
            if let Some(spec) = json.as_deref() {
                let output = changelog::add_items_bulk(spec)?;
                return Ok((ChangelogOutput::Add(output), 0));
            }

            // Core handles auto-detection of JSON in component_id
            let output = changelog::add_items(component_id.as_deref(), &all_messages, entry_type.as_deref())?;
            Ok((ChangelogOutput::Add(output), 0))
        }
        (Some(ChangelogCommand::Init {
            path,
            configure,
            component_id,
        }), _) => {
            let id = component_id.as_ref().ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId",
                    None,
                    Some(vec![
                        "Provide a component ID: homeboy changelog init <component-id>".to_string(),
                        "List available components: homeboy component list".to_string(),
                    ]),
                )
            })?;

            let output = changelog::init(id, path.as_deref(), *configure)?;
            Ok((ChangelogOutput::Init(output), 0))
        }
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
