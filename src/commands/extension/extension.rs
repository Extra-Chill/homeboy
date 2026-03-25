//! extension — extracted from extension.rs.

use super::ActionDetail;
use super::CliDetail;
use super::ExtensionDetail;
use super::ExtensionOutput;
use super::RequiresDetail;
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::extension::{
    self, extension_ready_status, is_extension_linked, load_extension, run_setup, ExtensionSummary,
    UpdateEntry,
};
use homeboy::project::{self, Project};
use serde::Serialize;

pub(crate) fn show_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let extension = load_extension(extension_id)?;
    let ready_status = extension_ready_status(&extension);
    let linked = is_extension_linked(&extension.id);

    let has_setup = extension
        .runtime()
        .and_then(|r| r.setup_command.as_ref())
        .map(|_| true);
    let has_ready_check = extension
        .runtime()
        .and_then(|r| r.ready_check.as_ref())
        .map(|_| true);

    let cli = extension.cli.as_ref().map(|c| CliDetail {
        tool: c.tool.clone(),
        display_name: c.display_name.clone(),
        command_template: c.command_template.clone(),
        default_cli_path: c.default_cli_path.clone(),
    });

    let actions: Vec<ActionDetail> = extension
        .actions
        .iter()
        .map(|a| ActionDetail {
            id: a.id.clone(),
            label: a.label.clone(),
            action_type: a.action_type.clone(),
            endpoint: a.endpoint.clone(),
            method: a.method.clone(),
            command: a.command.clone(),
        })
        .collect();

    let requires = extension.requires.as_ref().map(|r| RequiresDetail {
        extensions: r.extensions.clone(),
        components: r.components.clone(),
    });

    let source_revision = homeboy::extension::read_source_revision(&extension.id);

    let detail = ExtensionDetail {
        id: extension.id.clone(),
        name: extension.name.clone(),
        version: extension.version.clone(),
        description: extension.description.clone(),
        author: extension.author.clone(),
        homepage: extension.homepage.clone(),
        source_url: extension.source_url.clone(),
        runtime: if extension.executable.is_some() {
            "executable".to_string()
        } else {
            "platform".to_string()
        },
        has_setup,
        has_ready_check,
        ready: ready_status.ready,
        ready_reason: ready_status.reason,
        ready_detail: ready_status.detail,
        linked,
        path: extension.extension_path.clone().unwrap_or_default(),
        source_revision,
        cli,
        actions,
        inputs: extension.inputs().to_vec(),
        settings: extension.settings.clone(),
        requires,
    };

    Ok((ExtensionOutput::Show { extension: detail }, 0))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_extension(
    extension_id: &str,
    project: Option<String>,
    component: Option<String>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    stream: bool,
    no_stream: bool,
    step: Option<String>,
    skip: Option<String>,
) -> CmdResult<ExtensionOutput> {
    use homeboy::extension::{ExtensionExecutionMode, ExtensionStepFilter};

    let mode = if no_stream {
        ExtensionExecutionMode::Captured
    } else if stream || crate::commands::utils::tty::is_stdout_tty() {
        ExtensionExecutionMode::Interactive
    } else {
        ExtensionExecutionMode::Captured
    };

    let filter = ExtensionStepFilter { step, skip };

    let result = homeboy::extension::run_extension(
        extension_id,
        project.as_deref(),
        component.as_deref(),
        inputs,
        args,
        mode,
        filter,
    )?;

    Ok((
        ExtensionOutput::Run {
            extension_id: extension_id.to_string(),
            project_id: result.project_id,
            output: result.output,
        },
        result.exit_code,
    ))
}

pub(crate) fn install_extension(source: &str, id: Option<String>) -> CmdResult<ExtensionOutput> {
    let result = homeboy::extension::install(source, id.as_deref())?;
    let linked = is_extension_linked(&result.extension_id);

    Ok((
        ExtensionOutput::Install {
            extension_id: result.extension_id,
            source: result.url,
            path: result.path.to_string_lossy().to_string(),
            linked,
            source_revision: result.source_revision,
        },
        0,
    ))
}

pub(crate) fn uninstall_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let was_linked = is_extension_linked(extension_id);
    let path = homeboy::extension::uninstall(extension_id)?;

    Ok((
        ExtensionOutput::Uninstall {
            extension_id: extension_id.to_string(),
            path: path.to_string_lossy().to_string(),
            was_linked,
        },
        0,
    ))
}

pub(crate) fn setup_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let result = run_setup(extension_id)?;

    Ok((
        ExtensionOutput::Setup {
            extension_id: extension_id.to_string(),
        },
        result.exit_code,
    ))
}

pub(crate) fn set_extension(
    extension_id: Option<&str>,
    json: &str,
    replace_fields: &[String],
) -> CmdResult<ExtensionOutput> {
    match homeboy::extension::merge(extension_id, json, replace_fields)? {
        homeboy::MergeOutput::Single(result) => Ok((
            ExtensionOutput::Set {
                extension_id: result.id,
                updated_fields: result.updated_fields,
            },
            0,
        )),
        homeboy::MergeOutput::Bulk(batch) => {
            let exit_code = batch.exit_code();
            Ok((ExtensionOutput::SetBatch { batch }, exit_code))
        }
    }
}

pub(crate) fn exec_extension_tool(
    extension_id: &str,
    component: Option<String>,
    args: Vec<String>,
) -> CmdResult<ExtensionOutput> {
    let exit_code = extension::exec_tool(extension_id, component.as_deref(), &args)?;

    Ok((
        ExtensionOutput::Exec {
            extension_id: extension_id.to_string(),
            output: None,
        },
        exit_code,
    ))
}
