//! helpers — extracted from fleet.rs.

use homeboy::fleet::{self, Fleet, FleetComponentDrift, FleetStatusResult};
use super::super::{CmdResult, DynamicSetArgs};
use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::project::Project;
use homeboy::EntityCrudOutput;
use super::status;
use super::FleetOutput;
use super::FleetArgs;
use super::FleetCommand;
use super::projects;
use super::components;
use super::create;


pub fn run(args: FleetArgs, _global: &super::GlobalArgs) -> CmdResult<FleetOutput> {
    match args.command {
        FleetCommand::Create {
            id,
            projects,
            description,
        } => create(&id, projects.unwrap_or_default(), description),
        FleetCommand::Show { id } => show(&id),
        FleetCommand::Set { args } => set(args),
        FleetCommand::Delete { id } => delete(&id),
        FleetCommand::List => list(),
        FleetCommand::Add { id, project } => add(&id, &project),
        FleetCommand::Remove { id, project } => remove(&id, &project),
        FleetCommand::Projects { id } => projects(&id),
        FleetCommand::Components { id } => components(&id),
        FleetCommand::Status {
            id,
            cached,
            health_only,
        } => status(&id, cached, health_only),
        FleetCommand::Check { id, outdated } => check(&id, outdated),
        FleetCommand::Exec {
            id,
            command,
            check,
            user,
            serial: _,
        } => exec(&id, command, check, user),
    }
}

pub(crate) fn show(id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::load(id)?;

    Ok((
        FleetOutput {
            command: "fleet.show".to_string(),
            id: Some(id.to_string()),
            entity: Some(fl),
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn set(args: DynamicSetArgs) -> CmdResult<FleetOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match fleet::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        homeboy::MergeOutput::Single(result) => {
            let fl = fleet::load(&result.id)?;
            Ok((
                FleetOutput {
                    command: "fleet.set".to_string(),
                    id: Some(result.id),
                    entity: Some(fl),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        homeboy::MergeOutput::Bulk(_) => Err(homeboy::Error::internal_unexpected(
            "Unexpected bulk result for single fleet".to_string(),
        )),
    }
}

pub(crate) fn delete(id: &str) -> CmdResult<FleetOutput> {
    fleet::delete(id)?;

    Ok((
        FleetOutput {
            command: "fleet.delete".to_string(),
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn list() -> CmdResult<FleetOutput> {
    let fleets = fleet::list()?;

    Ok((
        FleetOutput {
            command: "fleet.list".to_string(),
            entities: fleets,
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn add(fleet_id: &str, project_id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::add_project(fleet_id, project_id)?;

    Ok((
        FleetOutput {
            command: "fleet.add".to_string(),
            id: Some(fleet_id.to_string()),
            entity: Some(fl),
            updated_fields: vec!["project_ids".to_string()],
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn remove(fleet_id: &str, project_id: &str) -> CmdResult<FleetOutput> {
    let fl = fleet::remove_project(fleet_id, project_id)?;

    Ok((
        FleetOutput {
            command: "fleet.remove".to_string(),
            id: Some(fleet_id.to_string()),
            entity: Some(fl),
            updated_fields: vec!["project_ids".to_string()],
            ..Default::default()
        },
        0,
    ))
}
