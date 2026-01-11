use clap::Args;
use homeboy_core::config::{ConfigManager, ProjectConfiguration, ProjectTypeManager};
use homeboy_core::ssh::{execute_local_command, SshClient};
use homeboy_core::template::{render_map, TemplateVars};
use serde::Serialize;
use std::collections::HashMap;

use super::CmdResult;

#[derive(Args)]
pub struct Pm2Args {
    /// Project ID
    pub project_id: String,

    /// Execute locally instead of on remote server
    #[arg(long)]
    pub local: bool,

    /// PM2 command and arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct Pm2Output {
    pub project_id: String,
    pub local: bool,
    pub args: Vec<String>,
    pub command: String,
}

pub fn run(args: Pm2Args) -> CmdResult<Pm2Output> {
    if args.args.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No command provided".to_string(),
        ));
    }

    let project = ConfigManager::load_project(&args.project_id)?;

    let type_def = ProjectTypeManager::resolve(&project.project_type);

    let cli_config = type_def.cli.ok_or_else(|| {
        homeboy_core::Error::Other(format!(
            "Project type '{}' does not support CLI",
            type_def.display_name
        ))
    })?;

    if cli_config.tool != "pm2" {
        return Err(homeboy_core::Error::Other(format!(
            "Project '{}' is a {} project (uses '{}', not 'pm2')",
            args.project_id, type_def.display_name, cli_config.tool
        )));
    }

    let command = build_command(&project, &cli_config, &args.args, args.local)?;

    let exit_code = if args.local {
        let output = execute_local_command(&command);
        output.exit_code
    } else {
        let server_id = project.server_id.as_ref().ok_or_else(|| {
            homeboy_core::Error::Other("Server not configured for project".to_string())
        })?;
        let server = ConfigManager::load_server(server_id)?;
        let client = SshClient::from_server(&server, server_id)?;
        let output = client.execute(&command);
        output.exit_code
    };

    Ok((
        Pm2Output {
            project_id: args.project_id,
            local: args.local,
            args: args.args,
            command,
        },
        exit_code,
    ))
}

fn build_command(
    project: &ProjectConfiguration,
    cli_config: &homeboy_core::config::CliConfig,
    args: &[String],
    local: bool,
) -> homeboy_core::Result<String> {
    let site_path = if local {
        if !project.local_environment.is_configured() {
            return Err(homeboy_core::Error::Other(
                "Local environment not configured for project".to_string(),
            ));
        }
        project.local_environment.site_path.clone()
    } else {
        project
            .base_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                homeboy_core::Error::Other("Remote base path not configured".to_string())
            })?
    };

    let cli_path = if local {
        project
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
    variables.insert(
        TemplateVars::DOMAIN.to_string(),
        if local {
            project.local_environment.domain.clone()
        } else {
            project.domain.clone()
        },
    );
    variables.insert(TemplateVars::ARGS.to_string(), args.join(" "));
    variables.insert(TemplateVars::SITE_PATH.to_string(), site_path);
    variables.insert(TemplateVars::CLI_PATH.to_string(), cli_path);

    Ok(render_map(&cli_config.command_template, &variables))
}
