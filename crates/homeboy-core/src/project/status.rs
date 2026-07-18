use crate::release_provider;
use crate::server::health::{self, ServerHealth};

#[derive(Debug, Clone)]
pub struct ProjectComponentStatus {
    pub component_id: String,
    pub version: Option<String>,
    pub version_source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectStatusSnapshot {
    pub health: Option<ServerHealth>,
    pub component_versions: Option<Vec<ProjectComponentStatus>>,
}

pub fn collect_status(project_id: &str, health_only: bool) -> ProjectStatusSnapshot {
    let proj = match super::load(project_id) {
        Ok(project) => project,
        Err(_) => {
            return ProjectStatusSnapshot {
                health: None,
                component_versions: None,
            };
        }
    };

    let health = health::collect_project_health(&proj);
    let component_versions = if health_only {
        None
    } else {
        collect_component_versions(project_id)
    };

    ProjectStatusSnapshot {
        health,
        component_versions,
    }
}

fn collect_component_versions(project_id: &str) -> Option<Vec<ProjectComponentStatus>> {
    release_provider::deploy_component_statuses(project_id)
        .ok()
        .map(|statuses| {
            statuses
                .iter()
                .map(|s| ProjectComponentStatus {
                    component_id: s.id.clone(),
                    version: s.remote_version.clone(),
                    version_source: Some("live".to_string()),
                })
                .collect()
        })
}
