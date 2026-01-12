use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::docs;

use super::CmdResult;
use homeboy_core::changelog;
use homeboy_core::config::ConfigManager;

#[derive(Args)]
pub struct ChangelogArgs {
    /// JSON input spec for batch operations.
    ///
    /// Use "-" to read from stdin, "@file.json" to read from a file, or an inline JSON string.
    #[arg(long)]
    pub json: Option<String>,

    #[command(subcommand)]
    pub command: Option<ChangelogCommand>,
}

#[derive(Subcommand)]
pub enum ChangelogCommand {
    /// Add changelog items to the configured "next" section
    Add {
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
#[serde(rename_all = "camelCase")]
pub struct ChangelogAddOutput {
    pub component_id: String,
    pub changelog_path: String,
    pub next_section_label: String,
    pub messages: Vec<String>,
    pub items_added: usize,
    pub changed: bool,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum ChangelogOutput {
    #[serde(rename_all = "camelCase")]
    Show(ChangelogShowOutput),
    #[serde(rename_all = "camelCase")]
    Add(ChangelogAddOutput),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangelogAddData {
    pub component_id: String,
    pub messages: Vec<String>,
}

pub fn run_markdown(args: ChangelogArgs) -> CmdResult<String> {
    match args.command {
        None => show_markdown(),
        Some(ChangelogCommand::Add { .. }) => {
            Err(homeboy_core::Error::validation_invalid_argument(
                "command",
                "Markdown output is only supported for 'changelog'",
                None,
                None,
            ))
        }
    }
}

pub fn is_show_markdown(args: &ChangelogArgs) -> bool {
    args.command.is_none()
}

pub fn run(
    args: ChangelogArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangelogOutput> {
    let json_spec = args.json.as_deref();
    match args.command {
        None => {
            let (out, code) = show_json()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        Some(ChangelogCommand::Add {
            component_id,
            message,
        }) => {
            if let Some(spec) = json_spec {
                let data: ChangelogAddData =
                    homeboy_core::json::load_op_data(spec, "changelog.add")?;

                let (out, code) = add_next_items(&data.component_id, &data.messages)?;

                return Ok((ChangelogOutput::Add(out), code));
            }

            let component_id = component_id.ok_or_else(|| {
                homeboy_core::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId",
                    None,
                    None,
                )
            })?;

            let message = message.ok_or_else(|| {
                homeboy_core::Error::validation_invalid_argument(
                    "message",
                    "Missing message",
                    None,
                    None,
                )
            })?;

            let (out, code) = add_next_items(&component_id, &[message])?;
            Ok((ChangelogOutput::Add(out), code))
        }
    }
}

fn show_markdown() -> CmdResult<String> {
    let resolved = docs::resolve(&["changelog".to_string()]);

    if resolved.content.is_empty() {
        return Err(homeboy_core::Error::other(
            "No changelog found (expected embedded docs topic 'changelog')".to_string(),
        ));
    }

    Ok((resolved.content, 0))
}

fn show_json() -> CmdResult<ChangelogShowOutput> {
    let resolved = docs::resolve(&["changelog".to_string()]);

    if resolved.content.is_empty() {
        return Err(homeboy_core::Error::other(
            "No changelog found (expected embedded docs topic 'changelog')".to_string(),
        ));
    }

    Ok((
        ChangelogShowOutput {
            topic_label: resolved.topic_label,
            content: resolved.content,
        },
        0,
    ))
}

fn add_next_items(component_id: &str, messages: &[String]) -> CmdResult<ChangelogAddOutput> {
    let component = ConfigManager::load_component(component_id)?;

    let settings = changelog::resolve_effective_settings(Some(&component))?;
    let (path, changed, items_added) =
        changelog::read_and_add_next_section_items(&component, &settings, messages)?;

    Ok((
        ChangelogAddOutput {
            component_id: component_id.to_string(),
            changelog_path: path.to_string_lossy().to_string(),
            next_section_label: settings.next_section_label,
            messages: messages.to_vec(),
            items_added,
            changed,
        },
        0,
    ))
}
