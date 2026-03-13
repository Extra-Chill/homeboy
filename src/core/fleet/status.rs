use crate::deploy::{self, DeployConfig};
use crate::health::{self, ServerHealth};
use crate::project;
use crate::version;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FleetComponentStatus {
    pub component_id: String,
    pub version: Option<String>,
    pub version_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetProjectStatus {
    pub project_id: String,
    pub server_id: Option<String>,
    pub components: Vec<FleetComponentStatus>,
    pub health: Option<ServerHealth>,
}

pub fn collect_status(
    fleet_id: &str,
    cached: bool,
    health_only: bool,
) -> crate::Result<Vec<FleetProjectStatus>> {
    let fl = super::load(fleet_id)?;
    let mut project_statuses = Vec::new();

    if cached {
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
        for project_id in &fl.project_ids {
            let proj = match project::load(project_id) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let health = health::collect_project_health(&proj);

            if health_only {
                project_statuses.push(FleetProjectStatus {
                    project_id: project_id.clone(),
                    server_id: proj.server_id.clone(),
                    components: vec![],
                    health,
                });
                continue;
            }

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
                Err(_) => {
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

    Ok(project_statuses)
}
