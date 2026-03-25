//! check — extracted from fleet.rs.

use super::super::{CmdResult, DynamicSetArgs};
use super::FleetExtra;
use super::FleetOutput;
use clap::{Args, Subcommand};
use homeboy::fleet::{self, Fleet, FleetComponentDrift, FleetStatusResult};
use homeboy::project::Project;
use homeboy::EntityCrudOutput;
use serde::Serialize;

pub(crate) fn check(id: &str, only_outdated: bool) -> CmdResult<FleetOutput> {
    let (project_checks, summary, exit_code) = fleet::collect_check(id, only_outdated)?;

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

pub(crate) fn exec(
    id: &str,
    command: Vec<String>,
    check: bool,
    user: Option<String>,
) -> CmdResult<FleetOutput> {
    let (results, summary, exit_code) = fleet::collect_exec(id, command, check, user)?;

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
