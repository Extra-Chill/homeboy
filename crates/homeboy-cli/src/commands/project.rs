use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project, ProjectRecord};

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

        /// Project name (CLI mode)
        name: Option<String>,
        /// Public site domain (CLI mode)
        domain: Option<String>,
        /// Module to enable (can be specified multiple times)
        #[arg(long = "module", value_name = "MODULE")]
        modules: Vec<String>,
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
    Set {
        /// Project ID
        project_id: String,
        /// Project name
        #[arg(long)]
        name: Option<String>,
        /// Public site domain
        #[arg(long)]
        domain: Option<String>,
        /// Replace modules (can be specified multiple times)
        #[arg(long = "module", value_name = "MODULE")]
        modules: Vec<String>,
        /// Server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Table prefix
        #[arg(long)]
        table_prefix: Option<String>,
        /// Replace project component IDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        component_ids: Vec<String>,
    },
    /// Repair a project file whose name doesn't match the stored project name
    Repair {
        /// Project ID (file stem)
        project_id: String,
    },
    /// Rename a project (changes ID based on new name)
    Rename {
        /// Current project ID
        project_id: String,
        /// New display name (ID will be derived from this)
        new_name: String,
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
    name: String,
    domain: String,
    modules: Vec<String>,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    command: String,
    project_id: Option<String>,
    project: Option<ProjectRecord>,
    projects: Option<Vec<ProjectListItem>>,
    components: Option<ProjectComponentsOutput>,
    pin: Option<ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import: Option<project::CreateSummary>,
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
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
        } => {
            if let Some(spec) = json {
                return create_json(&spec, skip_existing);
            }

            let name = name.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "name",
                    "Missing required argument: name (or use --json)",
                    None,
                    None,
                )
            })?;
            let domain = domain.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "domain",
                    "Missing required argument: domain (or use --json)",
                    None,
                    None,
                )
            })?;

            create(&name, &domain, modules, server_id, base_path, table_prefix)
        }
        ProjectCommand::Set {
            project_id,
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
            component_ids,
        } => set(
            &project_id,
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
            component_ids,
        ),
        ProjectCommand::Repair { project_id } => repair(&project_id),
        ProjectCommand::Rename { project_id, new_name } => rename(&project_id, &new_name),
        ProjectCommand::Components { command } => components(command),
        ProjectCommand::Pin { command } => pin(command),
    }
}

fn list() -> homeboy::Result<(ProjectOutput, i32)> {
    let projects = project::list()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|record| ProjectListItem {
            id: record.id,
            name: record.config.name,
            domain: record.config.domain,
            modules: record.config.modules,
        })
        .collect();

    Ok((
        ProjectOutput {
            command: "project.list".to_string(),
            project_id: None,
            project: None,
            projects: Some(items),
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn show(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = project::load_record(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            project_id: Some(project.id.clone()),
            project: Some(project),
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn create_json(spec: &str, skip_existing: bool) -> homeboy::Result<(ProjectOutput, i32)> {
    let summary = project::create_from_json(spec, skip_existing)?;
    let exit_code = if summary.errors > 0 { 1 } else { 0 };

    Ok((
        ProjectOutput {
            command: "project.create".to_string(),
            project_id: None,
            project: None,
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: Some(summary),
        },
        exit_code,
    ))
}

fn create(
    name: &str,
    domain: &str,
    modules: Vec<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = project::create_from_cli(
        Some(name.to_string()),
        Some(domain.to_string()),
        modules,
        server_id,
        base_path,
        table_prefix,
    )?;

    let created_id = result.id;
    let project = project::load_record(&created_id)?;

    Ok((
        ProjectOutput {
            command: "project.create".to_string(),
            project_id: Some(created_id),
            project: Some(project),
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn set(
    project_id: &str,
    name: Option<String>,
    domain: Option<String>,
    modules: Vec<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut updated_fields: Vec<String> = Vec::new();

    if let Some(name) = name {
        let result = project::rename(project_id, &name)?;
        updated_fields.push("name".to_string());

        if result.new_id != project_id {
            updated_fields.push("id".to_string());
        }

        return Ok((
            ProjectOutput {
                command: "project.set".to_string(),
                project_id: Some(result.new_id.clone()),
                project: Some(project::load_record(&result.new_id)?),
                projects: None,
                components: None,
                pin: None,
                updated: Some(updated_fields),
                import: None,
            },
            0,
        ));
    }

    let mut project = project::load(project_id)?;

    if let Some(domain) = domain {
        project.domain = domain;
        updated_fields.push("domain".to_string());
    }

    if !modules.is_empty() {
        project.modules = modules;
        updated_fields.push("modules".to_string());
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

    if !component_ids.is_empty() {
        project.component_ids = project::validate_component_ids(component_ids, project_id)?;
        updated_fields.push("componentIds".to_string());
    }

    if updated_fields.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "fields",
            "No fields provided to update",
            Some(project_id.to_string()),
            None,
        ));
    }

    project::save(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.set".to_string(),
            project_id: Some(project_id.to_string()),
            project: Some(project::load_record(project_id)?),
            projects: None,
            components: None,
            pin: None,
            updated: Some(updated_fields),
            import: None,
        },
        0,
    ))
}

fn repair(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = project::repair(project_id)?;

    let updated = if result.new_id != result.old_id {
        Some(vec!["id".to_string()])
    } else {
        None
    };

    Ok((
        ProjectOutput {
            command: "project.repair".to_string(),
            project_id: Some(result.new_id.clone()),
            project: Some(project::load_record(&result.new_id)?),
            projects: None,
            components: None,
            pin: None,
            updated,
            import: None,
        },
        0,
    ))
}

fn rename(project_id: &str, new_name: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = project::rename(project_id, new_name)?;

    Ok((
        ProjectOutput {
            command: "project.rename".to_string(),
            project_id: Some(result.new_id.clone()),
            project: Some(project::load_record(&result.new_id)?),
            projects: None,
            components: None,
            pin: None,
            updated: Some(vec!["id".to_string(), "name".to_string()]),
            import: None,
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
            project: None,
            projects: None,
            components: Some(ProjectComponentsOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn components_set(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let deduped = project::validate_component_ids(component_ids, project_id)?;

    let mut project = project::load(project_id)?;
    project.component_ids = deduped.clone();

    write_project_components(project_id, "set", &project)
}

fn components_add(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let deduped = project::validate_component_ids(component_ids, project_id)?;

    let mut project = project::load(project_id)?;
    for id in deduped {
        if !project.component_ids.contains(&id) {
            project.component_ids.push(id);
        }
    }

    write_project_components(project_id, "add", &project)
}

fn components_remove(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    project::remove_components_validated(project_id, component_ids)?;
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
    project::save(project_id, project)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = component::load(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: format!("project.components.{action}"),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: Some(ProjectComponentsOutput {
                action: action.to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            pin: None,
            updated: Some(vec!["componentIds".to_string()]),
            import: None,
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

fn pin_list(
    project_id: &str,
    pin_type: ProjectPinType,
) -> homeboy::Result<(ProjectOutput, i32)> {
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
            project: None,
            projects: None,
            components: None,
            pin: Some(ProjectPinOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: Some(items),
                added: None,
                removed: None,
            }),
            updated: None,
            import: None,
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
    let type_string = match pin_type {
        ProjectPinType::File => {
            project::pin_file(project_id, path, label)?;
            "file"
        }
        ProjectPinType::Log => {
            project::pin_log(project_id, path, label, tail)?;
            "log"
        }
    };

    Ok((
        ProjectOutput {
            command: "project.pin.add".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: None,
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
            import: None,
        },
        0,
    ))
}

fn pin_remove(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let type_string = match pin_type {
        ProjectPinType::File => {
            project::unpin_file_by_path(project_id, path)?;
            "file"
        }
        ProjectPinType::Log => {
            project::unpin_log_by_path(project_id, path)?;
            "log"
        }
    };

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: None,
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
            import: None,
        },
        0,
    ))
}
