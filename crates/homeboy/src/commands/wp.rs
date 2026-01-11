use clap::Args;
use homeboy_core::config::{
    ConfigManager, ProjectConfiguration, ProjectRecord, ProjectTypeManager, SlugIdentifiable,
};
use homeboy_core::context::resolve_project_ssh;
use homeboy_core::ssh::{execute_local_command, CommandOutput};
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
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run(args: WpArgs) -> CmdResult<WpOutput> {
    run_with_loader_and_executor(
        args,
        ConfigManager::load_project_record,
        execute_local_command,
    )
}

fn run_with_loader_and_executor(
    args: WpArgs,
    project_loader: fn(&str) -> homeboy_core::Result<ProjectRecord>,
    local_executor: fn(&str) -> CommandOutput,
) -> CmdResult<WpOutput> {
    if args.args.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No command provided".to_string(),
        ));
    }

    let project = project_loader(&args.project_id)?;

    let type_def = ProjectTypeManager::resolve(&project.config.project_type);

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

    let (output, target_domain, command) = if args.local {
        let (target_domain, command) = build_command(&project, &cli_config, &args.args, true)?;
        let output = local_executor(&command);
        (output, Some(target_domain), command)
    } else {
        let (target_domain, command) = build_command(&project, &cli_config, &args.args, false)?;

        let ctx = resolve_project_ssh(&args.project_id)?;
        let output = ctx.client.execute(&command);
        (output, Some(target_domain), command)
    };

    Ok((
        WpOutput {
            project_id: args.project_id,
            local: args.local,
            args: args.args,
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
    cli_config: &homeboy_core::config::CliConfig,
    args: &[String],
    use_local_domain: bool,
) -> homeboy_core::Result<(String, String)> {
    let base_path = if use_local_domain {
        if !project.config.local_environment.is_configured() {
            return Err(homeboy_core::Error::Other(
                "Local environment not configured for project".to_string(),
            ));
        }
        project.config.local_environment.site_path.clone()
    } else {
        project
            .project
            .base_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                homeboy_core::Error::Config("Remote base path not configured".to_string())
            })?
    };

    let (target_domain, command_args) = resolve_subtarget(&project.project, args, use_local_domain);

    if command_args.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No command provided after subtarget".to_string(),
        ));
    }

    let cli_path = if use_local_domain {
        project
            .project
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
        .find(|t| t.slug_id().ok().as_deref() == Some(sub_id) || token::identifier_eq(&t.name, sub_id))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_project_loader(project_id: &str) -> homeboy_core::Result<ProjectRecord> {
        Ok(ProjectRecord {
            id: project_id.to_string(),
            config: ProjectConfiguration {
                name: "Sarai Chinwag".to_string(),
                domain: "example.com".to_string(),
                project_type: "wordpress".to_string(),
                server_id: Some("cloudways".to_string()),
                base_path: Some("/tmp".to_string()),
                table_prefix: Some("wp_".to_string()),
                remote_files: Default::default(),
                remote_logs: Default::default(),
                database: Default::default(),
                local_environment: homeboy_core::config::LocalEnvironmentConfig {
                    site_path: "/tmp".to_string(),
                    domain: "example.local".to_string(),
                    cli_path: None,
                },
                tools: Default::default(),
                api: Default::default(),
                changelog_next_section_label: None,
                changelog_next_section_aliases: None,
                sub_targets: vec![],
                shared_tables: vec![],
                component_ids: vec![],
                table_groupings: vec![],
                component_groupings: vec![],
                protected_table_patterns: vec![],
                unlocked_table_patterns: vec![],
            },
        })
    }

    fn fake_executor(_command: &str) -> CommandOutput {
        CommandOutput {
            stdout: "ok\n".to_string(),
            stderr: "".to_string(),
            success: true,
            exit_code: 0,
        }
    }

    #[test]
    fn wp_returns_executor_stdout_and_exit_code() {
        let args = WpArgs {
            project_id: "saraichinwag".to_string(),
            local: true,
            args: vec!["core".to_string(), "version".to_string()],
        };

        let (data, exit_code) =
            run_with_loader_and_executor(args, fake_project_loader, fake_executor).unwrap();

        assert_eq!(exit_code, 0);
        assert_eq!(data.exit_code, 0);
        assert_eq!(data.stdout, "ok\n");
        assert_eq!(data.stderr, "");
    }
}
