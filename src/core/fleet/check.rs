use crate::deploy::{self, DeployConfig};
use crate::project;
use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct FleetProjectCheck {
    pub project_id: String,
    pub server_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub components: Vec<FleetComponentCheck>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FleetComponentCheck {
    pub component_id: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub status: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FleetCheckSummary {
    pub total_projects: u32,
    pub projects_checked: u32,
    pub projects_failed: u32,
    pub components_up_to_date: u32,
    pub components_needs_update: u32,
    pub components_unknown: u32,
}

pub fn collect_check(
    fleet_id: &str,
    only_outdated: bool,
) -> crate::Result<(Vec<FleetProjectCheck>, FleetCheckSummary, i32)> {
    let fl = super::load(fleet_id)?;
    let mut project_checks = Vec::new();
    let mut summary = FleetCheckSummary {
        total_projects: fl.project_ids.len() as u32,
        ..Default::default()
    };

    for project_id in &fl.project_ids {
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

                    match status_str {
                        "up_to_date" => summary.components_up_to_date += 1,
                        "needs_update" | "behind_remote" => summary.components_needs_update += 1,
                        _ => summary.components_unknown += 1,
                    }

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
    Ok((project_checks, summary, exit_code))
}
