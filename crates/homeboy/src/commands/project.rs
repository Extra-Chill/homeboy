use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy_core::config::{
    ConfigManager, PinnedRemoteFile, PinnedRemoteLog, ProjectManager, ProjectRecord,
};
use uuid::Uuid;

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// List all configured projects
    List {
        /// Show only the active project ID
        #[arg(long)]
        current: bool,
    },
    /// Show project configuration
    Show {
        /// Project ID (uses active project if not specified)
        project_id: Option<String>,
    },
    /// Create a new project
    Create {
        /// Project name
        name: String,
        /// Public site domain
        domain: String,
        /// Project type (e.g. wordpress)
        project_type: String,
        /// Optional server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Optional remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Optional WordPress table prefix
        #[arg(long)]
        table_prefix: Option<String>,
        /// Switch active project after create
        #[arg(long)]
        activate: bool,
    },
    /// Update project configuration fields
    Set {
        /// Project ID
        project_id: String,
        /// Project name
        #[arg(long)]
        name: Option<String>,
        /// Public site domain
        #[arg(long)]
        domain: Option<String>,
        /// Project type (e.g. wordpress)
        #[arg(long)]
        project_type: Option<String>,
        /// Server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// WordPress table prefix
        #[arg(long)]
        table_prefix: Option<String>,
    },
    /// Switch active project
    Switch {
        /// Project ID to switch to
        project_id: String,
    },
    /// Repair a project file whose name doesn't match the stored project name
    Repair {
        /// Project ID (file stem)
        project_id: String,
    },
    /// Manage pinned files and logs
    Pin {
        #[command(subcommand)]
        command: ProjectPinCommand,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListItem {
    id: String,
    name: String,
    domain: String,
    project_type: String,
    active: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinOutput {
    pub action: String,
    pub project_id: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ProjectPinListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<ProjectPinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<ProjectPinChange>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinChange {
    pub path: String,
    pub r#type: String,
}

#[derive(Subcommand)]
enum ProjectPinCommand {
    /// List pinned items
    List {
        /// Project ID
        project_id: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
    },
    /// Pin a file or log
    Add {
        /// Project ID
        project_id: String,
        /// Path to pin (relative to basePath or absolute)
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
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
        r#type: ProjectPinType,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ProjectPinType {
    File,
    Log,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    command: String,
    project_id: Option<String>,
    active_project_id: Option<String>,
    project: Option<ProjectRecord>,
    projects: Option<Vec<ProjectListItem>>,
    pin: Option<ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<Vec<String>>,
}

pub fn run(args: ProjectArgs) -> homeboy_core::Result<(ProjectOutput, i32)> {
    match args.command {
        ProjectCommand::List { current } => list(current),
        ProjectCommand::Show { project_id } => show(project_id),
        ProjectCommand::Create {
            name,
            domain,
            project_type,
            server_id,
            base_path,
            table_prefix,
            activate,
        } => create(
            &name,
            &domain,
            &project_type,
            server_id,
            base_path,
            table_prefix,
            activate,
        ),
        ProjectCommand::Set {
            project_id,
            name,
            domain,
            project_type,
            server_id,
            base_path,
            table_prefix,
        } => set(
            &project_id,
            name,
            domain,
            project_type,
            server_id,
            base_path,
            table_prefix,
        ),
        ProjectCommand::Switch { project_id } => switch(&project_id),
        ProjectCommand::Repair { project_id } => repair(&project_id),
        ProjectCommand::Pin { command } => pin(command),
    }
}

fn list(current: bool) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let app_config = ConfigManager::load_app_config()?;
    let active_id = app_config.active_project_id.clone();

    if current {
        return Ok((
            ProjectOutput {
                command: "project.current".to_string(),
                project_id: None,
                active_project_id: active_id,
                project: None,
                projects: None,
                pin: None,
                updated: None,
            },
            0,
        ));
    }

    let projects = ConfigManager::list_projects()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|record| ProjectListItem {
            active: active_id.as_ref().is_some_and(|a| a == &record.id),
            id: record.id,
            name: record.config.name,
            domain: record.config.domain,
            project_type: record.config.project_type,
        })
        .collect();

    Ok((
        ProjectOutput {
            command: "project.list".to_string(),
            project_id: None,
            active_project_id: active_id,
            project: None,
            projects: Some(items),
            pin: None,
            updated: None,
        },
        0,
    ))
}

fn show(project_id: Option<String>) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let project = match project_id.clone() {
        Some(id) => ConfigManager::load_project_record(&id)?,
        None => ConfigManager::get_active_project()?,
    };

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            project_id: Some(project.id.clone()),
            active_project_id: None,
            project: Some(project),
            projects: None,
            pin: None,
            updated: None,
        },
        0,
    ))
}

fn create(
    name: &str,
    domain: &str,
    project_type: &str,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
    activate: bool,
) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let (created_project_id, _project) = ProjectManager::create_project(
        name,
        domain,
        project_type,
        server_id,
        base_path,
        table_prefix,
    )?;

    if activate {
        ConfigManager::set_active_project(&created_project_id)?;
    }

    let project = ConfigManager::load_project_record(&created_project_id)?;

    Ok((
        ProjectOutput {
            command: "project.create".to_string(),
            project_id: Some(created_project_id),
            active_project_id: None,
            project: Some(project),
            projects: None,
            pin: None,
            updated: None,
        },
        0,
    ))
}

fn set(
    project_id: &str,
    name: Option<String>,
    domain: Option<String>,
    project_type: Option<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let mut updated_fields: Vec<String> = Vec::new();

    if let Some(name) = name {
        let result = ProjectManager::rename_project(project_id, &name)?;
        updated_fields.push("name".to_string());

        if result.new_id != project_id {
            updated_fields.push("id".to_string());
        }

        return Ok((
            ProjectOutput {
                command: "project.set".to_string(),
                project_id: Some(result.new_id.clone()),
                active_project_id: None,
                project: Some(ConfigManager::load_project_record(&result.new_id)?),
                projects: None,
                pin: None,
                updated: Some(updated_fields),
            },
            0,
        ));
    }

    let mut project = ConfigManager::load_project(project_id)?;

    if let Some(domain) = domain {
        project.domain = domain;
        updated_fields.push("domain".to_string());
    }

    if let Some(project_type) = project_type {
        project.project_type = project_type;
        updated_fields.push("projectType".to_string());
    }

    if let Some(server_id) = server_id {
        project.server_id = Some(server_id);
        updated_fields.push("serverId".to_string());
    }

    if let Some(base_path) = base_path {
        project.base_path = Some(base_path);
        updated_fields.push("basePath".to_string());
    }

    if let Some(table_prefix) = table_prefix {
        project.table_prefix = Some(table_prefix);
        updated_fields.push("tablePrefix".to_string());
    }

    if updated_fields.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No fields provided to update".to_string(),
        ));
    }

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.set".to_string(),
            project_id: Some(project_id.to_string()),
            active_project_id: None,
            project: Some(ConfigManager::load_project_record(project_id)?),
            projects: None,
            pin: None,
            updated: Some(updated_fields),
        },
        0,
    ))
}

fn switch(project_id: &str) -> homeboy_core::Result<(ProjectOutput, i32)> {
    ConfigManager::set_active_project(project_id)?;

    let project = ConfigManager::load_project_record(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.switch".to_string(),
            project_id: Some(project_id.to_string()),
            active_project_id: None,
            project: Some(project),
            projects: None,
            pin: None,
            updated: None,
        },
        0,
    ))
}

fn repair(project_id: &str) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let result = ProjectManager::repair_project(project_id)?;

    let updated = if result.new_id != result.old_id {
        Some(vec!["id".to_string()])
    } else {
        None
    };

    Ok((
        ProjectOutput {
            command: "project.repair".to_string(),
            project_id: Some(result.new_id.clone()),
            active_project_id: None,
            project: Some(ConfigManager::load_project_record(&result.new_id)?),
            projects: None,
            pin: None,
            updated,
        },
        0,
    ))
}

fn pin(command: ProjectPinCommand) -> homeboy_core::Result<(ProjectOutput, i32)> {
    match command {
        ProjectPinCommand::List { project_id, r#type } => pin_list(&project_id, r#type),
        ProjectPinCommand::Add {
            project_id,
            path,
            r#type,
            label,
            tail,
        } => pin_add(&project_id, &path, r#type, label, tail),
        ProjectPinCommand::Remove {
            project_id,
            path,
            r#type,
        } => pin_remove(&project_id, &path, r#type),
    }
}

fn pin_list(
    project_id: &str,
    pin_type: ProjectPinType,
) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let project = ConfigManager::load_project(project_id)?;

    let (items, type_string) = match pin_type {
        ProjectPinType::File => (
            project
                .remote_files
                .pinned_files
                .iter()
                .map(|file| ProjectPinListItem {
                    path: file.path.clone(),
                    label: file.label.clone(),
                    display_name: file.display_name().to_string(),
                    tail_lines: None,
                })
                .collect(),
            "file",
        ),
        ProjectPinType::Log => (
            project
                .remote_logs
                .pinned_logs
                .iter()
                .map(|log| ProjectPinListItem {
                    path: log.path.clone(),
                    label: log.label.clone(),
                    display_name: log.display_name().to_string(),
                    tail_lines: Some(log.tail_lines),
                })
                .collect(),
            "log",
        ),
    };

    Ok((
        ProjectOutput {
            command: "project.pin.list".to_string(),
            project_id: Some(project_id.to_string()),
            active_project_id: None,
            project: None,
            projects: None,
            pin: Some(ProjectPinOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: Some(items),
                added: None,
                removed: None,
            }),
            updated: None,
        },
        0,
    ))
}

fn pin_add(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
    label: Option<String>,
    tail: u32,
) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let type_string = match pin_type {
        ProjectPinType::File => {
            if project
                .remote_files
                .pinned_files
                .iter()
                .any(|file| file.path == path)
            {
                return Err(homeboy_core::Error::Other(format!(
                    "File '{}' is already pinned",
                    path
                )));
            }

            project.remote_files.pinned_files.push(PinnedRemoteFile {
                id: Uuid::new_v4(),
                path: path.to_string(),
                label,
            });

            "file"
        }
        ProjectPinType::Log => {
            if project
                .remote_logs
                .pinned_logs
                .iter()
                .any(|log| log.path == path)
            {
                return Err(homeboy_core::Error::Other(format!(
                    "Log '{}' is already pinned",
                    path
                )));
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

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.pin.add".to_string(),
            project_id: Some(project_id.to_string()),
            active_project_id: None,
            project: None,
            projects: None,
            pin: Some(ProjectPinOutput {
                action: "add".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: None,
                added: Some(ProjectPinChange {
                    path: path.to_string(),
                    r#type: type_string.to_string(),
                }),
                removed: None,
            }),
            updated: None,
        },
        0,
    ))
}

fn pin_remove(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let (removed, type_string) = match pin_type {
        ProjectPinType::File => {
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
        ProjectPinType::Log => {
            let original_len = project.remote_logs.pinned_logs.len();
            project
                .remote_logs
                .pinned_logs
                .retain(|log| log.path != path);

            (project.remote_logs.pinned_logs.len() < original_len, "log")
        }
    };

    if !removed {
        return Err(homeboy_core::Error::Other(format!(
            "{} '{}' is not pinned",
            type_string, path
        )));
    }

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            project_id: Some(project_id.to_string()),
            active_project_id: None,
            project: None,
            projects: None,
            pin: Some(ProjectPinOutput {
                action: "remove".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: None,
                added: None,
                removed: Some(ProjectPinChange {
                    path: path.to_string(),
                    r#type: type_string.to_string(),
                }),
            }),
            updated: None,
        },
        0,
    ))
}
