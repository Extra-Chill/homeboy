use serde::Serialize;
use std::collections::HashMap;

use crate::component::{self, Component};
use crate::context::resolve_project_ssh;
use crate::error::ErrorCode;
use crate::module::{find_module_by_tool, CliConfig};
use crate::project::{self, Project, ProjectRecord};
use crate::shell;
use crate::ssh::{execute_local_command, CommandOutput};
use crate::template::{render_map, TemplateVars};
use crate::token;
use crate::{Error, Result};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CliToolResult {
    pub tool: String,
    pub module_id: String,
    pub identifier: String,
    pub target_domain: Option<String>,
    pub executed_command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run(tool: &str, identifier: &str, args: &[String]) -> Result<CliToolResult> {
    if let Some(result) = try_run_for_component(tool, identifier, args) {
        return result;
    }

    run_for_project(tool, identifier, args)
}

fn try_run_for_component(
    tool: &str,
    identifier: &str,
    args: &[String],
) -> Option<Result<CliToolResult>> {
    match component::load(identifier) {
        Ok(component) => {
            let module = find_module_by_tool(tool)?;
            let cli_config = module.cli.as_ref()?;

            let command = build_component_command(&component, cli_config, args);
            let output = execute_local_command(&command);

            Some(Ok(CliToolResult {
                tool: tool.to_string(),
                module_id: module.id.clone(),
                identifier: identifier.to_string(),
                target_domain: None,
                executed_command: command,
                stdout: output.stdout,
                stderr: output.stderr,
                exit_code: output.exit_code,
            }))
        }
        Err(e) if e.code == ErrorCode::ComponentNotFound => None,
        Err(e) => Some(Err(e)),
    }
}

fn build_component_command(
    component: &Component,
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

fn run_for_project(tool: &str, project_id: &str, args: &[String]) -> Result<CliToolResult> {
    run_for_project_with_executor(
        tool,
        project_id,
        args,
        project::load_record,
        execute_local_command,
    )
}

fn run_for_project_with_executor(
    tool: &str,
    project_id: &str,
    args: &[String],
    project_loader: fn(&str) -> Result<ProjectRecord>,
    local_executor: fn(&str) -> CommandOutput,
) -> Result<CliToolResult> {
    if args.is_empty() {
        return Err(Error::other("No command provided".to_string()));
    }

    let module = find_module_by_tool(tool)
        .ok_or_else(|| Error::other(format!("No module provides tool '{}'", tool)))?;

    let cli_config = module.cli.as_ref().ok_or_else(|| {
        Error::other(format!(
            "Module '{}' does not have CLI configuration",
            module.id
        ))
    })?;

    let project = project_loader(project_id)?;

    let (target_domain, command) = build_project_command(&project, cli_config, args)?;

    let output = if project
        .config
        .server_id
        .as_ref()
        .map_or(true, |s| s.is_empty())
    {
        local_executor(&command)
    } else {
        let ctx = resolve_project_ssh(project_id)?;
        ctx.client.execute(&command)
    };

    Ok(CliToolResult {
        tool: tool.to_string(),
        module_id: module.id,
        identifier: project_id.to_string(),
        target_domain: Some(target_domain),
        executed_command: command,
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
    })
}

fn build_project_command(
    project: &ProjectRecord,
    cli_config: &CliConfig,
    args: &[String],
) -> Result<(String, String)> {
    let base_path = project
        .config
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config("Base path not configured".to_string()))?;

    let (target_domain, command_args) = resolve_subtarget(&project.config, args);

    if command_args.is_empty() {
        return Err(Error::other(
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

fn resolve_subtarget(project: &Project, args: &[String]) -> (String, Vec<String>) {
    let default_domain = project.domain.clone();

    if project.sub_targets.is_empty() {
        return (default_domain, args.to_vec());
    }

    let Some(sub_id) = args.first() else {
        return (default_domain, args.to_vec());
    };

    if let Some(subtarget) = project.sub_targets.iter().find(|t| {
        project::slugify_id(&t.name).ok().as_deref() == Some(sub_id)
            || token::identifier_eq(&t.name, sub_id)
    }) {
        return (subtarget.domain.clone(), args[1..].to_vec());
    }

    (default_domain, args.to_vec())
}
