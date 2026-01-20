use serde::Serialize;
use std::collections::HashMap;

use crate::component::{self, Component};
use crate::context::resolve_project_ssh;
use crate::error::ErrorCode;
use crate::executor;
use crate::module::{find_module_by_tool, CliConfig};
use crate::project::{self, Project};
use crate::ssh::{execute_local_command, CommandOutput};
use crate::utils::shell;
use crate::utils::template::{render_map, TemplateVars};
use crate::utils::token;
use crate::{Error, Result};

#[derive(Serialize, Clone)]

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
    run_for_project_with_executor(tool, project_id, args, project::load, execute_local_command)
}

fn run_for_project_with_executor(
    tool: &str,
    project_id: &str,
    args: &[String],
    project_loader: fn(&str) -> Result<Project>,
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

    let (target_domain, command_args) = resolve_subtarget(&project, args)?;

    if command_args.is_empty() {
        return Err(Error::other(
            "No command provided after subtarget".to_string(),
        ));
    }

    // Try direct execution first (bypasses shell escaping issues)
    let (output, executed_command) = if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        let result = executor::execute_for_project_direct(
            &project,
            cli_config,
            &module.id,
            &command_args,
            &target_domain,
        );
        match result {
            Ok(cmd_output) => (
                cmd_output,
                format!("{} {}", cli_config.tool, command_args.join(" ")),
            ),
            Err(_) => {
                // Fallback to shell execution if direct fails
                let (_, rendered_cmd) =
                    build_project_command(&project, cli_config, &module.id, args)?;
                (local_executor(&rendered_cmd), rendered_cmd)
            }
        }
    } else {
        let ctx = resolve_project_ssh(project_id)?;
        let (_, rendered_cmd) = build_project_command(&project, cli_config, &module.id, args)?;
        let cmd_output = ctx.client.execute(&rendered_cmd);
        (cmd_output, rendered_cmd)
    };

    Ok(CliToolResult {
        tool: tool.to_string(),
        module_id: module.id,
        identifier: project_id.to_string(),
        target_domain: Some(target_domain),
        executed_command,
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
    })
}

fn build_project_command(
    project: &Project,
    cli_config: &CliConfig,
    module_id: &str,
    args: &[String],
) -> Result<(String, String)> {
    let base_path = project
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config("Base path not configured".to_string()))?;

    let (target_domain, command_args) = resolve_subtarget(project, args)?;

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

    let mut rendered = render_map(&cli_config.command_template, &variables);

    // Append settings-based flags from module config
    if !cli_config.settings_flags.is_empty() {
        if let Some(modules) = &project.modules {
            if let Some(module_config) = modules.get(module_id) {
                for (setting_key, flag_template) in &cli_config.settings_flags {
                    if let Some(value) = module_config.settings.get(setting_key) {
                        if let Some(value_str) = value.as_str() {
                            if !value_str.is_empty() {
                                let flag = flag_template
                                    .replace("{{value}}", &shell::quote_arg(value_str));
                                rendered.push(' ');
                                rendered.push_str(&flag);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok((target_domain, rendered))
}

fn resolve_subtarget(project: &Project, args: &[String]) -> Result<(String, Vec<String>)> {
    let require_domain = || {
        Error::validation_invalid_argument(
            "domain",
            "This operation requires a domain to be configured on the project",
            Some(project.id.clone()),
            None,
        )
    };

    if project.sub_targets.is_empty() {
        let domain = project.domain.clone().ok_or_else(require_domain)?;
        return Ok((domain, args.to_vec()));
    }

    let Some(sub_id) = args.first() else {
        let subtarget_list = project
            .sub_targets
            .iter()
            .map(|t| {
                let slug = project::slugify_id(&t.name).unwrap_or_else(|_| t.name.clone());
                format!("- {} (use: {})", t.name, slug)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::validation_invalid_argument(
            "subtarget",
            format!(
                "This project has subtargets configured. You must specify which subtarget to use.\n\nAvailable subtargets for project '{}':\n{}",
                project.id, subtarget_list
            ),
            Some(project.id.clone()),
            None,
        ));
    };

    if let Some(subtarget) = project.sub_targets.iter().find(|t| {
        project::slugify_id(&t.name).ok().as_deref() == Some(sub_id)
            || token::identifier_eq(&t.name, sub_id)
    }) {
        return Ok((subtarget.domain.clone(), args[1..].to_vec()));
    }

    let subtarget_list = project
        .sub_targets
        .iter()
        .map(|t| {
            let slug = project::slugify_id(&t.name).unwrap_or_else(|_| t.name.clone());
            format!("- {} (use: {})", t.name, slug)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Err(Error::validation_invalid_argument(
        "subtarget",
        format!(
            "Subtarget '{}' not found. Available subtargets for project '{}':\n{}",
            sub_id, project.id, subtarget_list
        ),
        Some(project.id.clone()),
        None,
    ))
}
