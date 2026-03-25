//! internal_implementation — extracted from mod.rs.

use std::path::PathBuf;
use crate::component::{self, Component};
use crate::config::{is_json_input, parse_bulk_ids};
use crate::deploy::permissions;
use crate::engine::command::CapturedOutput;
use crate::engine::shell;
use crate::error::{Error, Result};
use crate::extension::{self, exec_context, ExtensionCapability, ExtensionExecutionContext};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::paths;
use crate::server::execute_local_command_in_dir;
use serde::Serialize;
use crate::core::extension::build::ResolvedBuildCommand;
use crate::core::extension::build::BuildOutput;
use crate::core::extension::build::run;
use crate::core::extension::build::BuildResult;


/// Resolve build command for a component using extension-managed build configuration.
///
/// Priority:
/// 1. Extension's bundled script (`extension.build.extension_script`)
/// 2. Local script matching the extension's `script_names` pattern
pub(crate) fn resolve_build_command(component: &Component) -> Result<ResolvedBuildCommand> {
    // 1. Check exactly one build-capable extension for bundled script or local script patterns
    if let Ok(context) = extension::resolve_execution_context(component, ExtensionCapability::Build)
    {
        let extension_id = context.extension_id.clone();
        let extension = extension::load_extension(&extension_id)?;
        if let Some(build) = &extension.build {
            // Priority 1: Extension's bundled build script
            let bundled = build
                .extension_script
                .as_ref()
                .and_then(|extension_script| {
                    paths::extension(&extension_id)
                        .ok()
                        .and_then(|extension_dir| {
                            let script_path = extension_dir.join(extension_script);
                            script_path.exists().then(|| {
                                let quoted_path = shell::quote_path(&script_path.to_string_lossy());
                                let command = build
                                    .command_template
                                    .as_ref()
                                    .map(|t| t.replace("{{script}}", &quoted_path))
                                    .unwrap_or_else(|| format!("sh {}", quoted_path));
                                ResolvedBuildCommand::ExtensionProvided {
                                    context: context.clone(),
                                    command,
                                    source: format!("{}:{}", extension_id, extension_script),
                                }
                            })
                        })
                });
            if let Some(result) = bundled {
                return Ok(result);
            }

            // Priority 2: Local script matching the extension's script_names pattern
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

    if extension::extension_provides_build(component) {
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
        Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "Component '{}' has no linked extension with build support",
                component.id
            ),
            Some(component.id.clone()),
            Some(vec![
                format!("Link an extension with build support: homeboy component set {} --extension <extension_id>", component.id),
            ]),
        ))
    }
}

pub(crate) fn run_single(component_id: &str) -> Result<(BuildResult, i32)> {
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

pub(crate) fn run_bulk(json_spec: &str) -> Result<(BuildResult, i32)> {
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

/// Build multiple pre-resolved components.
pub fn run_components(components: &[Component]) -> Result<(BuildResult, i32)> {
    let mut results = Vec::with_capacity(components.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for component in components {
        match execute_build_component(component) {
            Ok((output, _)) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: component.id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: component.id.clone(),
                    result: None,
                    error: Some(error.to_string()),
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

pub(crate) fn execute_build(component_id: &str, path_override: Option<&str>) -> Result<(BuildOutput, i32)> {
    let comp = component::resolve_effective(Some(component_id), path_override, None)?;
    execute_build_component(&comp)
}

pub(crate) fn execute_build_component(comp: &Component) -> Result<(BuildOutput, i32)> {
    // Validate required extensions are installed before resolving build commands.
    // Without this, missing extensions cause vague "no build command" errors.
    extension::validate_required_extensions(comp)?;

    // Validate local_path before attempting build
    let validated_path = component::validate_local_path(comp)?;
    let local_path_str = validated_path.to_string_lossy().to_string();

    let resolved = resolve_build_command(comp)?;
    let build_cmd = resolved.command().to_string();
    let build_context = match &resolved {
        ResolvedBuildCommand::ExtensionProvided { context, .. } => Some(context),
        _ => None,
    };

    // Run pre-build script if extension provides one
    if let Some((exit_code, stderr)) = run_pre_build_scripts(build_context)? {
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

    // Execute via ExtensionRunner — uses the full exec context protocol (settings,
    // project info, context version) instead of the minimal env var set.
    let runner_output = if let Some(context) = build_context {
        extension::ExtensionRunner::for_context(context.clone())
            .component(comp.clone())
            .working_dir(&local_path_str)
            .command_override(build_cmd.clone())
            // Legacy env var for backward compat with existing build scripts
            .env("HOMEBOY_PLUGIN_PATH", &comp.local_path)
            .run()?
    } else {
        // LocalScript variant — no extension context, run command directly
        let context =
            extension::resolve_execution_context(comp, extension::ExtensionCapability::Build)?;
        extension::ExtensionRunner::for_context(context)
            .component(comp.clone())
            .working_dir(&local_path_str)
            .command_override(build_cmd.clone())
            .env("HOMEBOY_PLUGIN_PATH", &comp.local_path)
            .run()?
    };

    Ok((
        BuildOutput {
            command: "build.run".to_string(),
            component_id: comp.id.clone(),
            build_command: build_cmd,
            output: CapturedOutput::new(runner_output.stdout, runner_output.stderr),
            success: runner_output.success,
        },
        runner_output.exit_code,
    ))
}

/// Run pre-build scripts from all configured extensions.
/// Returns Some((exit_code, stderr)) if any script fails, None if all pass or no scripts.
pub(crate) fn run_pre_build_scripts(
    build_context: Option<&ExtensionExecutionContext>,
) -> Result<Option<(i32, String)>> {
    let Some(build_context) = build_context else {
        return Ok(None);
    };

    let extension = extension::load_extension(&build_context.extension_id)?;
    let build_config = match &extension.build {
        Some(b) => b,
        None => return Ok(None),
    };

    let pre_build_script = match &build_config.pre_build_script {
        Some(s) => s,
        None => return Ok(None),
    };

    let script_path = build_context.extension_path.join(pre_build_script);
    if !script_path.exists() {
        return Ok(None);
    }

    let extension_path_lossy = build_context.extension_path.to_string_lossy().to_string();
    let env: [(&str, &str); 4] = [
        (exec_context::EXTENSION_PATH, &extension_path_lossy),
        (exec_context::COMPONENT_ID, &build_context.component.id),
        (
            exec_context::COMPONENT_PATH,
            &build_context.component.local_path,
        ),
        ("HOMEBOY_PLUGIN_PATH", &build_context.component.local_path),
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

    Ok(None)
}
