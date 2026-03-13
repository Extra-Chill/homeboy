use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::fleet::{self, Fleet};
use homeboy::health::ServerHealth;
use homeboy::project::Project;
use homeboy::EntityCrudOutput;

use super::{CmdResult, DynamicSetArgs};

#[derive(Args)]
pub struct FleetArgs {
    #[command(subcommand)]
    command: FleetCommand,
}

#[derive(Subcommand)]
enum FleetCommand {
    /// Create a new fleet
    Create {
        /// Fleet ID
        id: String,

        /// Project IDs to include (comma-separated or repeated)
        #[arg(long, short = 'p', value_delimiter = ',')]
        projects: Option<Vec<String>>,

        /// Description of the fleet
        #[arg(long, short = 'd')]
        description: Option<String>,
    },
    /// Display fleet configuration
    Show {
        /// Fleet ID
        id: String,
    },
    /// Update fleet configuration
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Delete a fleet
    Delete {
        /// Fleet ID
        id: String,
    },
    /// List all fleets
    List,
    /// Add a project to a fleet
    Add {
        /// Fleet ID
        id: String,

        /// Project ID to add
        #[arg(long, short = 'p')]
        project: String,
    },
    /// Remove a project from a fleet
    Remove {
        /// Fleet ID
        id: String,

        /// Project ID to remove
        #[arg(long, short = 'p')]
        project: String,
    },
    /// Show projects in a fleet
    Projects {
        /// Fleet ID
        id: String,
    },
    /// Show component usage across a fleet
    Components {
        /// Fleet ID
        id: String,
    },
    /// Show live component versions and server health across a fleet (via SSH)
    Status {
        /// Fleet ID
        id: String,

        /// Use locally cached versions instead of live SSH check
        #[arg(long)]
        cached: bool,

        /// Show only server health metrics, skip component versions
        #[arg(long)]
        health_only: bool,
    },
    /// Check component drift across a fleet (compares local vs remote)
    Check {
        /// Fleet ID
        id: String,

        /// Only show components that need updates
        #[arg(long)]
        outdated: bool,
    },
    /// Run a command across all projects in a fleet via SSH
    Exec {
        /// Fleet ID
        id: String,

        /// Command to execute on each project's server
        #[arg(num_args = 0.., trailing_var_arg = true)]
        command: Vec<String>,

        /// Show what would execute without running anything
        #[arg(long)]
        check: bool,

        /// Reserved for future parallel mode. Currently all execution is serial.
        #[arg(long, hide = true)]
        serial: bool,
    },
    /// [DEPRECATED] Use 'homeboy deploy' instead. See issue #101.
    Sync {
        /// Fleet ID
        id: String,

        /// Sync only specific categories (repeatable)
        #[arg(long, short = 'c', value_delimiter = ',')]
        category: Option<Vec<String>>,

        /// Show what would be synced without doing it
        #[arg(long)]
        dry_run: bool,

        /// Override leader server (defaults to fleet-sync.json config)
        #[arg(long)]
        leader: Option<String>,
    },
}

/// Entity-specific fields for fleet commands.
#[derive(Debug, Default, Serialize)]
pub struct FleetExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<FleetProjectStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<Vec<homeboy::fleet::FleetProjectCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<homeboy::fleet::FleetCheckSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<homeboy::fleet::FleetExecProjectResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_summary: Option<homeboy::fleet::FleetExecSummary>,
}

pub type FleetOutput = EntityCrudOutput<Fleet, FleetExtra>;

#[derive(Debug, Default, Serialize)]
pub struct FleetProjectStatus {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub components: Vec<FleetComponentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ServerHealth>,
}

#[derive(Debug, Default, Serialize)]
pub struct FleetComponentStatus {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Where the version was resolved from: "live" (SSH) or "cached" (local file)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_source: Option<String>,
}

pub fn run(args: FleetArgs, _global: &super::GlobalArgs) -> CmdResult<FleetOutput> {
    match args.command {
        FleetCommand::Create {
            id,
            projects,
            description,
        } => create(&id, projects.unwrap_or_default(), description),
        FleetCommand::Show { id } => show(&id),
        FleetCommand::Set { args } => set(args),
        FleetCommand::Delete { id } => delete(&id),
        FleetCommand::List => list(),
        FleetCommand::Add { id, project } => add(&id, &project),
        FleetCommand::Remove { id, project } => remove(&id, &project),
        FleetCommand::Projects { id } => projects(&id),
        FleetCommand::Components { id } => components(&id),
        FleetCommand::Status {
            id,
            cached,
            health_only,
        } => status(&id, cached, health_only),
        FleetCommand::Check { id, outdated } => check(&id, outdated),
        FleetCommand::Exec {
            id,
            command,
            check,
            serial: _,
        } => exec(&id, command, check),
        FleetCommand::Sync {
            id,
            category,
            dry_run,
            leader,
        } => sync(&id, category, dry_run, leader),
    }
}

fn create(
    id: &str,
    project_ids: Vec<String>,
    description: Option<String>,
) -> CmdResult<FleetOutput> {
    // Validate projects exist
    for pid in &project_ids {
        if !homeboy::project::exists(pid) {
            return Err(homeboy::Error::project_not_found(pid, vec![]));
        }
    }

    let mut new_fleet = Fleet::new(id.to_string(), project_ids);
    new_fleet.description = description;

    let json_spec = homeboy::config::to_json_string(&new_fleet)?;

    match fleet::create(&json_spec, false)? {
        homeboy::CreateOutput::Single(result) => Ok((
            FleetOutput {
                command: "fleet.create".to_string(),
                id: Some(result.id),
                entity: Some(result.entity),
                ..Default::default()
            },
            0,
        )),
        homeboy::CreateOutput::Bulk(_) => Err(homeboy::Error::internal_unexpected(
            "Unexpected bulk result for single fleet".to_string(),
        )),
    }
}

fn show(id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::load(id)?;

    Ok((
        FleetOutput {
            command: "fleet.show".to_string(),
            id: Some(id.to_string()),
            entity: Some(fl),
            ..Default::default()
        },
        0,
    ))
}

fn set(args: DynamicSetArgs) -> CmdResult<FleetOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match fleet::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        homeboy::MergeOutput::Single(result) => {
            let fl = fleet::load(&result.id)?;
            Ok((
                FleetOutput {
                    command: "fleet.set".to_string(),
                    id: Some(result.id),
                    entity: Some(fl),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        homeboy::MergeOutput::Bulk(_) => Err(homeboy::Error::internal_unexpected(
            "Unexpected bulk result for single fleet".to_string(),
        )),
    }
}

fn delete(id: &str) -> CmdResult<FleetOutput> {
    fleet::delete(id)?;

    Ok((
        FleetOutput {
            command: "fleet.delete".to_string(),
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn list() -> CmdResult<FleetOutput> {
    let fleets = fleet::list()?;

    Ok((
        FleetOutput {
            command: "fleet.list".to_string(),
            entities: fleets,
            ..Default::default()
        },
        0,
    ))
}

fn add(fleet_id: &str, project_id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::add_project(fleet_id, project_id)?;

    Ok((
        FleetOutput {
            command: "fleet.add".to_string(),
            id: Some(fleet_id.to_string()),
            entity: Some(fl),
            updated_fields: vec!["project_ids".to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn remove(fleet_id: &str, project_id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::remove_project(fleet_id, project_id)?;

    Ok((
        FleetOutput {
            command: "fleet.remove".to_string(),
            id: Some(fleet_id.to_string()),
            entity: Some(fl),
            updated_fields: vec!["project_ids".to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn projects(id: &str) -> CmdResult<FleetOutput> {
    let projects = fleet::get_projects(id)?;

    Ok((
        FleetOutput {
            command: "fleet.projects".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                projects: Some(projects),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn components(id: &str) -> CmdResult<FleetOutput> {
    let components = fleet::component_usage(id)?;

    Ok((
        FleetOutput {
            command: "fleet.components".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                components: Some(components),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn status(id: &str, cached: bool, health_only: bool) -> CmdResult<FleetOutput> {
    let project_statuses = fleet::collect_status(id, cached, health_only)?
        .into_iter()
        .map(|status| FleetProjectStatus {
            project_id: status.project_id,
            server_id: status.server_id,
            components: status
                .components
                .into_iter()
                .map(|component| FleetComponentStatus {
                    component_id: component.component_id,
                    version: component.version,
                    version_source: component.version_source,
                })
                .collect(),
            health: status.health,
        })
        .collect();

    Ok((
        FleetOutput {
            command: "fleet.status".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                status: Some(project_statuses),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn check(id: &str, only_outdated: bool) -> CmdResult<FleetOutput> {
    let (project_checks, summary, exit_code) = fleet::collect_check(id, only_outdated)?;

    Ok((
        FleetOutput {
            command: "fleet.check".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                check: Some(project_checks),
                summary: Some(summary),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

fn exec(id: &str, command: Vec<String>, check: bool) -> CmdResult<FleetOutput> {
    let (results, summary, exit_code) = fleet::collect_exec(id, command, check)?;

    Ok((
        FleetOutput {
            command: "fleet.exec".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                exec: Some(results),
                exec_summary: Some(summary),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

fn sync(
    _id: &str,
    _categories: Option<Vec<String>>,
    _dry_run: bool,
    _leader_override: Option<String>,
) -> CmdResult<FleetOutput> {
    Err(homeboy::Error::validation_invalid_argument(
        "fleet sync",
        "fleet sync has been deprecated. Use 'homeboy deploy' to sync files across servers. \
         Register your agent workspace as a component and deploy it like any other component.",
        None,
        None,
    )
    .with_hint("homeboy deploy <component> --fleet <fleet>".to_string())
    .with_hint("See: https://github.com/Extra-Chill/homeboy/issues/101".to_string()))
}
