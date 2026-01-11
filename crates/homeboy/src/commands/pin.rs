use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use uuid::Uuid;

use homeboy_core::config::{ConfigManager, PinnedRemoteFile, PinnedRemoteLog};
use homeboy_core::{Error, Result};

#[derive(Args)]
pub struct PinArgs {
    #[command(subcommand)]
    command: PinCommand,
}

#[derive(Subcommand)]
enum PinCommand {
    /// List pinned items
    List {
        /// Project ID
        project_id: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: PinType,
    },
    /// Pin a file or log
    Add {
        /// Project ID
        project_id: String,
        /// Path to pin (relative to basePath or absolute)
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: PinType,
        /// Optional display label
        #[arg(long)]
        label: Option<String>,
        /// Number of lines to tail (logs only)
        #[arg(long, default_value = "100")]
        tail: u32,
    },
    /// Unpin a file or log
    Remove {
        /// Project ID
        project_id: String,
        /// Path to unpin
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: PinType,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum PinType {
    File,
    Log,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinOutput {
    pub command: String,
    pub project_id: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<PinListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<PinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<PinChange>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinChange {
    pub path: String,
    pub r#type: String,
}

pub fn run(args: PinArgs) -> Result<(PinOutput, i32)> {
    match args.command {
        PinCommand::List { project_id, r#type } => list(&project_id, r#type),
        PinCommand::Add {
            project_id,
            path,
            r#type,
            label,
            tail,
        } => add(&project_id, &path, r#type, label, tail),
        PinCommand::Remove {
            project_id,
            path,
            r#type,
        } => remove(&project_id, &path, r#type),
    }
}

fn list(project_id: &str, pin_type: PinType) -> Result<(PinOutput, i32)> {
    let project = ConfigManager::load_project(project_id)?;

    let (items, type_string) = match pin_type {
        PinType::File => (
            project
                .remote_files
                .pinned_files
                .iter()
                .map(|file| PinListItem {
                    path: file.path.clone(),
                    label: file.label.clone(),
                    display_name: file.display_name().to_string(),
                    tail_lines: None,
                })
                .collect::<Vec<_>>(),
            "file",
        ),
        PinType::Log => (
            project
                .remote_logs
                .pinned_logs
                .iter()
                .map(|log| PinListItem {
                    path: log.path.clone(),
                    label: log.label.clone(),
                    display_name: log.display_name().to_string(),
                    tail_lines: Some(log.tail_lines),
                })
                .collect::<Vec<_>>(),
            "log",
        ),
    };

    Ok((
        PinOutput {
            command: "pin.list".to_string(),
            project_id: project_id.to_string(),
            r#type: type_string.to_string(),
            items: Some(items),
            added: None,
            removed: None,
        },
        0,
    ))
}

fn add(
    project_id: &str,
    path: &str,
    pin_type: PinType,
    label: Option<String>,
    tail: u32,
) -> Result<(PinOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let type_string = match pin_type {
        PinType::File => {
            if project
                .remote_files
                .pinned_files
                .iter()
                .any(|file| file.path == path)
            {
                return Err(Error::Other(format!("File '{}' is already pinned", path)));
            }

            project.remote_files.pinned_files.push(PinnedRemoteFile {
                id: Uuid::new_v4(),
                path: path.to_string(),
                label,
            });

            "file"
        }
        PinType::Log => {
            if project
                .remote_logs
                .pinned_logs
                .iter()
                .any(|log| log.path == path)
            {
                return Err(Error::Other(format!("Log '{}' is already pinned", path)));
            }

            project.remote_logs.pinned_logs.push(PinnedRemoteLog {
                id: Uuid::new_v4(),
                path: path.to_string(),
                label,
                tail_lines: tail,
            });

            "log"
        }
    };

    ConfigManager::save_project(&project)?;

    Ok((
        PinOutput {
            command: "pin.add".to_string(),
            project_id: project_id.to_string(),
            r#type: type_string.to_string(),
            items: None,
            added: Some(PinChange {
                path: path.to_string(),
                r#type: type_string.to_string(),
            }),
            removed: None,
        },
        0,
    ))
}

fn remove(project_id: &str, path: &str, pin_type: PinType) -> Result<(PinOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let (removed, type_string) = match pin_type {
        PinType::File => {
            let original_len = project.remote_files.pinned_files.len();
            project
                .remote_files
                .pinned_files
                .retain(|file| file.path != path);

            (
                project.remote_files.pinned_files.len() < original_len,
                "file",
            )
        }
        PinType::Log => {
            let original_len = project.remote_logs.pinned_logs.len();
            project
                .remote_logs
                .pinned_logs
                .retain(|log| log.path != path);

            (project.remote_logs.pinned_logs.len() < original_len, "log")
        }
    };

    if !removed {
        return Err(Error::Other(format!(
            "{} '{}' is not pinned",
            type_string, path
        )));
    }

    ConfigManager::save_project(&project)?;

    Ok((
        PinOutput {
            command: "pin.remove".to_string(),
            project_id: project_id.to_string(),
            r#type: type_string.to_string(),
            items: None,
            added: None,
            removed: Some(PinChange {
                path: path.to_string(),
                r#type: type_string.to_string(),
            }),
        },
        0,
    ))
}
