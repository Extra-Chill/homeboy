use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

use crate::component::{self, Component};
use crate::config::{is_json_input, parse_bulk_ids};
use crate::error::{Error, Result};
use crate::module;
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::paths;
use crate::permissions;
use crate::shell;
use crate::ssh::execute_local_command_in_dir;

// === Build Command Resolution ===

#[derive(Debug, Clone)]
pub enum ResolvedBuildCommand {
    ComponentDefined(String),
    ModuleProvided {
        command: String,
        source: String,
    },
    LocalScript {
        command: String,
        script_name: String,
    },
}

impl ResolvedBuildCommand {
    pub fn command(&self) -> &str {
        match self {
            ResolvedBuildCommand::ComponentDefined(cmd) => cmd,
            ResolvedBuildCommand::ModuleProvided { command, .. } => command,
            ResolvedBuildCommand::LocalScript { command, .. } => command,
        }
    }
}

/// Resolve build command for a component using the following priority:
/// 1. Explicit component.build_command (always wins)
/// 2. Module's bundled script (module.build.module_script)
/// 3. Local script matching module's script_names pattern
pub fn resolve_build_command(component: &Component) -> Result<ResolvedBuildCommand> {
    // 1. Explicit component override takes precedence
    if let Some(cmd) = &component.build_command {
        return Ok(ResolvedBuildCommand::ComponentDefined(cmd.clone()));
    }

    // 2. Check module for bundled script or local script patterns
    if let Some(modules) = &component.modules {
        for module_id in modules.keys() {
            if let Ok(module) = module::load_module(module_id) {
                if let Some(build) = &module.build {
                    // Check for module's bundled script
                    if let Some(module_script) = &build.module_script {
                        if let Ok(module_dir) = paths::module(module_id) {
                            let script_path = module_dir.join(module_script);
                            if script_path.exists() {
                                let quoted_path = shell::quote_path(&script_path.to_string_lossy());
                                let command = build
                                    .command_template
                                    .as_ref()
                                    .map(|t| t.replace("{{script}}", &quoted_path))
                                    .unwrap_or_else(|| format!("sh {}", quoted_path));
                                return Ok(ResolvedBuildCommand::ModuleProvided {
                                    command,
                                    source: format!("{}:{}", module_id, module_script),
                                });
                            }
                        }
                    }

                    // Check for local script matching module's script_names
                    let local_path = PathBuf::from(&component.local_path);
                    for script_name in &build.script_names {
                        let local_script = local_path.join(script_name);
                        if local_script.exists() {
                            let command = build
                                .command_template
                                .as_ref()
                                .map(|t| t.replace("{{script}}", script_name))
                                .unwrap_or_else(|| format!("sh {}", script_name));
                            return Ok(ResolvedBuildCommand::LocalScript {
                                command,
                                script_name: script_name.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    Err(Error::other(format!(
        "Component '{}' has no build configuration. Either:\n\
         - Configure buildCommand: homeboy component set {} --json '{{\"buildCommand\": \"<command>\"}}'\n\
         - Link a module with build support: homeboy component set {} --json '{{\"modules\": {{\"wordpress\": {{}}}}}}'",
        component.id, component.id, component.id
    )))
}

// === Public API ===

#[derive(Debug, Clone, Serialize)]

pub struct BuildOutput {
    pub command: String,
    pub component_id: String,
    pub build_command: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum BuildResult {
    Single(BuildOutput),
    Bulk(BulkResult<BuildOutput>),
}

/// Run build for one or more components.
///
/// Accepts either:
/// - A single component ID: "extrachill-api"
/// - A JSON spec: {"componentIds": ["api", "users"]}
pub fn run(input: &str) -> Result<(BuildResult, i32)> {
    if is_json_input(input) {
        run_bulk(input)
    } else {
        run_single(input)
    }
}

/// Build a component for deploy context.
/// Returns (exit_code, error_message) - None error means success.
///
/// Shell execution is required for build commands by design:
/// - Build commands execute shell scripts (bash, sh, npm, composer, etc.)
/// - Scripts use shell features (pipes, redirects, environment variables)
/// - Examples: "bash {{script}}", "sh build.sh", "npm run build"
/// - Build processes often require chaining with &&, ||, ;
/// - Direct execution cannot handle shell scripts or shell features
/// See executor.rs for detailed execution strategy decision tree
pub fn build_component(component: &component::Component) -> (Option<i32>, Option<String>) {
    let resolved = match resolve_build_command(component) {
        Ok(r) => r,
        Err(e) => return (Some(1), Some(e.to_string())),
    };

    let build_cmd = resolved.command().to_string();

    // Fix local permissions before build to ensure zip has correct permissions
    permissions::fix_local_permissions(&component.local_path);

    let output = execute_local_command_in_dir(&build_cmd, Some(&component.local_path), None);

    if output.success {
        (Some(output.exit_code), None)
    } else {
        (
            Some(output.exit_code),
            Some(format!(
                "Build failed for '{}'. Fix build errors before deploying.",
                component.id
            )),
        )
    }
}

// === Internal implementation ===

fn run_single(component_id: &str) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build(component_id)?;
    Ok((BuildResult::Single(output), exit_code))
}

fn run_bulk(json_spec: &str) -> Result<(BuildResult, i32)> {
    let input = parse_bulk_ids(json_spec)?;

    let mut results = Vec::with_capacity(input.component_ids.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in &input.component_ids {
        match execute_build(id) {
            Ok((output, _)) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        BuildResult::Bulk(BulkResult {
            action: "build".to_string(),
            results,
            summary: BulkSummary {
                total: succeeded + failed,
                succeeded,
                failed,
            },
        }),
        exit_code,
    ))
}

fn execute_build(component_id: &str) -> Result<(BuildOutput, i32)> {
    let comp = component::load(component_id)?;
    let resolved = resolve_build_command(&comp)?;
    let build_cmd = resolved.command().to_string();

    // Run pre-build script if module provides one
    if let Some((exit_code, stderr)) = run_pre_build_scripts(&comp)? {
        if exit_code != 0 {
            return Ok((
                BuildOutput {
                    command: "build.run".to_string(),
                    component_id: component_id.to_string(),
                    build_command: build_cmd,
                    stdout: String::new(),
                    stderr,
                    success: false,
                },
                exit_code,
            ));
        }
    }

    // Fix local permissions before build to ensure zip has correct permissions
    permissions::fix_local_permissions(&comp.local_path);

    let output = execute_local_command_in_dir(&build_cmd, Some(&comp.local_path), None);

    Ok((
        BuildOutput {
            command: "build.run".to_string(),
            component_id: component_id.to_string(),
            build_command: build_cmd,
            stdout: output.stdout,
            stderr: output.stderr,
            success: output.success,
        },
        output.exit_code,
    ))
}

/// Run pre-build scripts from all configured modules.
/// Returns Some((exit_code, stderr)) if any script fails, None if all pass or no scripts.
fn run_pre_build_scripts(comp: &Component) -> Result<Option<(i32, String)>> {
    let modules = match &comp.modules {
        Some(m) => m,
        None => return Ok(None),
    };

    for module_id in modules.keys() {
        let module = match module::load_module(module_id) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let build_config = match &module.build {
            Some(b) => b,
            None => continue,
        };

        let pre_build_script = match &build_config.pre_build_script {
            Some(s) => s,
            None => continue,
        };

        let module_path = paths::module(module_id)?;
        let script_path = module_path.join(pre_build_script);

        if !script_path.exists() {
            continue;
        }

        let output = Command::new(&script_path)
            .env("HOMEBOY_MODULE_PATH", module_path.to_string_lossy().to_string())
            .env("HOMEBOY_COMPONENT_PATH", &comp.local_path)
            .env("HOMEBOY_PLUGIN_PATH", &comp.local_path)
            .output()
            .map_err(|e| Error::internal_io(
                format!("Failed to execute pre-build script: {}", e),
                Some(format!("Script: {}", script_path.display())),
            ))?;

        let exit_code = output.status.code().unwrap_or(1);
        if exit_code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let combined = if stderr.is_empty() { stdout } else { stderr };
            return Ok(Some((exit_code, combined)));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_json_input_detects_json() {
        assert!(is_json_input(r#"{"componentIds": ["a"]}"#));
        assert!(is_json_input(r#"  {"componentIds": ["a"]}"#));
        assert!(!is_json_input("extrachill-api"));
        assert!(!is_json_input("some-component-id"));
    }
}
