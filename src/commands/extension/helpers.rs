//! helpers — extracted from extension.rs.

use super::update_extension;
use super::ExtensionArgs;
use super::ExtensionCommand;
use super::ExtensionOutput;
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::extension::{
    self, extension_ready_status, is_extension_linked, load_extension, run_setup, ExtensionSummary,
    UpdateEntry,
};
use homeboy::project::{self, Project};
use serde::Serialize;

pub fn run(
    args: ExtensionArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ExtensionOutput> {
    match args.command {
        ExtensionCommand::List { project } => list(project),
        ExtensionCommand::Show { extension_id } => show_extension(&extension_id),
        ExtensionCommand::Run {
            extension_id,
            project,
            component,
            input,
            step,
            skip,
            args,
            stream,
            no_stream,
        } => run_extension(
            &extension_id,
            project,
            component,
            input,
            args,
            stream,
            no_stream,
            step,
            skip,
        ),
        ExtensionCommand::Setup { extension_id } => setup_extension(&extension_id),
        ExtensionCommand::Install { source, id } => install_extension(&source, id),
        ExtensionCommand::Update {
            extension_id,
            all,
            force,
        } => update_extension(extension_id.as_deref(), all, force),
        ExtensionCommand::Uninstall { extension_id } => uninstall_extension(&extension_id),
        ExtensionCommand::Action {
            extension_id,
            action_id,
            project,
            data,
        } => run_action(&extension_id, &action_id, project, data),
        ExtensionCommand::Exec {
            extension_id,
            component,
            args,
        } => exec_extension_tool(&extension_id, component, args),
        ExtensionCommand::Set {
            extension_id,
            json,
            replace,
        } => set_extension(extension_id.as_deref(), &json, &replace),
    }
}

pub(crate) fn list(project: Option<String>) -> CmdResult<ExtensionOutput> {
    let project_config: Option<Project> = project.as_ref().and_then(|id| project::load(id).ok());
    let summaries = extension::list_summaries(project_config.as_ref());

    Ok((
        ExtensionOutput::List {
            project_id: project,
            extensions: summaries,
        },
        0,
    ))
}

pub(crate) fn run_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<String>,
    data: Option<String>,
) -> CmdResult<ExtensionOutput> {
    let response = homeboy::extension::run_action(
        extension_id,
        action_id,
        project_id.as_deref(),
        data.as_deref(),
    )?;

    Ok((
        ExtensionOutput::Action {
            extension_id: extension_id.to_string(),
            action_id: action_id.to_string(),
            project_id,
            response,
        },
        0,
    ))
}
