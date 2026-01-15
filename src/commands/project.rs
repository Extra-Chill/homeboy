use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project};

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// List all configured projects
    List,
    /// Show project configuration
    Show {
        /// Project ID
        project_id: String,
    },
    /// Create a new project
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Project ID (CLI mode)
        id: Option<String>,
        /// Public site domain (CLI mode)
        domain: Option<String>,
        /// Optional server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Optional remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Optional table prefix
        #[arg(long)]
        table_prefix: Option<String>,
    },
    /// Update project configuration fields
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Project ID (optional if provided in JSON body)
        project_id: Option<String>,
        /// JSON spec (positional, supports @file and - for stdin)
        spec: Option<String>,
        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
    /// Remove items from project configuration arrays
    Remove {
        /// Project ID (optional if provided in JSON body)
        project_id: Option<String>,
        /// JSON spec (positional, supports @file and - for stdin)
        spec: Option<String>,
        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
    /// Rename a project (changes ID)
    Rename {
        /// Current project ID
        project_id: String,
        /// New project ID
        new_id: String,
    },
    /// Manage project components
    Components {
        #[command(subcommand)]
        command: ProjectComponentsCommand,
    },
    /// Manage pinned files and logs
    Pin {
        #[command(subcommand)]
        command: ProjectPinCommand,
    },
    /// Delete a project configuration
    Delete {
        /// Project ID
        project_id: String,
    },
}

#[derive(Subcommand)]
enum ProjectComponentsCommand {
    /// List associated components
    List {
        /// Project ID
        project_id: String,
    },
    /// Replace project components with the provided list
    Set {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Add one or more components
    Add {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Remove one or more components
    Remove {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Remove all components
    Clear {
        /// Project ID
        project_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub components: Vec<Component>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<Project>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<Vec<ProjectListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<ProjectComponentsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import: Option<project::CreateSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch: Option<project::CreateSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

pub fn run(
    args: ProjectArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy::Result<(ProjectOutput, i32)> {
    match args.command {
        ProjectCommand::List => list(),
        ProjectCommand::Show { project_id } => show(&project_id),
        ProjectCommand::Create {
            json,
            skip_existing,
            id,
            domain,
            server_id,
            base_path,
            table_prefix,
        } => {
            // Pass all args to unified core function - it handles auto-detection
            match project::create(
                json.as_deref().or(id.as_deref()),
                domain.as_deref(),
                server_id.as_deref(),
                base_path.as_deref(),
                table_prefix.as_deref(),
                skip_existing,
            )? {
                project::CreateOutput::Single(result) => Ok((
                    ProjectOutput {
                        command: "project.create".to_string(),
                        project_id: Some(result.id),
                        project: Some(result.project),
                        ..Default::default()
                    },
                    0,
                )),
                project::CreateOutput::Bulk(summary) => {
                    let exit_code = if summary.errors > 0 { 1 } else { 0 };
                    Ok((
                        ProjectOutput {
                            command: "project.create".to_string(),
                            import: Some(summary),
                            ..Default::default()
                        },
                        exit_code,
                    ))
                }
            }
        }
        ProjectCommand::Set {
            project_id,
            spec,
            json,
        } => {
            let json_spec = json.or(spec).ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "spec",
                    "Provide JSON spec or use --json flag",
                    None,
                    None,
                )
            })?;
            set(project_id.as_deref(), &json_spec)
        }
        ProjectCommand::Remove {
            project_id,
            spec,
            json,
        } => {
            let json_spec = json.or(spec).ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "spec",
                    "Provide JSON spec or use --json flag",
                    None,
                    None,
                )
            })?;
            remove(project_id.as_deref(), &json_spec)
        }
        ProjectCommand::Rename { project_id, new_id } => rename(&project_id, &new_id),
        ProjectCommand::Components { command } => components(command),
        ProjectCommand::Pin { command } => pin(command),
        ProjectCommand::Delete { project_id } => delete(&project_id),
    }
}

fn list() -> homeboy::Result<(ProjectOutput, i32)> {
    let projects = project::list()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|p| ProjectListItem {
            id: p.id,
            domain: p.domain,
        })
        .collect();

    Ok((
        ProjectOutput {
            command: "project.list".to_string(),
            projects: Some(items),
            ..Default::default()
        },
        0,
    ))
}

fn show(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = project::load(project_id)?;

    let hint = if project.component_ids.is_empty() {
        Some(format!(
            "No components linked. Use: homeboy project components add {} <component-id>",
            project.id
        ))
    } else {
        None
    };

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            project_id: Some(project.id.clone()),
            project: Some(project),
            hint,
            ..Default::default()
        },
        0,
    ))
}

fn set(project_id: Option<&str>, json: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    match project::merge(project_id, json)? {
        homeboy::MergeOutput::Single(result) => Ok((
            ProjectOutput {
                command: "project.set".to_string(),
                project_id: Some(result.id.clone()),
                project: Some(project::load(&result.id)?),
                updated: Some(result.updated_fields),
                ..Default::default()
            },
            0,
        )),
        homeboy::MergeOutput::Bulk(summary) => {
            let exit_code = if summary.errors > 0 { 1 } else { 0 };
            Ok((
                ProjectOutput {
                    command: "project.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn remove(project_id: Option<&str>, json: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = project::remove_from_json(project_id, json)?;
    Ok((
        ProjectOutput {
            command: "project.remove".to_string(),
            project_id: Some(result.id.clone()),
            project: Some(project::load(&result.id)?),
            removed: Some(result.removed_from),
            ..Default::default()
        },
        0,
    ))
}

fn rename(project_id: &str, new_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = project::rename(project_id, new_id)?;

    Ok((
        ProjectOutput {
            command: "project.rename".to_string(),
            project_id: Some(result.new_id.clone()),
            project: Some(project::load(&result.new_id)?),
            updated: Some(vec!["id".to_string()]),
            ..Default::default()
        },
        0,
    ))
}

fn delete(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    project::delete(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.delete".to_string(),
            project_id: Some(project_id.to_string()),
            deleted: Some(vec![project_id.to_string()]),
            ..Default::default()
        },
        0,
    ))
}

fn components(command: ProjectComponentsCommand) -> homeboy::Result<(ProjectOutput, i32)> {
    match command {
        ProjectComponentsCommand::List { project_id } => components_list(&project_id),
        ProjectComponentsCommand::Set {
            project_id,
            component_ids,
        } => components_set(&project_id, component_ids),
        ProjectComponentsCommand::Add {
            project_id,
            component_ids,
        } => components_add(&project_id, component_ids),
        ProjectComponentsCommand::Remove {
            project_id,
            component_ids,
        } => components_remove(&project_id, component_ids),
        ProjectComponentsCommand::Clear { project_id } => components_clear(&project_id),
    }
}

fn components_list(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = project::load(project_id)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = component::load(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: "project.components.list".to_string(),
            project_id: Some(project_id.to_string()),
            components: Some(ProjectComponentsOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            ..Default::default()
        },
        0,
    ))
}

fn components_set(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    project::set_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "set", &project)
}

fn components_add(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    project::add_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "add", &project)
}

fn components_remove(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    project::remove_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "remove", &project)
}

fn components_clear(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut project = project::load(project_id)?;
    project.component_ids.clear();

    write_project_components(project_id, "clear", &project)
}

fn write_project_components(
    project_id: &str,
    action: &str,
    project: &Project,
) -> homeboy::Result<(ProjectOutput, i32)> {
    project::save(project)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = component::load(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: format!("project.components.{action}"),
            project_id: Some(project_id.to_string()),
            components: Some(ProjectComponentsOutput {
                action: action.to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            updated: Some(vec!["componentIds".to_string()]),
            ..Default::default()
        },
        0,
    ))
}

fn pin(command: ProjectPinCommand) -> homeboy::Result<(ProjectOutput, i32)> {
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

fn pin_list(project_id: &str, pin_type: ProjectPinType) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = project::load(project_id)?;

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
            pin: Some(ProjectPinOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: Some(items),
                added: None,
                removed: None,
            }),
            ..Default::default()
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
) -> homeboy::Result<(ProjectOutput, i32)> {
    let (core_type, type_string) = match pin_type {
        ProjectPinType::File => (project::PinType::File, "file"),
        ProjectPinType::Log => (project::PinType::Log, "log"),
    };

    project::pin(
        project_id,
        core_type,
        path,
        project::PinOptions {
            label,
            tail_lines: tail,
        },
    )?;

    Ok((
        ProjectOutput {
            command: "project.pin.add".to_string(),
            project_id: Some(project_id.to_string()),
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
            ..Default::default()
        },
        0,
    ))
}

fn pin_remove(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let (core_type, type_string) = match pin_type {
        ProjectPinType::File => (project::PinType::File, "file"),
        ProjectPinType::Log => (project::PinType::Log, "log"),
    };

    project::unpin(project_id, core_type, path)?;

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            project_id: Some(project_id.to_string()),
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
            ..Default::default()
        },
        0,
    ))
}
