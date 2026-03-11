use clap::{Args, Subcommand};
use homeboy::log_status;
use serde::Serialize;

use homeboy::deploy::{self, DeployConfig};
use homeboy::fleet::{self, Fleet};
use homeboy::health::{self, ServerHealth};
use homeboy::project::{self, Project};
use homeboy::version;
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
    pub check: Option<Vec<FleetProjectCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<FleetCheckSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<FleetExecProjectResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_summary: Option<FleetExecSummary>,
}

pub type FleetOutput = EntityCrudOutput<Fleet, FleetExtra>;

#[derive(Debug, Default, Serialize)]
pub struct FleetExecProjectResult {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    pub command: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct FleetExecSummary {
    pub total: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct FleetProjectCheck {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub components: Vec<FleetComponentCheck>,
}

#[derive(Debug, Default, Serialize)]
pub struct FleetComponentCheck {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_version: Option<String>,
    pub status: String,
}

#[derive(Debug, Default, Serialize)]
pub struct FleetCheckSummary {
    pub total_projects: u32,
    pub projects_checked: u32,
    pub projects_failed: u32,
    pub components_up_to_date: u32,
    pub components_needs_update: u32,
    pub components_unknown: u32,
}

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
    let fl = fleet::load(id)?;
    let mut project_statuses = Vec::new();

    if cached {
        // Cached mode: read versions from local files (no SSH, no health)
        for project_id in &fl.project_ids {
            let proj = match project::load(project_id) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let mut component_statuses = Vec::new();
            for component_id in project::project_component_ids(&proj) {
                let comp_version = match project::resolve_project_component(&proj, &component_id) {
                    Ok(comp) => version::get_component_version(&comp),
                    Err(_) => None,
                };

                component_statuses.push(FleetComponentStatus {
                    component_id: component_id.clone(),
                    version: comp_version,
                    version_source: Some("cached".to_string()),
                });
            }

            project_statuses.push(FleetProjectStatus {
                project_id: project_id.clone(),
                server_id: proj.server_id.clone(),
                components: component_statuses,
                health: None,
            });
        }
    } else {
        // Live mode (default): SSH into each server for versions and health
        for project_id in &fl.project_ids {
            let proj = match project::load(project_id) {
                Ok(p) => p,
                Err(_) => continue,
            };

            log_status!("fleet", "Checking '{}'...", project_id);

            // Collect health metrics via direct SSH
            let health = health::collect_project_health(&proj);

            if health_only {
                // Skip component version check
                project_statuses.push(FleetProjectStatus {
                    project_id: project_id.clone(),
                    server_id: proj.server_id.clone(),
                    components: vec![],
                    health,
                });
                continue;
            }

            // Use the deploy check infrastructure to get remote versions via SSH
            let config = DeployConfig {
                component_ids: vec![],
                all: true,
                outdated: false,
                dry_run: false,
                check: true,
                force: false,
                skip_build: true,
                keep_deps: false,
                expected_version: None,
                no_pull: true,
                head: true,
            };

            match deploy::run(project_id, &config) {
                Ok(result) => {
                    let mut component_statuses = Vec::new();
                    for comp_result in &result.results {
                        component_statuses.push(FleetComponentStatus {
                            component_id: comp_result.id.clone(),
                            version: comp_result.remote_version.clone(),
                            version_source: Some("live".to_string()),
                        });
                    }

                    project_statuses.push(FleetProjectStatus {
                        project_id: project_id.clone(),
                        server_id: proj.server_id.clone(),
                        components: component_statuses,
                        health,
                    });
                }
                Err(e) => {
                    // SSH failed for versions — fall back to cached, but keep whatever health we got
                    log_status!(
                        "fleet",
                        "Warning: could not reach '{}' — falling back to cached versions: {}",
                        project_id,
                        e
                    );

                    let mut component_statuses = Vec::new();
                    for component_id in project::project_component_ids(&proj) {
                        let comp_version =
                            match project::resolve_project_component(&proj, &component_id) {
                                Ok(comp) => version::get_component_version(&comp),
                                Err(_) => None,
                            };

                        component_statuses.push(FleetComponentStatus {
                            component_id: component_id.clone(),
                            version: comp_version,
                            version_source: Some("cached".to_string()),
                        });
                    }

                    project_statuses.push(FleetProjectStatus {
                        project_id: project_id.clone(),
                        server_id: proj.server_id.clone(),
                        components: component_statuses,
                        health,
                    });
                }
            }
        }
    }

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
    let fl = fleet::load(id)?;
    let mut project_checks = Vec::new();
    let mut summary = FleetCheckSummary {
        total_projects: fl.project_ids.len() as u32,
        ..Default::default()
    };

    for project_id in &fl.project_ids {
        log_status!("fleet", "Checking project '{}'...", project_id);

        // Use existing deploy check infrastructure
        let config = DeployConfig {
            component_ids: vec![],
            all: true,
            outdated: false,
            dry_run: false,
            check: true,
            force: false,
            skip_build: true,
            keep_deps: false,
            expected_version: None,
            no_pull: true, // Fleet checks are read-only
            head: true,    // Fleet checks don't build — skip tag checkout
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
    use homeboy::engine::shell;
    use homeboy::ssh::{resolve_context, SshClient, SshResolveArgs};

    if command.is_empty() {
        return Err(
            homeboy::Error::validation_missing_argument(vec!["command".to_string()])
                .with_hint("Usage: homeboy fleet exec <fleet> -- <command>".to_string()),
        );
    }

    let command_string = if command.len() == 1 {
        command[0].clone()
    } else {
        shell::quote_args(&command)
    };

    let projects = fleet::get_projects(id)?;

    if projects.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "fleet",
            "Fleet has no projects",
            Some(id.to_string()),
            None,
        ));
    }

    let mut results: Vec<FleetExecProjectResult> = Vec::new();
    let mut summary = FleetExecSummary {
        total: projects.len() as u32,
        ..Default::default()
    };

    for proj in &projects {
        let server_id = proj.server_id.clone();

        // Check mode: just show the plan
        if check {
            let effective_cmd = match &proj.base_path {
                Some(bp) => format!("cd {} && {}", shell::quote_path(bp), &command_string),
                None => command_string.clone(),
            };

            results.push(FleetExecProjectResult {
                project_id: proj.id.clone(),
                server_id: server_id.clone(),
                base_path: proj.base_path.clone(),
                command: effective_cmd,
                status: "planned".to_string(),
                ..Default::default()
            });
            continue;
        }

        homeboy::log_status!("fleet", "Executing on '{}'...", proj.id);

        // Resolve SSH context via project
        let resolve_result = match resolve_context(&SshResolveArgs {
            id: None,
            project: Some(proj.id.clone()),
            server: None,
        }) {
            Ok(r) => r,
            Err(e) => {
                summary.failed += 1;
                results.push(FleetExecProjectResult {
                    project_id: proj.id.clone(),
                    server_id: server_id.clone(),
                    base_path: proj.base_path.clone(),
                    command: command_string.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    ..Default::default()
                });
                continue;
            }
        };

        let client = match SshClient::from_server(&resolve_result.server, &resolve_result.server_id)
        {
            Ok(c) => c,
            Err(e) => {
                summary.failed += 1;
                results.push(FleetExecProjectResult {
                    project_id: proj.id.clone(),
                    server_id: server_id.clone(),
                    base_path: proj.base_path.clone(),
                    command: command_string.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    ..Default::default()
                });
                continue;
            }
        };

        // Build effective command with cd to base_path if available
        let effective_cmd = match &resolve_result.base_path {
            Some(bp) => format!("cd {} && {}", shell::quote_path(bp), &command_string),
            None => command_string.clone(),
        };

        let output = client.execute(&effective_cmd);

        if output.success {
            summary.succeeded += 1;
        } else {
            summary.failed += 1;
        }

        results.push(FleetExecProjectResult {
            project_id: proj.id.clone(),
            server_id: server_id.clone(),
            base_path: proj.base_path.clone(),
            command: effective_cmd,
            status: if output.success {
                "success".to_string()
            } else {
                "failed".to_string()
            },
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code: Some(output.exit_code),
            error: None,
        });
    }

    if check {
        summary.skipped = summary.total;
    }

    let exit_code = if summary.failed > 0 { 1 } else { 0 };

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
