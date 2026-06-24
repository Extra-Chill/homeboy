use crate::core::deploy::{self, DeployConfig};
use crate::core::project;
use crate::core::release::version;
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
) -> crate::core::Result<(Vec<FleetProjectCheck>, FleetCheckSummary, i32)> {
    let fl = super::load(fleet_id)?;
    let mut project_checks = Vec::new();
    let mut summary = FleetCheckSummary {
        total_projects: fl.project_ids.len() as u32,
        ..Default::default()
    };

    for project_id in &fl.project_ids {
        let config = DeployConfig::check_all_no_pull_head();

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
                        Some(deploy::ComponentStatus::BehindUpstream) => "behind_upstream",
                        Some(deploy::ComponentStatus::SourceStale) => "source_stale",
                        Some(deploy::ComponentStatus::Unknown) | None => "unknown",
                    };

                    match status_str {
                        "up_to_date" => summary.components_up_to_date += 1,
                        "needs_update" | "behind_remote" | "behind_upstream" | "source_stale" => {
                            summary.components_needs_update += 1
                        }
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
                if only_outdated && project::load(project_id).is_ok() {
                    summary.projects_checked += 1;
                    continue;
                }

                if !only_outdated {
                    if let Ok(proj) = project::load(project_id) {
                        let components = cached_project_component_checks(&proj, &mut summary);
                        summary.projects_checked += 1;
                        project_checks.push(FleetProjectCheck {
                            project_id: project_id.clone(),
                            server_id: proj.server_id,
                            status: "checked_cached".to_string(),
                            error: Some(format!(
                                "live check failed; using cached local versions: {e}"
                            )),
                            components,
                        });
                        continue;
                    }

                    summary.projects_failed += 1;
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

fn cached_project_component_checks(
    proj: &project::Project,
    summary: &mut FleetCheckSummary,
) -> Vec<FleetComponentCheck> {
    project::project_component_ids(proj)
        .into_iter()
        .map(|component_id| {
            let local_version = project::resolve_project_component(proj, &component_id)
                .ok()
                .and_then(|comp| version::get_component_version(&comp));

            summary.components_unknown += 1;

            FleetComponentCheck {
                component_id,
                local_version,
                remote_version: None,
                status: "unknown".to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{self, Component};
    use crate::core::fleet::{self, Fleet};
    use crate::core::project::{self, Project, ProjectComponentAttachment};
    use crate::test_support::with_isolated_home;

    #[test]
    fn fleet_check_falls_back_to_cached_components_when_live_check_fails() {
        with_isolated_home(|home| {
            let component_dir = home.path().join("tooling-component");
            std::fs::create_dir_all(&component_dir).expect("component dir");

            let component = Component {
                id: "tooling-component".to_string(),
                local_path: component_dir.to_string_lossy().to_string(),
                remote_path: String::new(),
                ..Default::default()
            };
            component::write_standalone_component_config(&component).expect("component config");

            let mut project = Project {
                id: "local-site".to_string(),
                domain: Some("local-site.test".to_string()),
                server_id: None,
                base_path: Some(home.path().join("site").to_string_lossy().to_string()),
                ..Default::default()
            };
            project.components.push(ProjectComponentAttachment {
                id: "tooling-component".to_string(),
                local_path: component_dir.to_string_lossy().to_string(),
                remote_path: None,
            });
            project::save(&project).expect("project config");

            fleet::save(&Fleet::new(
                "local-fleet".to_string(),
                vec!["local-site".to_string()],
            ))
            .expect("fleet config");

            let (checks, summary, exit_code) =
                collect_check("local-fleet", false).expect("fleet check");

            assert_eq!(exit_code, 0);
            assert_eq!(summary.projects_checked, 1);
            assert_eq!(summary.projects_failed, 0);
            assert_eq!(summary.components_unknown, 1);
            assert_eq!(checks.len(), 1);
            assert_eq!(checks[0].status, "checked_cached");
            assert_eq!(checks[0].components.len(), 1);
            assert_eq!(checks[0].components[0].component_id, "tooling-component");
            assert_eq!(checks[0].components[0].status, "unknown");

            let (outdated_checks, outdated_summary, outdated_exit_code) =
                collect_check("local-fleet", true).expect("outdated fleet check");

            assert_eq!(outdated_exit_code, 0);
            assert_eq!(outdated_summary.projects_checked, 1);
            assert_eq!(outdated_summary.projects_failed, 0);
            assert!(outdated_checks.is_empty());
        });
    }
}
