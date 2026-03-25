//! projects — extracted from fleet.rs.

use homeboy::fleet::{self, Fleet, FleetComponentDrift, FleetStatusResult};
use super::super::{CmdResult, DynamicSetArgs};
use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::project::Project;
use homeboy::EntityCrudOutput;
use super::FleetOutput;
use super::FleetExtra;


pub(crate) fn create(
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

pub(crate) fn projects(id: &str) -> CmdResult<FleetOutput> {
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

pub(crate) fn components(id: &str) -> CmdResult<FleetOutput> {
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

pub(crate) fn status(id: &str, cached: bool, health_only: bool) -> CmdResult<FleetOutput> {
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
pub(crate) fn log_fleet_dashboard(result: &FleetStatusResult) {
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
