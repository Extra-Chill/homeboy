use std::collections::HashMap;

use crate::deploy::{self, DeployConfig, ReleaseStateStatus};
use crate::project;
use crate::server::health::{self, ServerHealth};
use crate::version;
use serde::Serialize;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct FleetComponentStatus {
    pub component_id: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    /// Where the version was resolved from: "live" (SSH) or "cached" (local file)
    pub version_source: String,
    /// Component drift status
    pub drift: FleetComponentDrift,
    /// Number of unreleased commits since last version tag
    pub unreleased_commits: u32,
}

/// Component drift status within the fleet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetComponentDrift {
    /// Local and remote versions match
    Current,
    /// Local version is ahead of remote (needs deploy)
    NeedsUpdate,
    /// Remote version is ahead of local
    BehindRemote,
    /// Has unreleased code commits (needs version bump)
    NeedsBump,
    /// Only docs changes since last tag
    DocsOnly,
    /// Cannot determine
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetProjectStatus {
    pub project_id: String,
    pub server_id: Option<String>,
    pub components: Vec<FleetComponentStatus>,
    pub health: Option<ServerHealth>,
}

/// Fleet-wide summary statistics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FleetStatusSummary {
    pub projects: FleetProjectSummary,
    pub components: FleetComponentSummary,
    pub servers: FleetServerSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<FleetWarning>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FleetProjectSummary {
    pub total: u32,
    pub healthy: u32,
    pub warning: u32,
    pub unreachable: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FleetComponentSummary {
    pub total: u32,
    pub current: u32,
    pub needs_update: u32,
    pub needs_bump: u32,
    pub docs_only: u32,
    pub unknown: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FleetServerSummary {
    pub total: u32,
    pub healthy: u32,
    pub warning: u32,
    pub unreachable: u32,
    pub services_up: u32,
    pub services_down: u32,
}

/// A warning with its source context.
#[derive(Debug, Clone, Serialize)]
pub struct FleetWarning {
    pub server_id: String,
    pub project_id: String,
    pub message: String,
}

/// Full fleet status result including per-project data and fleet-wide summary.
#[derive(Debug, Clone, Serialize)]
pub struct FleetStatusResult {
    pub projects: Vec<FleetProjectStatus>,
    pub summary: FleetStatusSummary,
}

// ============================================================================
// Collection
// ============================================================================

pub fn collect_status(
    fleet_id: &str,
    cached: bool,
    health_only: bool,
) -> crate::Result<FleetStatusResult> {
    let fl = super::load(fleet_id)?;

    if cached {
        return collect_cached_status(&fl.project_ids);
    }

    // Deduplicate servers: collect health once per unique server
    let mut server_health_cache: HashMap<String, Option<ServerHealth>> = HashMap::new();
    let mut project_statuses = Vec::new();
    let mut summary = FleetStatusSummary::default();
    summary.projects.total = fl.project_ids.len() as u32;

    for project_id in &fl.project_ids {
        let proj = match project::load(project_id) {
            Ok(p) => p,
            Err(_) => {
                summary.projects.unreachable += 1;
                continue;
            }
        };

        // Collect health (deduped by server_id)
        let health = if let Some(ref server_id) = proj.server_id {
            if let Some(cached_health) = server_health_cache.get(server_id) {
                cached_health.clone()
            } else {
                let h = health::collect_project_health(&proj);
                server_health_cache.insert(server_id.clone(), h.clone());
                h
            }
        } else {
            None
        };

        // Track server health in summary (only count each server once)
        if let Some(ref server_id) = proj.server_id {
            // Only count server stats on first encounter
            if server_health_cache.len() as u32 > summary.servers.total
                || !server_health_cache
                    .keys()
                    .take(summary.servers.total as usize)
                    .any(|k| k == server_id)
            {
                // This is a newly seen server (we just inserted it above)
            }
        }

        // Classify project health
        match &health {
            Some(h) if h.warnings.is_empty() => summary.projects.healthy += 1,
            Some(h) => {
                summary.projects.warning += 1;
                // Aggregate warnings
                if let Some(ref server_id) = proj.server_id {
                    for warning_msg in &h.warnings {
                        summary.warnings.push(FleetWarning {
                            server_id: server_id.clone(),
                            project_id: project_id.clone(),
                            message: warning_msg.clone(),
                        });
                    }
                }
            }
            None => summary.projects.unreachable += 1,
        }

        if health_only {
            project_statuses.push(FleetProjectStatus {
                project_id: project_id.clone(),
                server_id: proj.server_id.clone(),
                components: vec![],
                health,
            });
            continue;
        }

        // Collect component status: version drift + release state
        let component_statuses =
            collect_project_component_statuses(project_id, &proj, &mut summary.components);

        project_statuses.push(FleetProjectStatus {
            project_id: project_id.clone(),
            server_id: proj.server_id.clone(),
            components: component_statuses,
            health,
        });
    }

    // Compute server summary from deduped cache
    compute_server_summary(&server_health_cache, &project_statuses, &mut summary);

    Ok(FleetStatusResult {
        projects: project_statuses,
        summary,
    })
}

/// Collect cached status (local versions only, no SSH).
fn collect_cached_status(project_ids: &[String]) -> crate::Result<FleetStatusResult> {
    let mut project_statuses = Vec::new();
    let mut summary = FleetStatusSummary::default();
    summary.projects.total = project_ids.len() as u32;

    for project_id in project_ids {
        let proj = match project::load(project_id) {
            Ok(p) => p,
            Err(_) => continue,
        };

        summary.projects.healthy += 1; // Can't know health without SSH

        let mut component_statuses = Vec::new();
        for component_id in project::project_component_ids(&proj) {
            let local_version = match project::resolve_project_component(&proj, &component_id) {
                Ok(comp) => version::get_component_version(&comp),
                Err(_) => None,
            };

            summary.components.total += 1;
            summary.components.unknown += 1;

            component_statuses.push(FleetComponentStatus {
                component_id,
                local_version,
                remote_version: None,
                version_source: "cached".to_string(),
                drift: FleetComponentDrift::Unknown,
                unreleased_commits: 0,
            });
        }

        project_statuses.push(FleetProjectStatus {
            project_id: project_id.clone(),
            server_id: proj.server_id.clone(),
            components: component_statuses,
            health: None,
        });
    }

    Ok(FleetStatusResult {
        projects: project_statuses,
        summary,
    })
}

/// Collect component statuses for a single project via deploy check mode.
fn collect_project_component_statuses(
    project_id: &str,
    proj: &project::Project,
    component_summary: &mut FleetComponentSummary,
) -> Vec<FleetComponentStatus> {
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
        tagged: false,
    };

    match deploy::run(project_id, &config) {
        Ok(result) => {
            let mut statuses = Vec::new();
            for comp_result in &result.results {
                // Get release state for this component
                let (drift, unreleased) =
                    resolve_component_drift(proj, &comp_result.id, &comp_result.component_status);

                component_summary.total += 1;
                match &drift {
                    FleetComponentDrift::Current => component_summary.current += 1,
                    FleetComponentDrift::NeedsUpdate | FleetComponentDrift::BehindRemote => {
                        component_summary.needs_update += 1
                    }
                    FleetComponentDrift::NeedsBump => component_summary.needs_bump += 1,
                    FleetComponentDrift::DocsOnly => component_summary.docs_only += 1,
                    FleetComponentDrift::Unknown => component_summary.unknown += 1,
                }

                statuses.push(FleetComponentStatus {
                    component_id: comp_result.id.clone(),
                    local_version: comp_result.local_version.clone(),
                    remote_version: comp_result.remote_version.clone(),
                    version_source: "live".to_string(),
                    drift,
                    unreleased_commits: unreleased,
                });
            }
            statuses
        }
        Err(_) => {
            // SSH/deploy failed — fall back to local versions
            let mut statuses = Vec::new();
            for component_id in project::project_component_ids(proj) {
                let local_version = match project::resolve_project_component(proj, &component_id) {
                    Ok(comp) => version::get_component_version(&comp),
                    Err(_) => None,
                };

                component_summary.total += 1;
                component_summary.unknown += 1;

                statuses.push(FleetComponentStatus {
                    component_id,
                    local_version,
                    remote_version: None,
                    version_source: "cached".to_string(),
                    drift: FleetComponentDrift::Unknown,
                    unreleased_commits: 0,
                });
            }
            statuses
        }
    }
}

/// Determine component drift by combining deploy status with release state.
fn resolve_component_drift(
    proj: &project::Project,
    component_id: &str,
    deploy_status: &Option<deploy::ComponentStatus>,
) -> (FleetComponentDrift, u32) {
    // Check release state (unreleased commits)
    let release_info = project::resolve_project_component(proj, component_id)
        .ok()
        .and_then(|comp| {
            let state = deploy::calculate_release_state(&comp)?;
            Some((state.status(), state.commits_since_version))
        });

    if let Some((release_status, unreleased)) = release_info {
        match release_status {
            ReleaseStateStatus::NeedsBump => {
                return (FleetComponentDrift::NeedsBump, unreleased);
            }
            ReleaseStateStatus::DocsOnly => {
                return (FleetComponentDrift::DocsOnly, unreleased);
            }
            ReleaseStateStatus::Uncommitted => {
                // Uncommitted changes — still check deploy status
                // but flag as needs_bump since there's local work
                return (FleetComponentDrift::NeedsBump, unreleased);
            }
            _ => {}
        }
    }

    // Fall back to deploy status for drift detection
    let drift = match deploy_status {
        Some(deploy::ComponentStatus::UpToDate) => FleetComponentDrift::Current,
        Some(deploy::ComponentStatus::NeedsUpdate) => FleetComponentDrift::NeedsUpdate,
        Some(deploy::ComponentStatus::BehindRemote) => FleetComponentDrift::BehindRemote,
        Some(deploy::ComponentStatus::Unknown) | None => FleetComponentDrift::Unknown,
    };

    let unreleased = release_info.map(|(_, u)| u).unwrap_or(0);
    (drift, unreleased)
}

/// Compute fleet-wide server summary from the deduped health cache.
fn compute_server_summary(
    health_cache: &HashMap<String, Option<ServerHealth>>,
    _project_statuses: &[FleetProjectStatus],
    summary: &mut FleetStatusSummary,
) {
    summary.servers.total = health_cache.len() as u32;

    for health_opt in health_cache.values() {
        match health_opt {
            Some(h) => {
                if h.warnings.is_empty() {
                    summary.servers.healthy += 1;
                } else {
                    summary.servers.warning += 1;
                }
                for svc in &h.services {
                    if svc.active {
                        summary.servers.services_up += 1;
                    } else {
                        summary.servers.services_down += 1;
                    }
                }
            }
            None => summary.servers.unreachable += 1,
        }
    }
}
