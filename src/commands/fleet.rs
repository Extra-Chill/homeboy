use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::fleet::{self, Fleet};
use homeboy::project::Project;

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
}

#[derive(Default, Serialize)]
pub struct FleetOutput {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleet_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleet: Option<Fleet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleets: Option<Vec<Fleet>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub updated_fields: Vec<String>,
}

pub fn run(
    args: FleetArgs,
    _global: &super::GlobalArgs,
) -> CmdResult<FleetOutput> {
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

    let json_spec = serde_json::to_string(&new_fleet).map_err(|e| {
        homeboy::Error::internal_unexpected(format!("Failed to serialize: {}", e))
    })?;

    match fleet::create(&json_spec, false)? {
        homeboy::CreateOutput::Single(result) => Ok((
            FleetOutput {
                command: "fleet.create".to_string(),
                fleet_id: Some(result.id),
                fleet: Some(result.entity),
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
            fleet_id: Some(id.to_string()),
            fleet: Some(fl),
            ..Default::default()
        },
        0,
    ))
}

fn set(args: DynamicSetArgs) -> CmdResult<FleetOutput> {
    let spec = args.json_spec()?;
    let has_input = spec.is_some() || !args.extra.is_empty();
    if !has_input {
        return Err(homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        ));
    }

    let merged = super::merge_json_sources(spec.as_deref(), &args.extra)?;
    let json_string = serde_json::to_string(&merged).map_err(|e| {
        homeboy::Error::internal_unexpected(format!("Failed to serialize merged JSON: {}", e))
    })?;

    match fleet::merge(args.id.as_deref(), &json_string, &args.replace)? {
        homeboy::MergeOutput::Single(result) => {
            let fl = fleet::load(&result.id)?;
            Ok((
                FleetOutput {
                    command: "fleet.set".to_string(),
                    fleet_id: Some(result.id),
                    fleet: Some(fl),
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
            fleet_id: Some(id.to_string()),
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
            fleets: Some(fleets),
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
            fleet_id: Some(fleet_id.to_string()),
            fleet: Some(fl),
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
            fleet_id: Some(fleet_id.to_string()),
            fleet: Some(fl),
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
            fleet_id: Some(id.to_string()),
            projects: Some(projects),
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
            fleet_id: Some(id.to_string()),
            components: Some(components),
            ..Default::default()
        },
        0,
    ))
}
