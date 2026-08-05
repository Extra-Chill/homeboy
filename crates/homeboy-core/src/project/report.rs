use serde::Serialize;

use crate::error::Result;
use crate::output::{CreateOutput, EntityCrudOutput, MergeOutput, RemoveResult};

use super::{calculate_deploy_readiness, collect_status, list, load, Project};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectComponentVersion {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectShowReport {
    pub project: Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_ready: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploy_blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectListReport {
    pub projects: Vec<ProjectListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatusReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::server::health::ServerHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_versions: Option<Vec<ProjectComponentVersion>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPathResolutionReport {
    pub requested_path: String,
    pub resolved_path: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_domain: Option<String>,
    pub base_path: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ProjectReportExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<ProjectListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<crate::project::ProjectComponentsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<crate::project::ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_blockers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::server::health::ServerHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_versions: Option<Vec<ProjectComponentVersion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_resolution: Option<ProjectPathResolutionReport>,
}

pub type ProjectReportOutput = EntityCrudOutput<Project, ProjectReportExtra>;

pub fn list_report() -> Result<ProjectListReport> {
    let projects = list()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|p| ProjectListItem {
            id: p.id,
            domain: p.domain,
        })
        .collect();

    let hint = if items.is_empty() {
        Some(
            "No projects configured. Run 'homeboy status --full' to see project context"
                .to_string(),
        )
    } else {
        None
    };

    Ok(ProjectListReport {
        projects: items,
        hint,
    })
}

pub fn show_report(project_id: &str) -> Result<ProjectShowReport> {
    let project = load(project_id)?;

    let hint = if project.components.is_empty() {
        None
    } else if project.server_id.is_none() {
        Some("Local project: Server-deployed components require a server.".to_string())
    } else {
        None
    };

    let (deploy_ready, deploy_blockers) = if project.components.is_empty() {
        (None, Vec::new())
    } else {
        let (ready, blockers) = calculate_deploy_readiness(&project);
        (Some(ready), blockers)
    };

    Ok(ProjectShowReport {
        project,
        hint,
        deploy_ready,
        deploy_blockers,
    })
}

pub fn status_report(project_id: &str, health_only: bool) -> Result<ProjectStatusReport> {
    load(project_id)?;

    let snapshot = collect_status(project_id, health_only);
    let component_versions = snapshot.component_versions.map(|versions| {
        versions
            .into_iter()
            .map(|version| ProjectComponentVersion {
                component_id: version.component_id,
                version: version.version,
                version_source: version.version_source,
            })
            .collect()
    });

    Ok(ProjectStatusReport {
        health: snapshot.health,
        component_versions,
    })
}

pub fn build_list_output(report: ProjectListReport) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.list".to_string(),
        hint: report.hint,
        extra: ProjectReportExtra {
            projects: Some(report.projects),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_show_output(report: ProjectShowReport) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.show".to_string(),
        id: Some(report.project.id.clone()),
        entity: Some(report.project),
        hint: report.hint,
        extra: ProjectReportExtra {
            deploy_ready: report.deploy_ready,
            deploy_blockers: if report.deploy_blockers.is_empty() {
                None
            } else {
                Some(report.deploy_blockers)
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_create_output(result: CreateOutput<Project>) -> (ProjectReportOutput, i32) {
    match result {
        CreateOutput::Single(result) => (
            ProjectReportOutput {
                command: "project.create".to_string(),
                id: Some(result.id),
                entity: Some(result.entity),
                ..Default::default()
            },
            0,
        ),
        CreateOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            (
                ProjectReportOutput {
                    command: "project.create".to_string(),
                    import: Some(summary),
                    ..Default::default()
                },
                exit_code,
            )
        }
    }
}

pub fn build_set_output(result: MergeOutput) -> Result<(ProjectReportOutput, i32)> {
    match result {
        MergeOutput::Single(result) => Ok((
            ProjectReportOutput {
                command: "project.set".to_string(),
                id: Some(result.id.clone()),
                entity: Some(load(&result.id)?),
                updated_fields: result.updated_fields,
                ..Default::default()
            },
            0,
        )),
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                ProjectReportOutput {
                    command: "project.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

pub fn build_remove_output(result: RemoveResult) -> Result<ProjectReportOutput> {
    Ok(ProjectReportOutput {
        command: "project.remove".to_string(),
        id: Some(result.id.clone()),
        entity: Some(load(&result.id)?),
        extra: ProjectReportExtra {
            removed: Some(result.removed_from),
            ..Default::default()
        },
        ..Default::default()
    })
}

pub fn build_rename_output(project: Project) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.rename".to_string(),
        id: Some(project.id.clone()),
        entity: Some(project),
        updated_fields: vec!["id".to_string()],
        ..Default::default()
    }
}

pub fn build_delete_output(project_id: &str) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.delete".to_string(),
        id: Some(project_id.to_string()),
        deleted: vec![project_id.to_string()],
        ..Default::default()
    }
}

pub fn build_components_output(
    project_id: &str,
    action: &str,
    components: crate::project::ProjectComponentsOutput,
) -> ProjectReportOutput {
    ProjectReportOutput {
        command: format!("project.components.{action}"),
        id: Some(project_id.to_string()),
        updated_fields: vec!["componentIds".to_string()],
        extra: ProjectReportExtra {
            components: Some(components),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_pin_output(
    command: &str,
    project_id: &str,
    pin: crate::project::ProjectPinOutput,
) -> ProjectReportOutput {
    ProjectReportOutput {
        command: command.to_string(),
        id: Some(project_id.to_string()),
        extra: ProjectReportExtra {
            pin: Some(pin),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_status_output(project_id: &str, report: ProjectStatusReport) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.status".to_string(),
        id: Some(project_id.to_string()),
        extra: ProjectReportExtra {
            health: report.health,
            component_versions: report.component_versions,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_path_resolution_output(report: ProjectPathResolutionReport) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.resolve-path".to_string(),
        id: Some(report.project_id.clone()),
        hint: Some(format!(
            "{} maps to project '{}'",
            report.requested_path, report.project_id
        )),
        extra: ProjectReportExtra {
            path_resolution: Some(report),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build_init_output(project_id: &str, dir: &std::path::Path) -> ProjectReportOutput {
    ProjectReportOutput {
        command: "project.init".to_string(),
        id: Some(project_id.to_string()),
        entity: load(project_id).ok(),
        hint: Some(format!(
            "Project directory initialized at {}",
            dir.display()
        )),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectComponentAttachment;
    use crate::test_support::with_isolated_home;

    #[test]
    fn show_report_marks_project_not_deploy_ready_when_component_local_path_is_missing() {
        with_isolated_home(|_| {
            crate::project::save(&Project {
                id: "site".to_string(),
                base_path: Some("/srv/site".to_string()),
                components: vec![ProjectComponentAttachment {
                    id: "plugin".to_string(),
                    local_path: "/tmp/homeboy-missing-component-path".to_string(),
                    remote_path: Some("wp-content/plugins/plugin".to_string()),
                    ..Default::default()
                }],
                ..Project::default()
            })
            .expect("save project");

            let report = show_report("site").expect("show report");

            assert_eq!(report.deploy_ready, Some(false));
            assert!(report.deploy_blockers.iter().any(|blocker| {
                blocker.contains(
                    "Component 'plugin' local_path '/tmp/homeboy-missing-component-path' does not exist",
                )
            }));
        });
    }

    #[test]
    fn show_report_marks_repository_provider_project_ready_without_server_fields() {
        with_isolated_home(|_| {
            let repository = tempfile::tempdir().expect("component repository");
            std::fs::write(
                repository.path().join("homeboy.json"),
                serde_json::json!({
                    "id": "provider",
                    "deployment_provider": {
                        "extension": "fixture-extension",
                        "provider": "fixture.deploy",
                        "policy": {}
                    }
                })
                .to_string(),
            )
            .expect("portable component");
            crate::project::save(&Project {
                id: "site".to_string(),
                components: vec![ProjectComponentAttachment {
                    id: "provider".to_string(),
                    local_path: repository.path().to_string_lossy().to_string(),
                    deployment_provider_input: Some(serde_json::json!({ "target": "site" })),
                    ..Default::default()
                }],
                ..Project::default()
            })
            .expect("save project");

            let report = show_report("site").expect("show report");

            assert!(
                report.deploy_ready == Some(true),
                "unexpected blockers: {:?}",
                report.deploy_blockers
            );
            assert!(report.deploy_blockers.is_empty());
        });
    }

    #[test]
    fn show_report_does_not_treat_server_backed_projects_without_components_as_deploys() {
        with_isolated_home(|_| {
            crate::server::save(&crate::server::Server {
                id: "sandbox".to_string(),
                host: "localhost".to_string(),
                user: "tester".to_string(),
                port: 22,
                identity_file: None,
                aliases: Vec::new(),
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("save server");
            crate::project::save(&Project {
                id: "sandbox-project".to_string(),
                server_id: Some("sandbox".to_string()),
                ..Project::default()
            })
            .expect("save project");

            let report = show_report("sandbox-project").expect("show report");

            assert!(report.hint.is_none());
            assert!(report.deploy_ready.is_none());
            assert!(report.deploy_blockers.is_empty());
        });
    }

    #[test]
    fn status_report_marks_an_unprobed_server_backed_zero_component_project_not_checked() {
        with_isolated_home(|_| {
            crate::server::save(&crate::server::Server {
                id: "sandbox".to_string(),
                host: "sandbox.example.test".to_string(),
                user: "tester".to_string(),
                port: 22,
                identity_file: Some("/missing/homeboy-test-key".to_string()),
                aliases: Vec::new(),
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("save server");
            crate::project::save(&Project {
                id: "sandbox-project".to_string(),
                server_id: Some("sandbox".to_string()),
                ..Project::default()
            })
            .expect("save project");

            let report = status_report("sandbox-project", true).expect("status report");
            let health = report.health.expect("configured server health");

            assert_eq!(
                health.state,
                crate::server::health::ServerHealthState::NotChecked
            );
            assert_eq!(
                health.next_action.as_deref(),
                Some("homeboy server status sandbox")
            );
            assert!(report.component_versions.is_none());
        });
    }
}
