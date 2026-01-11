use clap::Args;
use homeboy_core::config::{ConfigManager, ProjectConfiguration, ProjectTypeManager};
use homeboy_core::ssh::{execute_local_command, SshClient};
use homeboy_core::template::{render_map, TemplateVars};
use homeboy_core::token;
use serde::Serialize;
use std::collections::HashMap;

use super::CmdResult;

#[derive(Args)]
pub struct WpArgs {
    /// Project ID
    pub project_id: String,

    /// Execute locally instead of on remote server
    #[arg(long)]
    pub local: bool,

    /// WP-CLI command and arguments (first arg may be a subtarget)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct WpOutput {
    pub project_id: String,
    pub local: bool,
    pub args: Vec<String>,
    pub target_domain: Option<String>,
    pub command: String,
}

pub fn run(args: WpArgs) -> CmdResult<WpOutput> {
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

    if cli_config.tool != "wp" {
        return Err(homeboy_core::Error::Other(format!(
            "Project '{}' is a {} project (uses '{}', not 'wp')",
            args.project_id, type_def.display_name, cli_config.tool
        )));
    }

    let (exit_code, target_domain, command) = if args.local {
        let (target_domain, command) = build_command(&project, &cli_config, &args.args, true)?;
        let output = execute_local_command(&command);
        (output.exit_code, Some(target_domain), command)
    } else {
        let (target_domain, command) = build_command(&project, &cli_config, &args.args, false)?;

        let server_id = project.server_id.as_ref().ok_or_else(|| {
            homeboy_core::Error::Other("Server not configured for project".to_string())
        })?;
        let server = ConfigManager::load_server(server_id)?;
        let client = SshClient::from_server(&server, server_id)?;
        let output = client.execute(&command);
        (output.exit_code, Some(target_domain), command)
    };

    Ok((
        WpOutput {
            project_id: args.project_id,
            local: args.local,
            args: args.args,
            target_domain,
            command,
        },
        exit_code,
    ))
}

fn build_command(
    project: &ProjectConfiguration,
    cli_config: &homeboy_core::config::CliConfig,
    args: &[String],
    use_local_domain: bool,
) -> homeboy_core::Result<(String, String)> {
    let base_path = if use_local_domain {
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

    let (target_domain, command_args) = resolve_subtarget(project, args, use_local_domain);

    if command_args.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No command provided after subtarget".to_string(),
        ));
    }

    let cli_path = if use_local_domain {
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
    variables.insert(TemplateVars::DOMAIN.to_string(), target_domain.clone());
    variables.insert(TemplateVars::ARGS.to_string(), command_args.join(" "));
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
    use_local_domain: bool,
) -> (String, Vec<String>) {
    let default_domain = if use_local_domain {
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

    if let Some(subtarget) = project
        .sub_targets
        .iter()
        .find(|t| token::identifier_eq(&t.id, sub_id) || token::identifier_eq(&t.name, sub_id))
    {
        let domain = if use_local_domain {
            let base_domain = if project.local_environment.domain.is_empty() {
                "localhost"
            } else {
                &project.local_environment.domain
            };
            if subtarget.is_default {
                base_domain.to_string()
            } else {
                format!("{}/{}", base_domain, subtarget.id)
            }
        } else {
            subtarget.domain.clone()
        };
        return (domain, args[1..].to_vec());
    }

    (default_domain, args.to_vec())
}
