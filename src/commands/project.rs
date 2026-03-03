use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::server;
use homeboy::EntityCrudOutput;

use super::CmdResult;

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
        #[command(flatten)]
        args: super::DynamicSetArgs,
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

pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub components: Vec<Component>,
}

#[derive(Debug, Serialize)]

pub struct ProjectListItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
}

#[derive(Debug, Serialize)]

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

pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Debug, Serialize)]

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

/// Entity-specific fields for project commands.
#[derive(Debug, Default, Serialize)]
pub struct ProjectExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<ProjectListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<ProjectComponentsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_blockers: Option<Vec<String>>,
}

pub type ProjectOutput = EntityCrudOutput<Project, ProjectExtra>;

pub fn run(args: ProjectArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ProjectOutput> {
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
            let json_spec = if let Some(spec) = json {
                spec
            } else {
                let id = id.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "id",
                        "Missing required argument: id",
                        None,
                        None,
                    )
                })?;

                let new_project = project::Project {
                    id: id.clone(),
                    domain,
                    server_id,
                    base_path,
                    table_prefix,
                    ..Default::default()
                };

                homeboy::config::serialize_with_id(&new_project, &id)?
            };

            match project::create(&json_spec, skip_existing)? {
                homeboy::CreateOutput::Single(result) => Ok((
                    ProjectOutput {
                        command: "project.create".to_string(),
                        id: Some(result.id),
                        entity: Some(result.entity),
                        ..Default::default()
                    },
                    0,
                )),
                homeboy::CreateOutput::Bulk(summary) => {
                    let exit_code = summary.exit_code();
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
        ProjectCommand::Set { args } => set(args),
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

fn list() -> CmdResult<ProjectOutput> {
    let projects = project::list()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|p| ProjectListItem {
            id: p.id,
            domain: p.domain,
        })
        .collect();

    let hint = if items.is_empty() {
        Some("No projects configured. Run 'homeboy init' to see project context".to_string())
    } else {
        None
    };

    Ok((
        ProjectOutput {
            command: "project.list".to_string(),
            hint,
            extra: ProjectExtra {
                projects: Some(items),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn show(project_id: &str) -> CmdResult<ProjectOutput> {
    let project = project::load(project_id)?;

    let hint = if project.server_id.is_none() {
        Some(
            "Local project: Commands execute on this machine. Only deploy requires a server."
                .to_string(),
        )
    } else if project.component_ids.is_empty() {
        Some(format!(
            "No components linked. Use: homeboy project components add {} <component-id>",
            project.id
        ))
    } else {
        None
    };

    // Calculate deploy readiness
    let (deploy_ready, deploy_blockers) = calculate_deploy_readiness(&project);

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            id: Some(project.id.clone()),
            entity: Some(project),
            hint,
            extra: ProjectExtra {
                deploy_ready: Some(deploy_ready),
                deploy_blockers: if deploy_blockers.is_empty() {
                    None
                } else {
                    Some(deploy_blockers)
                },
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn calculate_deploy_readiness(project: &Project) -> (bool, Vec<String>) {
    let mut blockers = Vec::new();

    // Check server_id
    match &project.server_id {
        None => {
            blockers.push(format!(
                "Missing server_id - set with: homeboy project set {} '{{\"server_id\": \"<server-id>\"}}'",
                project.id
            ));
        }
        Some(sid) if !server::exists(sid) => {
            blockers.push(format!(
                "Server '{}' not found - create with: homeboy server set {} '{{\"host\": \"...\", \"user\": \"...\"}}'",
                sid, sid
            ));
        }
        _ => {}
    }

    // Check base_path
    if project
        .base_path
        .as_ref()
        .map(|p| p.is_empty())
        .unwrap_or(true)
    {
        blockers.push(format!(
            "Missing base_path - set with: homeboy project set {} '{{\"base_path\": \"/path/to/webroot\"}}'",
            project.id
        ));
    }

    // Check components
    if project.component_ids.is_empty() {
        blockers.push(format!(
            "No components linked - add with: homeboy project components add {} <component-id>",
            project.id
        ));
    } else {
        // Check if at least one component is actually deployable (has artifact or git strategy)
        let has_deployable = project.component_ids.iter().any(|id| {
            if let Ok(comp) = component::load(id) {
                let is_git = comp.deploy_strategy.as_deref() == Some("git");
                let has_artifact = component::resolve_artifact(&comp).is_some();
                is_git || has_artifact
            } else {
                false
            }
        });
        if !has_deployable {
            blockers.push(format!(
                "No deployable components - {} component(s) exist but none have a build artifact or deploy strategy configured",
                project.component_ids.len()
            ));
        }
    }

    let deploy_ready = blockers.is_empty();
    (deploy_ready, blockers)
}

fn set(args: super::DynamicSetArgs) -> CmdResult<ProjectOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match project::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        homeboy::MergeOutput::Single(result) => Ok((
            ProjectOutput {
                command: "project.set".to_string(),
                id: Some(result.id.clone()),
                entity: Some(project::load(&result.id)?),
                updated_fields: result.updated_fields,
                ..Default::default()
            },
            0,
        )),
        homeboy::MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
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

fn remove(project_id: Option<&str>, json: &str) -> CmdResult<ProjectOutput> {
    let result = project::remove_from_json(project_id, json)?;
    Ok((
        ProjectOutput {
            command: "project.remove".to_string(),
            id: Some(result.id.clone()),
            entity: Some(project::load(&result.id)?),
            extra: ProjectExtra {
                removed: Some(result.removed_from),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn rename(project_id: &str, new_id: &str) -> CmdResult<ProjectOutput> {
    let project = project::rename(project_id, new_id)?;

    Ok((
        ProjectOutput {
            command: "project.rename".to_string(),
            id: Some(project.id.clone()),
            entity: Some(project),
            updated_fields: vec!["id".to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn delete(project_id: &str) -> CmdResult<ProjectOutput> {
    project::delete(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.delete".to_string(),
            id: Some(project_id.to_string()),
            deleted: vec![project_id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn components(command: ProjectComponentsCommand) -> CmdResult<ProjectOutput> {
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

fn components_list(project_id: &str) -> CmdResult<ProjectOutput> {
    let project = project::load(project_id)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = component::load(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: "project.components.list".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
                components: Some(ProjectComponentsOutput {
                    action: "list".to_string(),
                    project_id: project_id.to_string(),
                    component_ids: project.component_ids.clone(),
                    components,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn components_set(project_id: &str, component_ids: Vec<String>) -> CmdResult<ProjectOutput> {
    project::set_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "set", &project)
}

fn components_add(project_id: &str, component_ids: Vec<String>) -> CmdResult<ProjectOutput> {
    project::add_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "add", &project)
}

fn components_remove(project_id: &str, component_ids: Vec<String>) -> CmdResult<ProjectOutput> {
    project::remove_components(project_id, component_ids)?;
    let project = project::load(project_id)?;
    write_project_components(project_id, "remove", &project)
}

fn components_clear(project_id: &str) -> CmdResult<ProjectOutput> {
    let mut project = project::load(project_id)?;
    project.component_ids.clear();

    write_project_components(project_id, "clear", &project)
}

fn write_project_components(
    project_id: &str,
    action: &str,
    project: &Project,
) -> CmdResult<ProjectOutput> {
    project::save(project)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = component::load(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: format!("project.components.{action}"),
            id: Some(project_id.to_string()),
            updated_fields: vec!["componentIds".to_string()],
            extra: ProjectExtra {
                components: Some(ProjectComponentsOutput {
                    action: action.to_string(),
                    project_id: project_id.to_string(),
                    component_ids: project.component_ids.clone(),
                    components,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn pin(command: ProjectPinCommand) -> CmdResult<ProjectOutput> {
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

fn pin_list(project_id: &str, pin_type: ProjectPinType) -> CmdResult<ProjectOutput> {
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
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
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
) -> CmdResult<ProjectOutput> {
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
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
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
            ..Default::default()
        },
        0,
    ))
}

fn pin_remove(project_id: &str, path: &str, pin_type: ProjectPinType) -> CmdResult<ProjectOutput> {
    let (core_type, type_string) = match pin_type {
        ProjectPinType::File => (project::PinType::File, "file"),
        ProjectPinType::Log => (project::PinType::Log, "log"),
    };

    project::unpin(project_id, core_type, path)?;

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
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
            ..Default::default()
        },
        0,
    ))
}
