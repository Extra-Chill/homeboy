//! helpers — extracted from project.rs.

use super::super::CmdResult;
use super::components_attach_path;
use super::components_clear;
use super::components_list;
use super::components_remove;
use super::components_set;
use super::list;
use super::pin;
use super::remove;
use super::set;
use super::ProjectArgs;
use super::ProjectCommand;
use super::ProjectComponentsCommand;
use super::ProjectOutput;
use clap::{Args, Subcommand, ValueEnum};
use homeboy::log_status;
use homeboy::project::{self};
use std::path::Path;

pub fn run(args: ProjectArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ProjectOutput> {
    match args.command {
        ProjectCommand::List => list(),
        ProjectCommand::Show { project_id } => show(&project_id),
        ProjectCommand::Create {
            json,
            skip_existing,
            id,
            domain,
            server_id,
            base_path,
            table_prefix,
        } => {
            let json_spec = if let Some(spec) = json {
                spec
            } else {
                let id = id.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "id",
                        "Missing required argument: id",
                        None,
                        None,
                    )
                })?;

                let new_project = project::Project {
                    id: id.clone(),
                    domain,
                    server_id,
                    base_path,
                    table_prefix,
                    ..Default::default()
                };

                homeboy::config::serialize_with_id(&new_project, &id)?
            };

            Ok(project::build_create_output(project::create(
                &json_spec,
                skip_existing,
            )?))
        }
        ProjectCommand::Set { args } => set(args),
        ProjectCommand::Remove {
            project_id,
            spec,
            json,
        } => {
            let json_spec = json.or(spec).ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "spec",
                    "Provide JSON spec or use --json flag",
                    None,
                    None,
                )
            })?;
            remove(project_id.as_deref(), &json_spec)
        }
        ProjectCommand::Rename { project_id, new_id } => rename(&project_id, &new_id),
        ProjectCommand::Components { command } => components(command),
        ProjectCommand::Pin { command } => pin(command),
        ProjectCommand::Delete { project_id } => delete(&project_id),
        ProjectCommand::Init { project_id } => init(&project_id),
        ProjectCommand::Status {
            project_id,
            health_only,
        } => status(&project_id, health_only),
    }
}

pub(crate) fn show(project_id: &str) -> CmdResult<ProjectOutput> {
    Ok((
        project::build_show_output(project::show_report(project_id)?),
        0,
    ))
}

pub(crate) fn rename(project_id: &str, new_id: &str) -> CmdResult<ProjectOutput> {
    Ok((
        project::build_rename_output(project::rename(project_id, new_id)?),
        0,
    ))
}

pub(crate) fn delete(project_id: &str) -> CmdResult<ProjectOutput> {
    project::delete(project_id)?;

    Ok((project::build_delete_output(project_id), 0))
}

pub(crate) fn init(project_id: &str) -> CmdResult<ProjectOutput> {
    let dir = project::init_project_dir(project_id)?;

    Ok((project::build_init_output(project_id, &dir), 0))
}

pub(crate) fn components(command: ProjectComponentsCommand) -> CmdResult<ProjectOutput> {
    match command {
        ProjectComponentsCommand::List { project_id } => components_list(&project_id),
        ProjectComponentsCommand::Set { project_id, json } => components_set(&project_id, &json),
        ProjectComponentsCommand::AttachPath {
            project_id,
            local_path,
        } => components_attach_path(&project_id, &local_path),
        ProjectComponentsCommand::Remove {
            project_id,
            component_ids,
        } => components_remove(&project_id, component_ids),
        ProjectComponentsCommand::Clear { project_id } => components_clear(&project_id),
    }
}

pub(crate) fn status(project_id: &str, health_only: bool) -> CmdResult<ProjectOutput> {
    log_status!("project", "Checking '{}'...", project_id);

    Ok((
        project::build_status_output(project_id, project::status_report(project_id, health_only)?),
        0,
    ))
}
