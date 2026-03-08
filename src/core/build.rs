use serde::Serialize;
use std::path::PathBuf;

use crate::component::{self, Component};
use crate::config::{is_json_input, parse_bulk_ids};
use crate::error::{Error, Result};
use crate::extension::{self, exec_context};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::paths;
use crate::permissions;
use crate::ssh::execute_local_command_in_dir;
use crate::utils::command::CapturedOutput;
use crate::utils::shell;

// === Build Command Resolution ===

#[derive(Debug, Clone)]
pub enum ResolvedBuildCommand {
    ComponentDefined(String),
    ExtensionProvided {
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
            ResolvedBuildCommand::ExtensionProvided { command, .. } => command,
            ResolvedBuildCommand::LocalScript { command, .. } => command,
        }
    }
}

/// Resolve build command for a component using the following priority:
/// 1. Extension's bundled script (extension.build.extension_script)
/// 2. Local script matching extension's script_names pattern
/// 3. Explicit component.build_command (fallback when no extension provides build)
///
/// Extensions own the build lifecycle for their component type.
/// If a component has both an extension with build support AND a build_command,
/// the extension wins and a warning is emitted.
pub(crate) fn resolve_build_command(component: &Component) -> Result<ResolvedBuildCommand> {
    // 1. Check extension for bundled script or local script patterns (takes priority)
    if let Some(extensions) = &component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(extension) = extension::load_extension(extension_id) {
                if let Some(build) = &extension.build {
                    // Check for extension's bundled script
                    let bundled = build
                        .extension_script
                        .as_ref()
                        .and_then(|extension_script| {
                            paths::extension(extension_id)
                                .ok()
                                .and_then(|extension_dir| {
                                    let script_path = extension_dir.join(extension_script);
                                    script_path.exists().then(|| {
                                        let quoted_path =
                                            shell::quote_path(&script_path.to_string_lossy());
                                        let command = build
                                            .command_template
                                            .as_ref()
                                            .map(|t| t.replace("{{script}}", &quoted_path))
                                            .unwrap_or_else(|| format!("sh {}", quoted_path));
                                        ResolvedBuildCommand::ExtensionProvided {
                                            command,
                                            source: format!(
                                                "{}:{}",
                                                extension_id, extension_script
                                            ),
                                        }
                                    })
                                })
                        });
                    if let Some(result) = bundled {
                        return Ok(result);
                    }

                    // Check for local script matching extension's script_names
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

    // 2. Fallback: explicit component.build_command (only when no extension provides build)
    if let Some(cmd) = &component.build_command {
        if extension::extension_provides_build(component) {
            // Extension provides build but no scripts found — warn about build_command being ignored
            log_status!(
                "build",
                "Warning: component '{}' has both an extension with build support AND a build_command. \
                 The extension owns the build lifecycle; build_command is ignored.",
                component.id
            );
            log_status!(
                "hint",
                "Remove build_command: homeboy component set {} --replace build_command",
                component.id
            );
        }
        return Ok(ResolvedBuildCommand::ComponentDefined(cmd.clone()));
    }

    // Check if any extension provides build (makes build_command optional)
    if extension::extension_provides_build(component) {
        // Extension provides build config but no matching scripts found
        Err(Error::validation_invalid_argument(
            "buildCommand",
            format!(
                "Component '{}' links a extension with build support, but no build script was found.\n\
                 Expected: extension's bundled script OR local script matching extension pattern.\n\
                 Check extension installation or add a local build.sh to the component directory.",
                component.id
            ),
            Some(component.id.clone()),
            None,
        ))
    } else {
        // No extensions with build support - explicit buildCommand required
        Err(Error::validation_invalid_argument(
            "buildCommand",
            format!("Component '{}' has no build configuration", component.id),
            Some(component.id.clone()),
            Some(vec![
                format!("Configure buildCommand: homeboy component set {} --json '{{\"buildCommand\": \"<command>\"}}'", component.id),
                format!("Link a extension with build support: homeboy component set {} --json '{{\"extensions\": {{\"wordpress\": {{}}}}}}'", component.id),
            ]),
        ))
    }
}

// === Public API ===

#[derive(Debug, Clone, Serialize)]
pub struct BuildOutput {
    pub command: String,
    pub component_id: String,
    pub build_command: String,
    #[serde(flatten)]
    pub output: CapturedOutput,
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
///
/// See executor.rs for detailed execution strategy decision tree
pub(crate) fn build_component(component: &component::Component) -> (Option<i32>, Option<String>) {
    // Validate local_path before attempting build
    let validated_path = match component::validate_local_path(component) {
        Ok(p) => p,
        Err(e) => return (Some(1), Some(format_path_validation_error(component, &e))),
    };

    let resolved = match resolve_build_command(component) {
        Ok(r) => r,
        Err(e) => return (Some(1), Some(e.to_string())),
    };

    let build_cmd = resolved.command().to_string();

    // Fix local permissions before build to ensure zip has correct permissions
    let local_path_str = validated_path.to_string_lossy().to_string();
    permissions::fix_local_permissions(&local_path_str);

    // Get extension path env vars for build command (matches pre-build script behavior)
    let env_vars = get_build_env_vars(component);
    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let output = execute_local_command_in_dir(
        &build_cmd,
        Some(&local_path_str),
        if env_refs.is_empty() {
            None
        } else {
            Some(&env_refs)
        },
    );

    if output.success {
        (Some(output.exit_code), None)
    } else {
        (
            Some(output.exit_code),
            Some(format_build_error(
                &component.id,
                &build_cmd,
                &local_path_str,
                output.exit_code,
                &output.stderr,
                &output.stdout,
            )),
        )
    }
}

/// Format a path validation error with build context.
fn format_path_validation_error(component: &component::Component, error: &Error) -> String {
    format!(
        "Build failed for component '{}':\n  {}\n\nHint: Update local_path with:\n  homeboy component set {} --local-path \"/path/to/component\"",
        component.id,
        error.message,
        component.id
    )
}

/// Format a build error message with context from stderr/stdout.
/// Only includes universal POSIX exit code hints - Homeboy is technology-agnostic.
fn format_build_error(
    component_id: &str,
    build_cmd: &str,
    working_dir: &str,
    exit_code: i32,
    stderr: &str,
    stdout: &str,
) -> String {
    // Get useful output (prefer stderr, fall back to stdout)
    let output_text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };

    // Get last 15 lines for context
    let tail: Vec<&str> = output_text.lines().rev().take(15).collect();
    let output_tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");

    // Translate universal POSIX exit codes only (no tool-specific hints)
    let hint = match exit_code {
        127 => "\nHint: Command not found. Check that the build command and its dependencies are installed and in PATH.",
        126 => "\nHint: Permission denied. Check file permissions on the build script.",
        _ => "",
    };

    let mut msg = format!(
        "Build failed for '{}' (exit code {}).\n  Command: {}\n  Working directory: {}",
        component_id, exit_code, build_cmd, working_dir
    );

    if !output_tail.is_empty() {
        msg.push_str("\n\n--- Build output (last 15 lines) ---\n");
        msg.push_str(&output_tail);
        msg.push_str("\n--- End of output ---");
    }

    if !hint.is_empty() {
        msg.push_str(hint);
    }

    msg
}

// === Internal implementation ===

fn run_single(component_id: &str) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build(component_id, None)?;
    Ok((BuildResult::Single(output), exit_code))
}

/// Build a single component with an overridden local_path.
///
/// Use this for workspace clones, temporary checkouts, or CI builds
/// where the source lives somewhere other than the configured `local_path`.
pub fn run_with_path(component_id: &str, path: &str) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build(component_id, Some(path))?;
    Ok((BuildResult::Single(output), exit_code))
}

fn run_bulk(json_spec: &str) -> Result<(BuildResult, i32)> {
    let input = parse_bulk_ids(json_spec)?;

    let mut results = Vec::with_capacity(input.component_ids.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in &input.component_ids {
        match execute_build(id, None) {
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

/// Build a pre-resolved component (supports both registered and discovered components).
pub fn run_component(component: &Component) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build_component(component)?;
    Ok((BuildResult::Single(output), exit_code))
}

fn execute_build(component_id: &str, path_override: Option<&str>) -> Result<(BuildOutput, i32)> {
    let mut comp = component::load(component_id)?;
    if let Some(path) = path_override {
        comp.local_path = path.to_string();
    }
    execute_build_component(&comp)
}

fn execute_build_component(comp: &Component) -> Result<(BuildOutput, i32)> {
    // Validate required extensions are installed before resolving build commands.
    // Without this, missing extensions cause vague "no build command" errors.
    extension::validate_required_extensions(comp)?;

    // Validate local_path before attempting build
    let validated_path = component::validate_local_path(comp)?;
    let local_path_str = validated_path.to_string_lossy().to_string();

    let resolved = resolve_build_command(comp)?;
    let build_cmd = resolved.command().to_string();

    // Run pre-build script if extension provides one
    if let Some((exit_code, stderr)) = run_pre_build_scripts(comp)? {
        if exit_code != 0 {
            return Ok((
                BuildOutput {
                    command: "build.run".to_string(),
                    component_id: comp.id.clone(),
                    build_command: build_cmd,
                    output: CapturedOutput::new(String::new(), stderr),
                    success: false,
                },
                exit_code,
            ));
        }
    }

    // Fix local permissions before build to ensure zip has correct permissions
    permissions::fix_local_permissions(&local_path_str);

    // Get extension path env vars for build command (matches pre-build script behavior)
    let env_vars = get_build_env_vars(comp);
    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let cmd_output = execute_local_command_in_dir(
        &build_cmd,
        Some(&local_path_str),
        if env_refs.is_empty() {
            None
        } else {
            Some(&env_refs)
        },
    );

    Ok((
        BuildOutput {
            command: "build.run".to_string(),
            component_id: comp.id.clone(),
            build_command: build_cmd,
            output: CapturedOutput::new(cmd_output.stdout, cmd_output.stderr),
            success: cmd_output.success,
        },
        cmd_output.exit_code,
    ))
}

/// Run pre-build scripts from all configured extensions.
/// Returns Some((exit_code, stderr)) if any script fails, None if all pass or no scripts.
fn run_pre_build_scripts(comp: &Component) -> Result<Option<(i32, String)>> {
    let extensions = match &comp.extensions {
        Some(m) => m,
        None => return Ok(None),
    };

    for extension_id in extensions.keys() {
        let extension = match extension::load_extension(extension_id) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let build_config = match &extension.build {
            Some(b) => b,
            None => continue,
        };

        let pre_build_script = match &build_config.pre_build_script {
            Some(s) => s,
            None => continue,
        };

        let extension_path = paths::extension(extension_id)?;
        let script_path = extension_path.join(pre_build_script);

        if !script_path.exists() {
            continue;
        }

        let env: [(&str, &str); 4] = [
            (
                exec_context::EXTENSION_PATH,
                &extension_path.to_string_lossy(),
            ),
            (exec_context::COMPONENT_ID, &comp.id),
            (exec_context::COMPONENT_PATH, &comp.local_path),
            ("HOMEBOY_PLUGIN_PATH", &comp.local_path),
        ];

        let output = execute_local_command_in_dir(&script_path.to_string_lossy(), None, Some(&env));

        if !output.success {
            let combined = if output.stderr.is_empty() {
                output.stdout
            } else {
                output.stderr
            };
            return Ok(Some((output.exit_code, combined)));
        }
    }

    Ok(None)
}

/// Get environment variables for build commands (extension path, component path).
/// Matches the env vars passed to pre-build scripts for consistency.
fn get_build_env_vars(comp: &Component) -> Vec<(String, String)> {
    let mut env = Vec::new();

    // Always pass the component ID so build scripts can name artifacts consistently
    env.push((exec_context::COMPONENT_ID.to_string(), comp.id.clone()));

    if let Some(extensions) = &comp.extensions {
        for extension_id in extensions.keys() {
            if let Ok(extension) = extension::load_extension(extension_id) {
                if extension.build.is_some() {
                    if let Ok(extension_path) = paths::extension(extension_id) {
                        let extension_path_str = extension_path.to_string_lossy().to_string();
                        env.push((exec_context::EXTENSION_PATH.to_string(), extension_path_str));
                        env.push((
                            exec_context::COMPONENT_PATH.to_string(),
                            comp.local_path.clone(),
                        ));
                        env.push(("HOMEBOY_PLUGIN_PATH".to_string(), comp.local_path.clone()));
                        break; // Use first extension with build config
                    }
                }
            }
        }
    }

    env
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
