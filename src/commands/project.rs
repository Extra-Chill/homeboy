use clap::{Args, Subcommand, ValueEnum};
use homeboy::log_status;
use serde::Serialize;
use std::path::Path;

use homeboy::health::ServerHealth;
use homeboy::project::{self, Project};
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
    /// Show live server health and component versions for a project
    Status {
        /// Project ID
        project_id: String,

        /// Show only server health metrics, skip component versions
        #[arg(long)]
        health_only: bool,
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
        /// JSON array of attachments: [{"id":"foo","local_path":"/repo"}]
        #[arg(long)]
        json: String,
    },
    /// Attach a repo path for a project component discovered via homeboy.json
    AttachPath {
        /// Project ID
        project_id: String,
        /// Local repo path containing homeboy.json
        local_path: String,
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

pub struct ProjectListItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
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
    pub components: Option<homeboy::project::ProjectComponentsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<homeboy::project::ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_blockers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ServerHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_versions: Option<Vec<ProjectComponentVersion>>,
}

#[derive(Debug, Serialize)]
pub struct ProjectComponentVersion {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Where the version was resolved from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_source: Option<String>,
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
        ProjectCommand::Status {
            project_id,
            health_only,
        } => status(&project_id, health_only),
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
        Some(
            "No projects configured. Run 'homeboy status --full' to see project context"
                .to_string(),
        )
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
    } else if project.components.is_empty() {
        Some(format!(
            "No components linked. Use: homeboy project components add {} <component-id> or homeboy project components attach-path {} <component-id> <path>",
            project.id,
            project.id
        ))
    } else {
        None
    };

    // Calculate deploy readiness
    let (deploy_ready, deploy_blockers) = project::calculate_deploy_readiness(&project);

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
        ProjectComponentsCommand::Set { project_id, json } => components_set(&project_id, &json),
        ProjectComponentsCommand::AttachPath {
            project_id,
            local_path,
        } => components_attach_path(&project_id, &local_path),
        ProjectComponentsCommand::Remove {
            project_id,
            component_ids,
        } => components_remove(&project_id, component_ids),
        ProjectComponentsCommand::Clear { project_id } => components_clear(&project_id),
    }
}

fn components_list(project_id: &str) -> CmdResult<ProjectOutput> {
    let components = project::list_components(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.components.list".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
                components: Some(components),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn components_set(project_id: &str, json: &str) -> CmdResult<ProjectOutput> {
    let components = project::set_components(project_id, json)?;
    Ok(write_project_components_response(project_id, "set", components))
}

fn components_attach_path(project_id: &str, local_path: &str) -> CmdResult<ProjectOutput> {
    let components = project::attach_component_path_report(project_id, Path::new(local_path))?;
    Ok(write_project_components_response(
        project_id,
        "attach_path",
        components,
    ))
}

fn components_remove(project_id: &str, component_ids: Vec<String>) -> CmdResult<ProjectOutput> {
    let components = project::remove_components_report(project_id, component_ids)?;
    Ok(write_project_components_response(project_id, "remove", components))
}

fn components_clear(project_id: &str) -> CmdResult<ProjectOutput> {
    let components = project::clear_components(project_id)?;
    Ok(write_project_components_response(project_id, "clear", components))
}

fn write_project_components_response(
    project_id: &str,
    action: &str,
    components: homeboy::project::ProjectComponentsOutput,
) -> (ProjectOutput, i32) {
    (
        ProjectOutput {
            command: format!("project.components.{action}"),
            id: Some(project_id.to_string()),
            updated_fields: vec!["componentIds".to_string()],
            extra: ProjectExtra {
                components: Some(components),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    )
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
    let pin = project::list_pins(project_id, map_pin_type(pin_type))?;

    Ok((
        ProjectOutput {
            command: "project.pin.list".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
                pin: Some(pin),
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
    let pin = project::add_pin(
        project_id,
        map_pin_type(pin_type),
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
                pin: Some(pin),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn pin_remove(project_id: &str, path: &str, pin_type: ProjectPinType) -> CmdResult<ProjectOutput> {
    let pin = project::remove_pin(project_id, map_pin_type(pin_type), path)?;

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
                pin: Some(pin),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn map_pin_type(pin_type: ProjectPinType) -> project::PinType {
    match pin_type {
        ProjectPinType::File => project::PinType::File,
        ProjectPinType::Log => project::PinType::Log,
    }
}

fn status(project_id: &str, health_only: bool) -> CmdResult<ProjectOutput> {
    project::load(project_id)?;

    log_status!("project", "Checking '{}'...", project_id);

    let snapshot = project::collect_status(project_id, health_only);
    let component_versions = snapshot.component_versions.map(|versions| {
        versions
            .into_iter()
            .map(|version| ProjectComponentVersion {
                component_id: version.component_id,
                version: version.version,
                version_source: version.version_source,
            })
            .collect()
    });

    Ok((
        ProjectOutput {
            command: "project.status".to_string(),
            id: Some(project_id.to_string()),
            extra: ProjectExtra {
                health: snapshot.health,
                component_versions,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}
