use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::component;
use homeboy::deploy::{self, DeployConfig};
use homeboy::fleet::{self, Fleet};
use homeboy::project::{self, Project};
use homeboy::version;

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
    /// Show component versions across a fleet (local only)
    Status {
        /// Fleet ID
        id: String,
    },
    /// Check component drift across a fleet (compares local vs remote)
    Check {
        /// Fleet ID
        id: String,

        /// Only show components that need updates
        #[arg(long)]
        outdated: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<FleetProjectStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<Vec<FleetProjectCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<FleetCheckSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub updated_fields: Vec<String>,
    // sync field removed — fleet sync deprecated (see #101)
}

#[derive(Default, Serialize)]
pub struct FleetProjectCheck {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub components: Vec<FleetComponentCheck>,
}

#[derive(Default, Serialize)]
pub struct FleetComponentCheck {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_version: Option<String>,
    pub status: String,
}

#[derive(Default, Serialize)]
pub struct FleetCheckSummary {
    pub total_projects: u32,
    pub projects_checked: u32,
    pub projects_failed: u32,
    pub components_up_to_date: u32,
    pub components_needs_update: u32,
    pub components_unknown: u32,
}

#[derive(Default, Serialize)]
pub struct FleetProjectStatus {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub components: Vec<FleetComponentStatus>,
}

#[derive(Default, Serialize)]
pub struct FleetComponentStatus {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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
        FleetCommand::Status { id } => status(&id),
        FleetCommand::Check { id, outdated } => check(&id, outdated),
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

    let json_spec = serde_json::to_string(&new_fleet)
        .map_err(|e| homeboy::Error::internal_unexpected(format!("Failed to serialize: {}", e)))?;

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

fn status(id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::load(id)?;
    let mut project_statuses = Vec::new();

    for project_id in &fl.project_ids {
        let proj = match project::load(project_id) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let mut component_statuses = Vec::new();
        for component_id in &proj.component_ids {
            let comp_version = match component::load(component_id) {
                Ok(comp) => version::get_component_version(&comp),
                Err(_) => None,
            };

            component_statuses.push(FleetComponentStatus {
                component_id: component_id.clone(),
                version: comp_version,
            });
        }

        project_statuses.push(FleetProjectStatus {
            project_id: project_id.clone(),
            server_id: proj.server_id.clone(),
            components: component_statuses,
        });
    }

    Ok((
        FleetOutput {
            command: "fleet.status".to_string(),
            fleet_id: Some(id.to_string()),
            status: Some(project_statuses),
            ..Default::default()
        },
        0,
    ))
}

fn check(id: &str, only_outdated: bool) -> CmdResult<FleetOutput> {
    let fl = fleet::load(id)?;
    let mut project_checks = Vec::new();
    let mut summary = FleetCheckSummary {
        total_projects: fl.project_ids.len() as u32,
        ..Default::default()
    };

    for project_id in &fl.project_ids {
        eprintln!("[fleet] Checking project '{}'...", project_id);

        // Use existing deploy check infrastructure
        let config = DeployConfig {
            component_ids: vec![],
            all: true,
            outdated: false,
            dry_run: false,
            check: true,
            force: false,
            skip_build: true,
        };

        match deploy::run(project_id, &config) {
            Ok(result) => {
                summary.projects_checked += 1;

                let proj = project::load(project_id).ok();
                let mut component_checks = Vec::new();

                for comp_result in &result.results {
                    let status_str = match &comp_result.component_status {
                        Some(deploy::ComponentStatus::UpToDate) => "up_to_date",
                        Some(deploy::ComponentStatus::NeedsUpdate) => "needs_update",
                        Some(deploy::ComponentStatus::BehindRemote) => "behind_remote",
                        Some(deploy::ComponentStatus::Unknown) | None => "unknown",
                    };

                    // Count for summary
                    match status_str {
                        "up_to_date" => summary.components_up_to_date += 1,
                        "needs_update" | "behind_remote" => summary.components_needs_update += 1,
                        _ => summary.components_unknown += 1,
                    }

                    // Skip up-to-date if only_outdated
                    if only_outdated && status_str == "up_to_date" {
                        continue;
                    }

                    component_checks.push(FleetComponentCheck {
                        component_id: comp_result.id.clone(),
                        local_version: comp_result.local_version.clone(),
                        remote_version: comp_result.remote_version.clone(),
                        status: status_str.to_string(),
                    });
                }

                // Skip project entirely if only_outdated and nothing to show
                if only_outdated && component_checks.is_empty() {
                    continue;
                }

                project_checks.push(FleetProjectCheck {
                    project_id: project_id.clone(),
                    server_id: proj.and_then(|p| p.server_id),
                    status: "checked".to_string(),
                    error: None,
                    components: component_checks,
                });
            }
            Err(e) => {
                summary.projects_failed += 1;

                if !only_outdated {
                    project_checks.push(FleetProjectCheck {
                        project_id: project_id.clone(),
                        server_id: None,
                        status: "failed".to_string(),
                        error: Some(e.to_string()),
                        components: vec![],
                    });
                }
            }
        }
    }

    let exit_code = if summary.projects_failed > 0 { 1 } else { 0 };

    Ok((
        FleetOutput {
            command: "fleet.check".to_string(),
            fleet_id: Some(id.to_string()),
            check: Some(project_checks),
            summary: Some(summary),
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
