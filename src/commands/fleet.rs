use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::fleet::{self, Fleet, FleetComponentDrift, FleetStatusResult};
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

        /// Override the SSH user for this execution (instead of each server's configured user)
        #[arg(long)]
        user: Option<String>,

        /// Reserved for future parallel mode. Currently all execution is serial.
        #[arg(long, hide = true)]
        serial: bool,
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
    pub status: Option<FleetStatusResult>,
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
            user,
            serial: _,
        } => exec(&id, command, check, user),
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
    let result = fleet::collect_status(id, cached, health_only)?;

    // Log human-readable dashboard to stderr
    log_fleet_dashboard(&result);

    let exit_code =
        if result.summary.servers.unreachable > 0 || result.summary.servers.services_down > 0 {
            1
        } else {
            0
        };

    Ok((
        FleetOutput {
            command: "fleet.status".to_string(),
            id: Some(id.to_string()),
            extra: FleetExtra {
                status: Some(result),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

/// Log a human-readable fleet status dashboard to stderr.
fn log_fleet_dashboard(result: &FleetStatusResult) {
    if !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }

    let summary = &result.summary;

    // Fleet summary header
    eprintln!("┌─── Fleet Status ───────────────────────────────────┐");
    eprintln!(
        "│ Servers: {} healthy, {} warning, {} unreachable      ",
        summary.servers.healthy, summary.servers.warning, summary.servers.unreachable,
    );
    if summary.servers.services_up > 0 || summary.servers.services_down > 0 {
        eprintln!(
            "│ Services: {} up, {} down                             ",
            summary.servers.services_up, summary.servers.services_down,
        );
    }
    eprintln!(
        "│ Components: {} current, {} outdated, {} need bump, {} unknown",
        summary.components.current,
        summary.components.needs_update,
        summary.components.needs_bump,
        summary.components.unknown,
    );
    eprintln!("└────────────────────────────────────────────────────┘");

    // Per-project component table
    for proj_status in &result.projects {
        if proj_status.components.is_empty() {
            continue;
        }

        let server_label = proj_status.server_id.as_deref().unwrap_or("unknown");

        // Health indicator
        let health_icon = match &proj_status.health {
            Some(h) if h.warnings.is_empty() => "✅",
            Some(_) => "⚠️ ",
            None => "❌",
        };

        eprintln!(
            "\n{} {} ({})",
            health_icon, proj_status.project_id, server_label,
        );

        // Component rows
        let id_width = proj_status
            .components
            .iter()
            .map(|c| c.component_id.len())
            .max()
            .unwrap_or(9)
            .max(9);

        for comp in &proj_status.components {
            let local = comp.local_version.as_deref().unwrap_or("-");
            let remote = comp.remote_version.as_deref().unwrap_or("-");
            let drift_icon = match &comp.drift {
                FleetComponentDrift::Current => "✅ current",
                FleetComponentDrift::NeedsUpdate => "⚠️  outdated",
                FleetComponentDrift::BehindRemote => "🔙 behind",
                FleetComponentDrift::NeedsBump => "🔶 needs bump",
                FleetComponentDrift::DocsOnly => "📝 docs only",
                FleetComponentDrift::Unknown => "❓ unknown",
            };

            eprintln!(
                "  {:<w$}  {} → {}  ({} unreleased)  {}",
                comp.component_id,
                local,
                remote,
                comp.unreleased_commits,
                drift_icon,
                w = id_width,
            );
        }
    }

    // Warnings
    if !summary.warnings.is_empty() {
        eprintln!("\n⚠️  Warnings:");
        for warning in &summary.warnings {
            eprintln!(
                "  {} ({}): {}",
                warning.server_id, warning.project_id, warning.message,
            );
        }
    }
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

fn exec(
    id: &str,
    command: Vec<String>,
    check: bool,
    user: Option<String>,
) -> CmdResult<FleetOutput> {
    let (results, summary, exit_code) = fleet::collect_exec(id, command, check, user)?;

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
