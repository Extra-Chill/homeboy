//! report — extracted from report.rs.

use crate::error::Result;
use super::super::{calculate_deploy_readiness, collect_status, list, load, Project};
use serde::Serialize;
use crate::output::{CreateOutput, EntityCrudOutput, MergeOutput, RemoveResult};
use super::ProjectListReport;
use super::ProjectStatusReport;
use super::ProjectShowReport;
use super::ProjectListItem;
use super::ProjectComponentVersion;


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

    let hint = if project.server_id.is_none() {
        Some(
            "Local project: Commands execute on this machine. Only deploy requires a server."
                .to_string(),
        )
    } else if project.components.is_empty() {
        Some(format!(
            "No components linked. Use: homeboy project components add {} <component-id> or homeboy project components attach-path {} <component-id> <path>",
            project.id,
            project.id
        ))
    } else {
        None
    };

    let (deploy_ready, deploy_blockers) = calculate_deploy_readiness(&project);

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
