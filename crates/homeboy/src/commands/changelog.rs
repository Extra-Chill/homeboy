use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::docs;

use homeboy_core::changelog;
use homeboy_core::config::ConfigManager;

use super::CmdResult;

#[derive(Args)]
pub struct ChangelogArgs {
    #[command(subcommand)]
    command: ChangelogCommand,
}

#[derive(Subcommand)]
enum ChangelogCommand {
    /// Show the embedded Homeboy CLI changelog documentation
    Show,

    /// Add changelog items to the configured "next" section
    Add {
        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Changelog item content (non-JSON mode)
        message: Option<String>,

        /// Optional project ID override (non-JSON mode; defaults to active project)
        #[arg(long)]
        project_id: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
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
    #[serde(default)]
    pub project_id: Option<String>,
    pub messages: Vec<String>,
}

pub fn run(args: ChangelogArgs, json_spec: Option<&str>) -> CmdResult<ChangelogOutput> {
    match args.command {
        ChangelogCommand::Show => {
            let (out, code) = show()?;
            Ok((ChangelogOutput::Show(out), code))
        }
        ChangelogCommand::Add {
            component_id,
            message,
            project_id,
        } => {
            if let Some(spec) = json_spec {
                let data: ChangelogAddData =
                    homeboy_core::json::load_op_data(spec, "changelog.add")?;

                let (out, code) = add_next_items(
                    &data.component_id,
                    &data.messages,
                    data.project_id.as_deref(),
                )?;

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

            let (out, code) = add_next_items(&component_id, &[message], project_id.as_deref())?;
            Ok((ChangelogOutput::Add(out), code))
        }
    }
}

fn show() -> CmdResult<ChangelogShowOutput> {
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

fn resolve_project_id(project_id_override: Option<&str>) -> homeboy_core::Result<Option<String>> {
    if let Some(project_id) = project_id_override {
        return Ok(Some(project_id.to_string()));
    }

    let app = ConfigManager::load_app_config()?;
    Ok(app.active_project_id)
}

fn add_next_items(
    component_id: &str,
    messages: &[String],
    project_id_override: Option<&str>,
) -> CmdResult<ChangelogAddOutput> {
    let component = ConfigManager::load_component(component_id)?;

    let project_id = resolve_project_id(project_id_override)?;
    let project = match project_id.as_deref() {
        Some(id) => Some(ConfigManager::load_project(id)?),
        None => None,
    };

    let settings = changelog::resolve_effective_settings(project.as_ref(), Some(&component))?;
    let (path, changed, items_added) =
        changelog::read_and_add_next_section_items(&component, &settings, messages)?;

    Ok((
        ChangelogAddOutput {
            component_id: component_id.to_string(),
            project_id,
            changelog_path: path.to_string_lossy().to_string(),
            next_section_label: settings.next_section_label,
            messages: messages.to_vec(),
            items_added,
            changed,
        },
        0,
    ))
}
