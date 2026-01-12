use homeboy_core::config::{ConfigManager, ProjectConfiguration, ProjectRecord, SlugIdentifiable};
use homeboy_core::context::resolve_project_ssh;
use homeboy_core::plugin::{find_plugin_by_tool, CliConfig, PluginManifest};
use homeboy_core::shell;
use homeboy_core::ssh::{execute_local_command, CommandOutput};
use homeboy_core::template::{render_map, TemplateVars};
use homeboy_core::token;
use serde::Serialize;
use std::collections::HashMap;

use super::CmdResult;

#[derive(Serialize)]
pub struct CliOutput {
    pub tool: String,
    pub plugin_id: String,
    pub project_id: String,
    pub local: bool,
    pub args: Vec<String>,
    pub target_domain: Option<String>,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run(
    tool: &str,
    project_id: &str,
    local: bool,
    args: Vec<String>,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<CliOutput> {
    run_with_loader_and_executor(
        tool,
        project_id,
        local,
        args,
        ConfigManager::load_project_record,
        execute_local_command,
    )
}

fn run_with_loader_and_executor(
    tool: &str,
    project_id: &str,
    local: bool,
    args: Vec<String>,
    project_loader: fn(&str) -> homeboy_core::Result<ProjectRecord>,
    local_executor: fn(&str) -> CommandOutput,
) -> CmdResult<CliOutput> {
    if args.is_empty() {
        return Err(homeboy_core::Error::other(
            "No command provided".to_string(),
        ));
    }

    let plugin = find_plugin_by_tool(tool).ok_or_else(|| {
        homeboy_core::Error::other(format!("No plugin provides tool '{}'", tool))
    })?;

    let cli_config = plugin.cli.as_ref().ok_or_else(|| {
        homeboy_core::Error::other(format!(
            "Plugin '{}' does not have CLI configuration",
            plugin.id
        ))
    })?;

    let project = project_loader(project_id)?;

    if !project.config.has_plugin(&plugin.id) {
        return Err(homeboy_core::Error::other(format!(
            "Project '{}' does not have the '{}' plugin enabled",
            project_id, plugin.id
        )));
    }

    let (output, target_domain, command) = if local {
        let (target_domain, command) =
            build_command(&project, &plugin, cli_config, &args, true)?;
        let output = local_executor(&command);
        (output, Some(target_domain), command)
    } else {
        let (target_domain, command) =
            build_command(&project, &plugin, cli_config, &args, false)?;

        let ctx = resolve_project_ssh(project_id)?;
        let output = ctx.client.execute(&command);
        (output, Some(target_domain), command)
    };

    Ok((
        CliOutput {
            tool: tool.to_string(),
            plugin_id: plugin.id,
            project_id: project_id.to_string(),
            local,
            args,
            target_domain,
            command,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        },
        output.exit_code,
    ))
}

fn build_command(
    project: &ProjectRecord,
    plugin: &PluginManifest,
    cli_config: &CliConfig,
    args: &[String],
    use_local: bool,
) -> homeboy_core::Result<(String, String)> {
    let base_path = if use_local {
        if !project.config.local_environment.is_configured() {
            return Err(homeboy_core::Error::other(
                "Local environment not configured for project".to_string(),
            ));
        }
        project.config.local_environment.site_path.clone()
    } else {
        project
            .config
            .base_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                homeboy_core::Error::config("Remote base path not configured".to_string())
            })?
    };

    let (target_domain, command_args) = resolve_subtarget(&project.config, args, use_local);

    let command_args = inject_plugin_args(&project.config, plugin, command_args);

    if command_args.is_empty() {
        return Err(homeboy_core::Error::other(
            "No command provided after subtarget".to_string(),
        ));
    }

    let cli_path = if use_local {
        project
            .config
            .local_environment
            .cli_path
            .clone()
            .or_else(|| cli_config.default_cli_path.clone())
            .unwrap_or_else(|| cli_config.tool.clone())
    } else {
        cli_config
            .default_cli_path
            .clone()
            .unwrap_or_else(|| cli_config.tool.clone())
    };

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

fn inject_plugin_args(
    project: &ProjectConfiguration,
    plugin: &PluginManifest,
    args: Vec<String>,
) -> Vec<String> {
    let cli_config = match &plugin.cli {
        Some(cli) => cli,
        None => return args,
    };

    let mut injected_args = Vec::new();

    for injection in &cli_config.arg_injections {
        if let Some(value) = project.get_plugin_setting_str(&plugin.id, &injection.setting_key) {
            let arg = injection.arg_template.replace("{{value}}", value);
            injected_args.push(arg);
        }
    }

    injected_args.extend(args);
    injected_args
}

fn resolve_subtarget(
    project: &ProjectConfiguration,
    args: &[String],
    use_local: bool,
) -> (String, Vec<String>) {
    let default_domain = if use_local {
        if project.local_environment.domain.is_empty() {
            "localhost".to_string()
        } else {
            project.local_environment.domain.clone()
        }
    } else {
        project.domain.clone()
    };

    if project.sub_targets.is_empty() {
        return (default_domain, args.to_vec());
    }

    let Some(sub_id) = args.first() else {
        return (default_domain, args.to_vec());
    };

    if let Some(subtarget) = project.sub_targets.iter().find(|t| {
        t.slug_id().ok().as_deref() == Some(sub_id) || token::identifier_eq(&t.name, sub_id)
    }) {
        let domain = if use_local {
            let base_domain = if project.local_environment.domain.is_empty() {
                "localhost"
            } else {
                &project.local_environment.domain
            };
            if subtarget.is_default {
                base_domain.to_string()
            } else {
                let slug = subtarget.slug_id().unwrap_or_default();
                format!("{}/{}", base_domain, slug)
            }
        } else {
            subtarget.domain.clone()
        };
        return (domain, args[1..].to_vec());
    }

    (default_domain, args.to_vec())
}
