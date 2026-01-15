use clap::{Args, Subcommand};
use serde::Serialize;

use crate::docs;

use super::CmdResult;
use homeboy::changelog::{self, AddItemsOutput, InitOutput};

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

    /// Initialize a new changelog file
    Init {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// Path for the changelog file (relative to component/cwd)
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

    Add(AddItemsOutput),

    Init(InitOutput),
}

pub fn run_markdown(args: ChangelogArgs) -> CmdResult<String> {
    match (&args.command, args.show_self) {
        (None, true) => show_markdown(),
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use a subcommand (add, init) or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog add <component_id> <message>".to_string(),
                "homeboy changelog init <component_id>".to_string(),
                "homeboy changelog --self".to_string(),
            ]),
        )),
        (Some(ChangelogCommand::Add { .. }) | Some(ChangelogCommand::Init { .. }), _) => {
            Err(homeboy::Error::validation_invalid_argument(
                "command",
                "Markdown output is only supported for 'changelog --self'",
                None,
                None,
            ))
        }
    }
}

pub fn is_show_markdown(args: &ChangelogArgs) -> bool {
    args.command.is_none() && args.show_self
}

pub fn run(
    args: ChangelogArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangelogOutput> {
    match (&args.command, args.show_self) {
        (None, true) => {
            let (out, code) = show_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        (None, false) => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "No subcommand provided. Use a subcommand (add, init) or --self to view Homeboy's changelog",
            None,
            Some(vec![
                "homeboy changelog add <component_id> <message>".to_string(),
                "homeboy changelog init <component_id>".to_string(),
                "homeboy changelog --self".to_string(),
            ]),
        )),
        (Some(ChangelogCommand::Add {
            cwd,
            json,
            component_id,
            message,
        }), _) => {
            // Priority: --cwd > --json > component_id (auto-detects JSON)
            let messages: Vec<String> = message.iter().cloned().collect();

            if *cwd {
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
        (Some(ChangelogCommand::Init {
            cwd,
            path,
            configure,
            component_id,
        }), _) => {
            if *cwd {
                let output = changelog::init_cwd(path.as_deref())?;
                return Ok((ChangelogOutput::Init(output), 0));
            }

            let id = component_id.as_ref().ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing component ID (or use --cwd). List components: homeboy component list",
                    None,
                    None,
                )
            })?;

            let output = changelog::init(id, path.as_deref(), *configure)?;
            Ok((ChangelogOutput::Init(output), 0))
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
