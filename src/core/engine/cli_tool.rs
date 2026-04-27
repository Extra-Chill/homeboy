use serde::Serialize;
use std::collections::HashMap;

use crate::component::{self, Component};
use crate::context::resolve_project_ssh;
use crate::engine::executor;
use crate::engine::shell;
use crate::engine::template::{render_map, TemplateVars};
use crate::engine::text;
use crate::error::ErrorCode;
use crate::extension::{find_extension_by_tool, CliAutoFlag, CliConfig};
use crate::project::{self, Project};
use crate::server;
use crate::server::{execute_local_command, CommandOutput};
use crate::{Error, Result};

#[derive(Serialize, Clone)]

pub struct CliToolResult {
    pub tool: String,
    pub extension_id: String,
    pub identifier: String,
    pub target_domain: Option<String>,
    pub executed_command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn run(tool: &str, identifier: &str, args: &[String]) -> Result<CliToolResult> {
    // Normalize args: split quoted strings containing spaces.
    // This ensures both syntaxes work identically:
    //   homeboy wp extra-chill:events datamachine pipelines list
    //   homeboy wp extra-chill:events "datamachine pipelines list"
    let args = shell::normalize_args(args);

    // Parse project:subtarget syntax
    let (project_id, embedded_subtarget) = crate::engine::text::split_identifier(identifier);

    // Build args with embedded subtarget prepended if present
    let full_args: Vec<String> = match embedded_subtarget {
        Some(sub) => std::iter::once(sub.to_string())
            .chain(args.iter().cloned())
            .collect(),
        None => args.to_vec(),
    };

    // Try component first (uses original identifier for component lookup)
    if let Some(result) = try_run_for_component(tool, identifier, &args) {
        return result;
    }

    run_for_project(tool, project_id, &full_args)
}

fn try_run_for_component(
    tool: &str,
    identifier: &str,
    args: &[String],
) -> Option<Result<CliToolResult>> {
    match component::resolve_effective(Some(identifier), None, None) {
        Ok(component) => {
            let extension = find_extension_by_tool(tool)?;
            let cli_config = extension.cli.as_ref()?;

            let command = build_component_command(&component, cli_config, &extension, args);
            let output = execute_local_command(&command);

            Some(Ok(CliToolResult {
                tool: tool.to_string(),
                extension_id: extension.id.clone(),
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
    extension: &crate::extension::ExtensionManifest,
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

    if let Some(ref path) = extension.extension_path {
        variables.insert(TemplateVars::EXTENSION_PATH.to_string(), path.clone());
    }

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
        return Err(Error::validation_missing_argument(vec![
            "command".to_string()
        ]));
    }

    let extension = find_extension_by_tool(tool).ok_or_else(|| {
        Error::validation_invalid_argument(
            "tool",
            format!("No extension provides tool '{}'", tool),
            Some(tool.to_string()),
            None,
        )
    })?;

    let cli_config = extension.cli.as_ref().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have CLI configuration",
            extension.id
        ))
    })?;

    let project = project_loader(project_id)?;

    let (target_domain, command_args) = resolve_subtarget(&project, args)?;

    if command_args.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "command".to_string()
        ]));
    }

    // Try direct execution first (bypasses shell escaping issues)
    let (output, executed_command) = if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        let result = executor::execute_for_project_direct(
            &project,
            cli_config,
            &extension.id,
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
                    build_project_command(&project, cli_config, &extension.id, args)?;
                (local_executor(&rendered_cmd), rendered_cmd)
            }
        }
    } else {
        let ctx = resolve_project_ssh(project_id)?;
        let (_, rendered_cmd) = build_project_command(&project, cli_config, &extension.id, args)?;
        let cmd_output = ctx.client.execute(&rendered_cmd);
        (cmd_output, rendered_cmd)
    };

    Ok(CliToolResult {
        tool: tool.to_string(),
        extension_id: extension.id,
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
    extension_id: &str,
    args: &[String],
) -> Result<(String, String)> {
    let base_path = project
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config("Base path not configured".to_string()))?;

    let (target_domain, command_args) = resolve_subtarget(project, args)?;

    if command_args.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "command".to_string()
        ]));
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

    // Add extension_path so {{extension_path}} resolves in command templates
    let extension_dir = crate::extension::extension_path(extension_id);
    if extension_dir.exists() {
        variables.insert(
            TemplateVars::EXTENSION_PATH.to_string(),
            extension_dir.to_string_lossy().to_string(),
        );
    }

    let mut rendered = render_map(&cli_config.command_template, &variables);

    // Append settings-based flags from extension config
    if let Some(extension_config) = project
        .extensions
        .as_ref()
        .and_then(|m| m.get(extension_id))
    {
        for (setting_key, flag_template) in &cli_config.settings_flags {
            if let Some(flag) = extension_config
                .settings
                .get(setting_key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|value_str| flag_template.replace("{{value}}", &shell::quote_arg(value_str)))
            {
                rendered.push(' ');
                rendered.push_str(&flag);
            }
        }
    }

    for flag in matching_auto_flags(
        extension_id,
        cli_config,
        project_server_user(project).as_deref(),
    ) {
        rendered.push(' ');
        rendered.push_str(flag);
    }

    Ok((target_domain, rendered))
}

fn project_server_user(project: &Project) -> Option<String> {
    let server_id = project.server_id.as_ref().filter(|s| !s.is_empty())?;
    server::load(server_id).ok().map(|svr| svr.user)
}

fn matching_auto_flags<'a>(
    extension_id: &str,
    cli_config: &'a CliConfig,
    server_user: Option<&str>,
) -> Vec<&'a str> {
    if cli_config.auto_flags.is_empty() {
        return legacy_default_auto_flags(extension_id, server_user);
    }

    cli_config
        .auto_flags
        .iter()
        .filter(|auto_flag| auto_flag_matches(auto_flag, server_user))
        .map(|auto_flag| auto_flag.flag.as_str())
        .collect()
}

fn legacy_default_auto_flags(extension_id: &str, server_user: Option<&str>) -> Vec<&'static str> {
    if extension_id == "wordpress" && server_user == Some("root") {
        vec!["--allow-root"]
    } else {
        Vec::new()
    }
}

fn auto_flag_matches(auto_flag: &CliAutoFlag, server_user: Option<&str>) -> bool {
    if let Some(expected_user) = auto_flag.when.server_user.as_deref() {
        return server_user == Some(expected_user);
    }

    true
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
                "This project has subtargets configured. You must specify which subtarget to use.\n\n\
                 Available subtargets for project '{}':\n{}\n\n\
                 Syntax: homeboy <tool> {}:<subtarget> <command>...\n\
                     OR  homeboy <tool> {} <subtarget> <command>...\n\n\
                 Commands can be quoted or unquoted:\n  \
                   homeboy wp {}:events post list\n  \
                   homeboy wp {}:events \"post list\"",
                project.id, subtarget_list, project.id, project.id, project.id, project.id
            ),
            Some(project.id.clone()),
            None,
        ));
    };

    if let Some(subtarget) = project.sub_targets.iter().find(|t| {
        project::slugify_id(&t.name).ok().as_deref() == Some(sub_id)
            || text::identifier_eq(&t.name, sub_id)
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
            "Subtarget '{}' not found. Available subtargets for project '{}':\n{}\n\n\
             Syntax: homeboy <tool> {}:<subtarget> <command>...\n\
                 OR  homeboy <tool> {} <subtarget> <command>...\n\n\
             Commands can be quoted or unquoted:\n  \
               homeboy wp {}:events post list\n  \
               homeboy wp {}:events \"post list\"",
            sub_id, project.id, subtarget_list, project.id, project.id, project.id, project.id
        ),
        Some(project.id.clone()),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::{CliAutoFlagCondition, CliHelpConfig};

    fn cli_config(auto_flags: Vec<CliAutoFlag>) -> CliConfig {
        CliConfig {
            tool: "wp".to_string(),
            display_name: "WP-CLI".to_string(),
            command_template: "{{cliPath}} {{args}}".to_string(),
            default_cli_path: Some("wp".to_string()),
            working_dir_template: None,
            settings_flags: HashMap::new(),
            auto_flags,
            help: None::<CliHelpConfig>,
        }
    }

    #[test]
    fn auto_flags_match_server_user_conditions() {
        let config = cli_config(vec![
            CliAutoFlag {
                when: CliAutoFlagCondition {
                    server_user: Some("root".to_string()),
                },
                flag: "--allow-root".to_string(),
            },
            CliAutoFlag {
                when: CliAutoFlagCondition {
                    server_user: Some("deploy".to_string()),
                },
                flag: "--as-deploy".to_string(),
            },
        ]);

        assert_eq!(
            matching_auto_flags("wordpress", &config, Some("root")),
            vec!["--allow-root"]
        );
        assert_eq!(
            matching_auto_flags("wordpress", &config, Some("deploy")),
            vec!["--as-deploy"]
        );
        assert!(matching_auto_flags("wordpress", &config, Some("www-data")).is_empty());
        assert!(matching_auto_flags("wordpress", &config, None).is_empty());
    }

    #[test]
    fn auto_flags_with_empty_conditions_always_apply() {
        let config = cli_config(vec![CliAutoFlag {
            when: CliAutoFlagCondition::default(),
            flag: "--global-flag".to_string(),
        }]);

        assert_eq!(
            matching_auto_flags("wordpress", &config, Some("root")),
            vec!["--global-flag"]
        );
        assert_eq!(
            matching_auto_flags("wordpress", &config, None),
            vec!["--global-flag"]
        );
    }

    #[test]
    fn legacy_wordpress_auto_flag_still_applies_without_manifest_flags() {
        let config = cli_config(Vec::new());

        assert_eq!(
            matching_auto_flags("wordpress", &config, Some("root")),
            vec!["--allow-root"]
        );
        assert!(matching_auto_flags("wordpress", &config, Some("deploy")).is_empty());
        assert!(matching_auto_flags("custom", &config, Some("root")).is_empty());
    }

    #[test]
    fn cli_config_deserializes_manifest_auto_flags() {
        let config: CliConfig = serde_json::from_str(
            r#"{
                "tool": "wp",
                "display_name": "WP-CLI",
                "command_template": "{{cliPath}} {{args}}",
                "auto_flags": [
                    { "when": { "server_user": "root" }, "flag": "--allow-root" }
                ]
            }"#,
        )
        .expect("parse cli config");

        assert_eq!(config.auto_flags.len(), 1);
        assert_eq!(
            config.auto_flags[0].when.server_user.as_deref(),
            Some("root")
        );
        assert_eq!(config.auto_flags[0].flag, "--allow-root");
    }
}
