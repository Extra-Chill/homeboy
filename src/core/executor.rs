// Unified command execution - routes to local or SSH based on project config
//
// ## Execution Strategy Decision Tree
//
// ### Direct Execution (Preferred for CLI tools)
// Use when:
// - Simple command structure (program + args)
// - No shell operators (&&, ||, |, >, >>, <, <<, &, `, $(), EOF, ;)
// - working_dir_template available for directory changes
// - CLI tool commands with clean template syntax: "{{cliPath}} {{args}}"
//
// Benefits:
// - No shell overhead (faster)
// - No shell escaping bugs (safer, simpler)
// - Direct argument passing (no quoting complexity)
//
// ### Shell Execution (Required for complex operations)
// Use when:
// - Pipes and redirects (|, >, >>, <, <<)
// - Command chaining (&&, ||, ;)
// - Variable assignment and subshells
// - Bash script execution
// - Log operations with pipes (tail -f logs | grep error)
// - Database queries with complex SQL strings
// - Deploy install commands with subshells and conditional logic
// - Discovery commands with fallback operators (||)
//
// Module runtime and build commands use shell execution by design:
// - Runtime commands execute bash scripts (set -euo pipefail, arrays, jq)
// - Build commands use shell scripts (rsync, composer, npm, etc.)
// - These scripts require shell features and cannot use direct execution
//
// ### Routing Logic
// execute_for_project() -> routes to local or SSH based on server_id
// execute_for_project_interactive() -> routes local/SSH with inherited stdio
// execute_for_project_direct() -> tries direct first, falls back to shell

use crate::context::resolve_project_ssh;
use crate::error::{Error, Result};
use crate::module::CliConfig;
use crate::project::Project;
use crate::shell;
use crate::ssh::{execute_local_command, execute_local_command_interactive, CommandOutput};
use std::process::Command;

/// Execute a command for a project - routes to local or SSH based on server_id config.
///
/// When `server_id` is not configured: executes command locally via shell
/// When `server_id` is configured: executes command via SSH to that server
///
/// This is the same pattern used by cli_tool.rs for module CLI commands.
pub fn execute_for_project(project: &Project, command: &str) -> Result<CommandOutput> {
    if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        // Local execution
        Ok(execute_local_command(command))
    } else {
        // SSH execution
        let ctx = resolve_project_ssh(&project.id)?;
        Ok(ctx.client.execute(command))
    }
}

/// Execute an interactive command for a project (e.g., `tail -f`).
/// Returns exit code.
///
/// When `server_id` is not configured: executes locally with inherited stdio
/// When `server_id` is configured: executes via SSH interactive session
pub fn execute_for_project_interactive(project: &Project, command: &str) -> Result<i32> {
    if project.server_id.as_ref().is_none_or(|s| s.is_empty()) {
        // Local interactive execution
        Ok(execute_local_command_interactive(command, None, None))
    } else {
        // SSH interactive execution
        let ctx = resolve_project_ssh(&project.id)?;
        Ok(ctx.client.execute_interactive(Some(command)))
    }
}

/// Execute a CLI tool command for a project using direct execution (bypass shell).
///
/// Direct execution is the default for CLI tools when the template doesn't require
/// shell features (&&, |, cd, etc.).
///
/// Falls back to shell execution if direct execution isn't possible.
pub fn execute_for_project_direct(
    project: &Project,
    cli_config: &CliConfig,
    module_id: &str,
    args: &[String],
    target_domain: &str,
) -> Result<CommandOutput> {
    let base_path = project
        .base_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::config("Base path not configured".to_string()))?;

    // Try direct execution first
    if let Ok(output) = try_execute_direct(base_path.clone(), cli_config, project, module_id, args, target_domain) {
        return Ok(output);
    }

    // Fallback to shell execution
    let command = build_shell_command(&base_path, cli_config, args, target_domain)?;
    execute_for_project(project, &command)
}

fn try_execute_direct(
    base_path: String,
    cli_config: &CliConfig,
    project: &Project,
    module_id: &str,
    args: &[String],
    target_domain: &str,
) -> Result<CommandOutput> {
    // Check if template requires shell features
    if requires_shell_execution(&cli_config.command_template) {
        return Err(Error::other(
            "Template requires shell execution".to_string(),
        ));
    }

    // Parse the template
    let parsed = parse_direct_template(base_path, cli_config, project, module_id, args, target_domain)?;

    // Execute directly (no shell)
    let mut cmd = Command::new(&parsed.program);

    if let Some(dir) = &parsed.working_dir {
        cmd.current_dir(dir);
    }

    if !parsed.args.is_empty() {
        cmd.args(&parsed.args);
    }

    match cmd.output() {
        Ok(out) => Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            success: out.status.success(),
            exit_code: out.status.code().unwrap_or(-1),
        }),
        Err(e) => Ok(CommandOutput {
            stdout: String::new(),
            stderr: format!("Command error: {}", e),
            success: false,
            exit_code: -1,
        }),
    }
}

fn requires_shell_execution(template: &str) -> bool {
    let shell_operators = [
        "&&", "||", ";", "|", ">", ">>", "<", "<<", "&", "`", "$(", "EOF",
    ];

    for op in &shell_operators {
        if template.contains(op) {
            return true;
        }
    }

    false
}

struct ParsedDirectCommand {
    program: String,
    args: Vec<String>,
    working_dir: Option<String>,
}

fn parse_direct_template(
    base_path: String,
    cli_config: &CliConfig,
    project: &Project,
    module_id: &str,
    args: &[String],
    target_domain: &str,
) -> Result<ParsedDirectCommand> {
    let mut template = cli_config.command_template.clone();

    // Expand {{cliPath}}
    let cli_path = cli_config
        .default_cli_path
        .clone()
        .unwrap_or_else(|| cli_config.tool.clone());
    template = template.replace("{{cliPath}}", &cli_path);

    // Expand {{domain}} in --url={{domain}}
    template = template.replace("--url={{domain}}", &format!("--url={}", target_domain));

    // Handle {{args}} - expand to individual args
    let mut command_parts: Vec<String> =
        template.split_whitespace().map(|s| s.to_string()).collect();

    // Find and replace {{args}} placeholder with actual args
    let mut final_args: Vec<String> =
        if let Some(pos) = command_parts.iter().position(|p| p == "{{args}}") {
            command_parts.remove(pos);
            let mut result = Vec::new();
            result.extend(command_parts);
            result.extend(args.iter().cloned());
            result
        } else {
            command_parts
        };

    // Apply settings_flags from project module config
    if !cli_config.settings_flags.is_empty() {
        if let Some(modules) = &project.modules {
            if let Some(module_config) = modules.get(module_id) {
                for (setting_key, flag_template) in &cli_config.settings_flags {
                    if let Some(value) = module_config.settings.get(setting_key) {
                        if let Some(value_str) = value.as_str() {
                            if !value_str.is_empty() {
                                let flag = flag_template.replace("{{value}}", value_str);
                                final_args.push(flag);
                            }
                        }
                    }
                }
            }
        }
    }

    // Extract working directory from working_dir_template
    let working_dir = cli_config.working_dir_template.as_ref().and_then(|t| {
        if t == "{{sitePath}}" {
            Some(base_path)
        } else {
            None
        }
    });

    let program = final_args
        .first()
        .cloned()
        .unwrap_or_else(|| cli_path.clone());

    let parsed_args = if final_args.len() > 1 {
        final_args[1..].to_vec()
    } else {
        vec![]
    };

    Ok(ParsedDirectCommand {
        program,
        args: parsed_args,
        working_dir,
    })
}

fn build_shell_command(
    base_path: &str,
    cli_config: &CliConfig,
    args: &[String],
    target_domain: &str,
) -> Result<String> {
    let mut template = cli_config.command_template.clone();

    // Expand {{cliPath}}
    let cli_path = cli_config
        .default_cli_path
        .clone()
        .unwrap_or_else(|| cli_config.tool.clone());
    template = template.replace("{{cliPath}}", &cli_path);

    // Expand {{sitePath}}
    template = template.replace("{{sitePath}}", base_path);

    // Expand {{domain}}
    template = template.replace("{{domain}}", target_domain);

    // Quote and join args
    let quoted_args = shell::quote_args(args);
    template = template.replace("{{args}}", &quoted_args);

    Ok(template)
}
