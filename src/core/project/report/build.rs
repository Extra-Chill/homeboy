//! build — extracted from report.rs.

use crate::error::Result;
use crate::output::{CreateOutput, EntityCrudOutput, MergeOutput, RemoveResult};
use super::super::{calculate_deploy_readiness, collect_status, list, load, Project};
use serde::Serialize;
use super::ProjectReportExtra;
use super::ProjectListReport;
use super::ProjectStatusReport;
use super::ProjectShowReport;
use super::ProjectReportOutput;


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
            deploy_ready: Some(report.deploy_ready),
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
