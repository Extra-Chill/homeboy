//! write_project_components — extracted from project.rs.

use std::path::Path;
use super::super::CmdResult;
use homeboy::project::{self};
use clap::{Args, Subcommand, ValueEnum};


pub(crate) fn list() -> CmdResult<ProjectOutput> {
    Ok((project::build_list_output(project::list_report()?), 0))
}

pub(crate) fn set(args: super::DynamicSetArgs) -> CmdResult<ProjectOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    project::build_set_output(project::merge(
        args.id.as_deref(),
        &json_string,
        &replace_fields,
    )?)
}

pub(crate) fn remove(project_id: Option<&str>, json: &str) -> CmdResult<ProjectOutput> {
    Ok((
        project::build_remove_output(project::remove_from_json(project_id, json)?)?,
        0,
    ))
}

pub(crate) fn components_list(project_id: &str) -> CmdResult<ProjectOutput> {
    Ok((
        project::build_components_output(project_id, "list", project::list_components(project_id)?),
        0,
    ))
}

pub(crate) fn components_set(project_id: &str, json: &str) -> CmdResult<ProjectOutput> {
    let components = project::set_components(project_id, json)?;
    Ok(write_project_components_response(
        project_id, "set", components,
    ))
}

pub(crate) fn components_attach_path(project_id: &str, local_path: &str) -> CmdResult<ProjectOutput> {
    let components = project::attach_component_path_report(project_id, Path::new(local_path))?;
    Ok(write_project_components_response(
        project_id,
        "attach_path",
        components,
    ))
}

pub(crate) fn components_remove(project_id: &str, component_ids: Vec<String>) -> CmdResult<ProjectOutput> {
    let components = project::remove_components_report(project_id, component_ids)?;
    Ok(write_project_components_response(
        project_id, "remove", components,
    ))
}

pub(crate) fn components_clear(project_id: &str) -> CmdResult<ProjectOutput> {
    let components = project::clear_components(project_id)?;
    Ok(write_project_components_response(
        project_id, "clear", components,
    ))
}

pub(crate) fn write_project_components_response(
    project_id: &str,
    action: &str,
    components: homeboy::project::ProjectComponentsOutput,
) -> (ProjectOutput, i32) {
    (
        project::build_components_output(project_id, action, components),
        0,
    )
}

pub(crate) fn pin(command: ProjectPinCommand) -> CmdResult<ProjectOutput> {
    match command {
        ProjectPinCommand::List { project_id, r#type } => pin_list(&project_id, r#type),
        ProjectPinCommand::Add {
            project_id,
            path,
            r#type,
            label,
            tail,
        } => pin_add(&project_id, &path, r#type, label, tail),
        ProjectPinCommand::Remove {
            project_id,
            path,
            r#type,
        } => pin_remove(&project_id, &path, r#type),
    }
}

pub(crate) fn pin_list(project_id: &str, pin_type: ProjectPinType) -> CmdResult<ProjectOutput> {
    Ok((
        project::build_pin_output(
            "project.pin.list",
            project_id,
            project::list_pins(project_id, map_pin_type(pin_type))?,
        ),
        0,
    ))
}

pub(crate) fn pin_add(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
    label: Option<String>,
    tail: u32,
) -> CmdResult<ProjectOutput> {
    let pin = project::add_pin(
        project_id,
        map_pin_type(pin_type),
        path,
        project::PinOptions {
            label,
            tail_lines: tail,
        },
    )?;

    Ok((
        project::build_pin_output("project.pin.add", project_id, pin),
        0,
    ))
}

pub(crate) fn pin_remove(project_id: &str, path: &str, pin_type: ProjectPinType) -> CmdResult<ProjectOutput> {
    let pin = project::remove_pin(project_id, map_pin_type(pin_type), path)?;

    Ok((
        project::build_pin_output("project.pin.remove", project_id, pin),
        0,
    ))
}

pub(crate) fn map_pin_type(pin_type: ProjectPinType) -> project::PinType {
    match pin_type {
        ProjectPinType::File => project::PinType::File,
        ProjectPinType::Log => project::PinType::Log,
    }
}
