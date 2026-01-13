use homeboy::config::{
    ComponentConfiguration, ConfigManager, ProjectConfiguration, ProjectRecord, SlugIdentifiable,
};
use homeboy::error::ErrorCode;
use homeboy::context::resolve_project_ssh;
use homeboy::module::{find_module_by_tool, CliConfig};
use homeboy::shell;
use homeboy::ssh::{execute_local_command, CommandOutput};
use homeboy::template::{render_map, TemplateVars};
use homeboy::token;
use serde::Serialize;
use std::collections::HashMap;

use super::CmdResult;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliOutput {
    pub command: String,
    pub tool: String,
    pub module_id: String,
    pub project_id: String,
    pub args: Vec<String>,
    pub target_domain: Option<String>,
    pub executed_command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run(
    tool: &str,
    identifier: &str,
    args: Vec<String>,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<CliOutput> {
    // Try component first
    if let Some(result) = try_run_for_component(tool, identifier, args.clone()) {
        return result;
    }

    // Fall back to project-based execution
    run_with_loader_and_executor(
        tool,
        identifier,
        args,
        ConfigManager::load_project_record,
        execute_local_command,
    )
}

fn try_run_for_component(
    tool: &str,
    identifier: &str,
    args: Vec<String>,
) -> Option<CmdResult<CliOutput>> {
    match ConfigManager::load_component(identifier) {
        Ok(component) => {
            let module = find_module_by_tool(tool)?;
            let cli_config = module.cli.as_ref()?;

            let command = build_component_command(&component, cli_config, &args);
            let output = execute_local_command(&command);

            Some(Ok((
                CliOutput {
                    command: "cli.run".to_string(),
                    tool: tool.to_string(),
                    module_id: module.id.clone(),
                    project_id: identifier.to_string(),
                    args,
                    target_domain: None,
                    executed_command: command,
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_code: output.exit_code,
                },
                output.exit_code,
            )))
        }
        Err(e) if e.code == ErrorCode::ComponentNotFound => None,
        Err(e) => Some(Err(e)),
    }
}

fn build_component_command(
    component: &ComponentConfiguration,
    cli_config: &CliConfig,
    args: &[String],
) -> String {
    let mut variables = HashMap::new();
    variables.insert(
        TemplateVars::SITE_PATH.to_string(),
        component.local_path.clone(),
    );
    variables.insert(
        TemplateVars::CLI_PATH.to_string(),
        cli_config
            .default_cli_path
            .clone()
            .unwrap_or_else(|| cli_config.tool.clone()),
    );
    variables.insert(TemplateVars::ARGS.to_string(), shell::quote_args(args));

    render_map(&cli_config.command_template, &variables)
}

fn run_with_loader_and_executor(
    tool: &str,
    project_id: &str,
    args: Vec<String>,
    project_loader: fn(&str) -> homeboy::Result<ProjectRecord>,
    local_executor: fn(&str) -> CommandOutput,
) -> CmdResult<CliOutput> {
    if args.is_empty() {
        return Err(homeboy::Error::other(
            "No command provided".to_string(),
        ));
    }

    let module = find_module_by_tool(tool)
        .ok_or_else(|| homeboy::Error::other(format!("No module provides tool '{}'", tool)))?;

    let cli_config = module.cli.as_ref().ok_or_else(|| {
        homeboy::Error::other(format!(
            "Module '{}' does not have CLI configuration",
            module.id
        ))
    })?;

    let project = project_loader(project_id)?;

    if !project.config.has_module(&module.id) {
        return Err(homeboy::Error::other(format!(
            "Project '{}' does not have the '{}' module enabled",
            project_id, module.id
        )));
    }

    let (target_domain, command) = build_command(&project, cli_config, &args)?;

    // Execute locally if no server configured, otherwise via SSH
    let output = if project.config.server_id.as_ref().map_or(true, |s| s.is_empty()) {
        local_executor(&command)
    } else {
        let ctx = resolve_project_ssh(project_id)?;
        ctx.client.execute(&command)
    };

    Ok((
        CliOutput {
            command: "cli.run".to_string(),
            tool: tool.to_string(),
            module_id: module.id,
            project_id: project_id.to_string(),
            args,
            target_domain: Some(target_domain),
            executed_command: command,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        },
        output.exit_code,
    ))
}

fn build_command(
    project: &ProjectRecord,
    cli_config: &CliConfig,
    args: &[String],
) -> homeboy::Result<(String, String)> {
    let base_path = project
        .config
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| homeboy::Error::config("Base path not configured".to_string()))?;

    let (target_domain, command_args) = resolve_subtarget(&project.config, args);

    if command_args.is_empty() {
        return Err(homeboy::Error::other(
            "No command provided after subtarget".to_string(),
        ));
    }

    let cli_path = cli_config
        .default_cli_path
        .clone()
        .unwrap_or_else(|| cli_config.tool.clone());

    let mut variables = HashMap::new();
    variables.insert(TemplateVars::PROJECT_ID.to_string(), project.id.clone());
    variables.insert(TemplateVars::DOMAIN.to_string(), target_domain.clone());
    variables.insert(
        TemplateVars::ARGS.to_string(),
        shell::quote_args(&command_args),
    );
    variables.insert(TemplateVars::SITE_PATH.to_string(), base_path);
    variables.insert(TemplateVars::CLI_PATH.to_string(), cli_path);

    Ok((
        target_domain,
        render_map(&cli_config.command_template, &variables),
    ))
}

fn resolve_subtarget(
    project: &ProjectConfiguration,
    args: &[String],
) -> (String, Vec<String>) {
    let default_domain = project.domain.clone();

    if project.sub_targets.is_empty() {
        return (default_domain, args.to_vec());
    }

    let Some(sub_id) = args.first() else {
        return (default_domain, args.to_vec());
    };

    if let Some(subtarget) = project.sub_targets.iter().find(|t| {
        t.slug_id().ok().as_deref() == Some(sub_id) || token::identifier_eq(&t.name, sub_id)
    }) {
        return (subtarget.domain.clone(), args[1..].to_vec());
    }

    (default_domain, args.to_vec())
}
