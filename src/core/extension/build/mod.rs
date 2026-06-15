use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::core::artifact_inputs::{self, ResolvedArtifactInput};
use crate::core::component::{self, Component};
use crate::core::config::{is_json_input, parse_bulk_ids};
use crate::core::deploy::permissions;
use crate::core::engine::command::CapturedOutput;
use crate::core::engine::run_dir::RunDir;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::extension::{
    self, exec_context, ExtensionCapability, ExtensionExecutionContext, ExtensionPhaseTiming,
};
use crate::core::output::{BulkResult, BulkResultBuilder};
use crate::core::paths;
use crate::core::server::execute_local_command_in_dir;

mod artifact;

pub use artifact::{resolve_artifact_path, resolve_artifact_path_from_root};

// === Build Command Resolution ===

#[derive(Debug, Clone)]
pub enum ResolvedBuildCommand {
    ComponentScript {
        command: String,
    },
    ExtensionProvided {
        context: ExtensionExecutionContext,
        command: String,
        source: String,
    },
    LocalScript {
        context: ExtensionExecutionContext,
        command: String,
        script_name: String,
    },
}

impl ResolvedBuildCommand {
    pub fn command(&self) -> &str {
        match self {
            ResolvedBuildCommand::ComponentScript { command } => command,
            ResolvedBuildCommand::ExtensionProvided { command, .. } => command,
            ResolvedBuildCommand::LocalScript { command, .. } => command,
        }
    }
}

/// Resolve build command for a component using extension-managed build configuration.
///
/// Priority:
/// 1. Component-owned `scripts.build`
/// 2. Extension's bundled script (`extension.build.extension_script`)
/// 3. Local script matching the extension's `script_names` pattern
pub(crate) fn resolve_build_command(component: &Component) -> Result<ResolvedBuildCommand> {
    let component_scripts = component.script_commands(ExtensionCapability::Build);
    if !component_scripts.is_empty() {
        return Ok(ResolvedBuildCommand::ComponentScript {
            command: component_scripts.join(" && "),
        });
    }

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
                        context: context.clone(),
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
                "Component '{}' links an extension with build support, but no build script was found.\n\
                 Expected: extension's bundled script OR local script matching extension pattern.\n\
                 Check extension installation or add a local build.sh to the component directory.",
                component.id
            ),
            Some(component.id.clone()),
            None,
        ))
    } else {
        let mut err = Error::validation_invalid_argument(
            "extensions",
            format!(
                "Component '{}' has no linked extension with build support",
                component.id
            ),
            Some(component.id.clone()),
            None,
        );

        for hint in extension::extension_guidance_hints(component, Some(ExtensionCapability::Build))
        {
            err = err.with_hint(hint);
        }

        Err(err)
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<ResolvedArtifactInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_scope: Option<BuildChangedScopeReport>,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BuildChangedScopeReport {
    pub changed_since: String,
    pub outcome: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BuildChangedScopeOutcome {
    NoOp,
    Scoped { build_args: Vec<String> },
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildChangedScopeDecision {
    outcome: BuildChangedScopeOutcome,
    report: BuildChangedScopeReport,
}

#[derive(Debug, Deserialize)]
struct ProviderChangedScopeResponse {
    outcome: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    build_args: Vec<String>,
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
    run_changed_since(input, None)
}

pub fn run_changed_since(input: &str, changed_since: Option<&str>) -> Result<(BuildResult, i32)> {
    if is_json_input(input) {
        run_bulk_changed_since(input, changed_since)
    } else {
        run_single_changed_since(input, changed_since)
    }
}

/// Build a component for deploy context.
/// Returns (exit_code, error_message) - None error means success.
///
/// Thin wrapper around `execute_build_component` that adapts the return type
/// for the deploy pipeline's error handling convention.
pub(crate) fn build_component(component: &component::Component) -> (Option<i32>, Option<String>) {
    let result = execute_build_component(component, None);

    match result {
        Ok((output, exit_code)) => {
            if output.success {
                (Some(exit_code), None)
            } else {
                (
                    Some(exit_code),
                    Some(format_build_error(
                        &component.id,
                        &output.build_command,
                        &component.local_path,
                        exit_code,
                        &output.output.stderr,
                        &output.output.stdout,
                    )),
                )
            }
        }
        Err(e) => (Some(1), Some(e.to_string())),
    }
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

fn run_single_changed_since(
    component_id: &str,
    changed_since: Option<&str>,
) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build(component_id, None, changed_since)?;
    Ok((BuildResult::Single(output), exit_code))
}

/// Build a single component with an overridden local_path.
///
/// Use this for workspace clones, temporary checkouts, or CI builds
/// where the source lives somewhere other than the configured `local_path`.
pub fn run_with_path(component_id: &str, path: &str) -> Result<(BuildResult, i32)> {
    run_with_path_changed_since(component_id, path, None)
}

pub fn run_with_path_changed_since(
    component_id: &str,
    path: &str,
    changed_since: Option<&str>,
) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build(component_id, Some(path), changed_since)?;
    Ok((BuildResult::Single(output), exit_code))
}

fn run_bulk_changed_since(
    json_spec: &str,
    changed_since: Option<&str>,
) -> Result<(BuildResult, i32)> {
    let input = parse_bulk_ids(json_spec)?;

    let mut builder = BulkResultBuilder::with_capacity("build", input.component_ids.len());

    for id in &input.component_ids {
        match execute_build(id, None, changed_since) {
            Ok((output, _)) => {
                if output.success {
                    builder.record_success(id.clone(), output);
                } else {
                    builder.record_failed_result(id.clone(), output);
                }
            }
            Err(e) => {
                builder.record_error(id.clone(), e.to_string());
            }
        }
    }

    let output = builder.finish();
    let exit_code = if output.summary.failed > 0 { 1 } else { 0 };

    Ok((BuildResult::Bulk(output), exit_code))
}

/// Build a pre-resolved component (supports both registered and discovered components).
pub fn run_component(component: &Component) -> Result<(BuildResult, i32)> {
    run_component_with_changed_since(component, None)
}

pub fn run_component_with_changed_since(
    component: &Component,
    changed_since: Option<&str>,
) -> Result<(BuildResult, i32)> {
    let (output, exit_code) = execute_build_component(component, changed_since)?;
    Ok((BuildResult::Single(output), exit_code))
}

/// Build multiple pre-resolved components.
pub fn run_components(components: &[Component]) -> Result<(BuildResult, i32)> {
    run_components_with_changed_since(components, None)
}

pub fn run_components_with_changed_since(
    components: &[Component],
    changed_since: Option<&str>,
) -> Result<(BuildResult, i32)> {
    let mut builder = BulkResultBuilder::with_capacity("build", components.len());

    for component in components {
        match execute_build_component(component, changed_since) {
            Ok((output, _)) => {
                if output.success {
                    builder.record_success(component.id.clone(), output);
                } else {
                    builder.record_failed_result(component.id.clone(), output);
                }
            }
            Err(error) => {
                builder.record_error(component.id.clone(), error.to_string());
            }
        }
    }

    let output = builder.finish();
    let exit_code = if output.summary.failed > 0 { 1 } else { 0 };

    Ok((BuildResult::Bulk(output), exit_code))
}

fn execute_build(
    component_id: &str,
    path_override: Option<&str>,
    changed_since: Option<&str>,
) -> Result<(BuildOutput, i32)> {
    let comp = component::resolve_effective(Some(component_id), path_override, None)?;
    execute_build_component(&comp, changed_since)
}

fn execute_build_component(
    comp: &Component,
    changed_since: Option<&str>,
) -> Result<(BuildOutput, i32)> {
    comp.validate_supported_build_config()?;

    // Validate required extensions are installed before resolving build commands.
    // Without this, missing extensions cause vague "no build command" errors.
    extension::validate_required_extensions(comp)?;

    // Validate local_path before attempting build
    let validated_path = component::validate_local_path(comp)?;
    let local_path_str = validated_path.to_string_lossy().to_string();

    // Warn when HEAD is ahead of the latest tag — the build will include
    // unreleased commits that won't be deployed unless using `deploy --head`.
    if let Some(gap) = crate::core::deploy::provenance::detect_tag_gap(comp) {
        crate::core::deploy::provenance::warn_tag_gap(&comp.id, &gap, "build");
        log_status!(
            "build",
            "Build uses current working tree. To deploy these commits: use `deploy --head` or run `homeboy release`."
        );
    }

    let resolved = resolve_build_command(comp)?;
    let build_cmd = resolved.command().to_string();
    let build_context = match &resolved {
        ResolvedBuildCommand::ComponentScript { .. } => None,
        ResolvedBuildCommand::ExtensionProvided { context, .. } => Some(context),
        ResolvedBuildCommand::LocalScript { context, .. } => Some(context),
    };

    let changed_scope = resolve_changed_scope(comp, build_context, changed_since, &local_path_str)?;
    if matches!(
        changed_scope.as_ref().map(|decision| &decision.outcome),
        Some(BuildChangedScopeOutcome::NoOp)
    ) {
        return Ok((
            BuildOutput {
                command: "build.run".to_string(),
                component_id: comp.id.clone(),
                build_command: build_cmd,
                output: CapturedOutput::new(String::new(), String::new()),
                artifact_inputs: Vec::new(),
                extension_phase_timings: Vec::new(),
                changed_scope: changed_scope.map(|decision| decision.report),
                success: true,
            },
            0,
        ));
    }

    let build_args = changed_scope
        .as_ref()
        .and_then(|decision| match &decision.outcome {
            BuildChangedScopeOutcome::Scoped { build_args } => Some(build_args.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let build_cmd = command_with_args(&build_cmd, &build_args);

    // Run pre-build script if extension provides one
    if let Some((exit_code, stderr)) = run_pre_build_scripts(build_context)? {
        if exit_code != 0 {
            return Ok((
                BuildOutput {
                    command: "build.run".to_string(),
                    component_id: comp.id.clone(),
                    build_command: build_cmd,
                    output: CapturedOutput::new(String::new(), stderr),
                    artifact_inputs: Vec::new(),
                    extension_phase_timings: Vec::new(),
                    changed_scope: changed_scope.clone().map(|decision| decision.report),
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
    let run_dir = RunDir::create()?;
    let runner_output = if let ResolvedBuildCommand::ComponentScript { .. } = &resolved {
        crate::core::extension::component_script::run_component_scripts_with_run_dir(
            comp,
            extension::ExtensionCapability::Build,
            &validated_path,
            &run_dir,
            true,
            &build_env(changed_since, changed_scope.as_ref()),
            &[],
        )?
        .into()
    } else if let Some(context) = build_context {
        let mut runner = extension::ExtensionRunner::for_context(context.clone())
            .component(comp.clone())
            .working_dir(&local_path_str)
            .command_override(build_cmd.clone())
            .with_run_dir(&run_dir)
            // Legacy env var for backward compat with existing build scripts
            .env("HOMEBOY_PLUGIN_PATH", &comp.local_path);
        for (key, value) in build_env(changed_since, changed_scope.as_ref()) {
            runner = runner.env(&key, &value);
        }
        runner.run()?
    } else {
        let context =
            extension::resolve_execution_context(comp, extension::ExtensionCapability::Build)?;
        let mut runner = extension::ExtensionRunner::for_context(context)
            .component(comp.clone())
            .working_dir(&local_path_str)
            .command_override(build_cmd.clone())
            .with_run_dir(&run_dir)
            .env("HOMEBOY_PLUGIN_PATH", &comp.local_path);
        for (key, value) in build_env(changed_since, changed_scope.as_ref()) {
            runner = runner.env(&key, &value);
        }
        runner.run()?
    };

    let success = runner_output.success;
    let artifact_inputs = if success {
        apply_artifact_inputs(comp)?
    } else {
        Vec::new()
    };

    Ok((
        BuildOutput {
            command: "build.run".to_string(),
            component_id: comp.id.clone(),
            build_command: build_cmd,
            output: CapturedOutput::new(runner_output.stdout, runner_output.stderr),
            artifact_inputs,
            extension_phase_timings: runner_output.extension_phase_timings,
            changed_scope: changed_scope.map(|decision| decision.report),
            success,
        },
        runner_output.exit_code,
    ))
}

fn resolve_changed_scope(
    comp: &Component,
    build_context: Option<&ExtensionExecutionContext>,
    changed_since: Option<&str>,
    working_dir: &str,
) -> Result<Option<BuildChangedScopeDecision>> {
    let Some(changed_since) = changed_since else {
        return Ok(None);
    };

    let Some(context) = build_context else {
        return Ok(Some(full_scope_decision(
            changed_since,
            None,
            "no build provider changed-scope resolver is configured; running full build",
        )));
    };

    let extension = extension::load_extension(&context.extension_id)?;
    let changed_scope_script = extension
        .build
        .as_ref()
        .and_then(|build| build.changed_scope_script.as_deref());
    let Some(changed_scope_script) = changed_scope_script else {
        return Ok(Some(full_scope_decision(
            changed_since,
            Some(context.extension_id.clone()),
            "build provider has no changed-scope resolver; running full build",
        )));
    };

    let mut scope_context = context.clone();
    scope_context.script_path = changed_scope_script.to_string();
    let output = extension::ExtensionRunner::for_context(scope_context)
        .component(comp.clone())
        .working_dir(working_dir)
        .env("HOMEBOY_CHANGED_SINCE", changed_since)
        .passthrough(false)
        .run()?;

    if !output.success {
        return Ok(Some(full_scope_decision(
            changed_since,
            Some(context.extension_id.clone()),
            "changed-scope resolver failed; running full build",
        )));
    }

    Ok(Some(provider_scope_decision(
        changed_since,
        Some(context.extension_id.clone()),
        &output.stdout,
    )))
}

fn provider_scope_decision(
    changed_since: &str,
    provider: Option<String>,
    stdout: &str,
) -> BuildChangedScopeDecision {
    let parsed = serde_json::from_str::<ProviderChangedScopeResponse>(stdout.trim());
    let Ok(response) = parsed else {
        return full_scope_decision(
            changed_since,
            provider,
            "changed-scope resolver did not return valid JSON; running full build",
        );
    };

    let reason = response
        .reason
        .unwrap_or_else(|| "provider changed-scope decision".to_string());
    match response.outcome.as_str() {
        "no-op" | "noop" | "skip" => BuildChangedScopeDecision {
            outcome: BuildChangedScopeOutcome::NoOp,
            report: BuildChangedScopeReport {
                changed_since: changed_since.to_string(),
                outcome: "no-op".to_string(),
                reason,
                provider,
                build_args: Vec::new(),
            },
        },
        "scoped" => BuildChangedScopeDecision {
            outcome: BuildChangedScopeOutcome::Scoped {
                build_args: response.build_args.clone(),
            },
            report: BuildChangedScopeReport {
                changed_since: changed_since.to_string(),
                outcome: "scoped".to_string(),
                reason,
                provider,
                build_args: response.build_args,
            },
        },
        "full" => full_scope_decision(changed_since, provider, reason),
        _ => full_scope_decision(
            changed_since,
            provider,
            "changed-scope resolver returned an unknown outcome; running full build",
        ),
    }
}

fn full_scope_decision(
    changed_since: &str,
    provider: Option<String>,
    reason: impl Into<String>,
) -> BuildChangedScopeDecision {
    BuildChangedScopeDecision {
        outcome: BuildChangedScopeOutcome::Full,
        report: BuildChangedScopeReport {
            changed_since: changed_since.to_string(),
            outcome: "full".to_string(),
            reason: reason.into(),
            provider,
            build_args: Vec::new(),
        },
    }
}

fn build_env(
    changed_since: Option<&str>,
    changed_scope: Option<&BuildChangedScopeDecision>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(changed_since) = changed_since {
        env.push((
            "HOMEBOY_CHANGED_SINCE".to_string(),
            changed_since.to_string(),
        ));
    }
    if let Some(changed_scope) = changed_scope {
        env.push((
            "HOMEBOY_BUILD_SCOPE_OUTCOME".to_string(),
            changed_scope.report.outcome.clone(),
        ));
    }
    env
}

fn command_with_args(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        return command.to_string();
    }

    format!(
        "{} {}",
        command,
        crate::core::engine::shell::quote_args(args)
    )
}

fn apply_artifact_inputs(comp: &Component) -> Result<Vec<ResolvedArtifactInput>> {
    if comp.artifact_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let artifact_pattern = component::resolve_artifact(comp).ok_or_else(|| {
        Error::validation_invalid_argument(
            "build_artifact",
            format!(
                "Component '{}' declares artifact_inputs but has no build_artifact configured",
                comp.id
            ),
            Some(comp.id.clone()),
            None,
        )
    })?;
    let artifact_path =
        resolve_artifact_path_from_root(&artifact_pattern, Some(Path::new(&comp.local_path)))?;
    artifact_inputs::apply_to_component_artifact(comp, &artifact_path)
}

/// Run pre-build scripts from all configured extensions.
/// Returns Some((exit_code, stderr)) if any script fails, None if all pass or no scripts.
fn run_pre_build_scripts(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ComponentScriptsConfig;

    #[test]
    fn is_json_input_detects_json() {
        assert!(is_json_input(r#"{"componentIds": ["a"]}"#));
        assert!(is_json_input(r#"  {"componentIds": ["a"]}"#));
        assert!(!is_json_input("extrachill-api"));
        assert!(!is_json_input("some-component-id"));
    }

    #[test]
    fn resolve_build_command_guides_unconfigured_components() {
        let component = Component {
            id: "plain-package".to_string(),
            ..Default::default()
        };

        let err = resolve_build_command(&component).unwrap_err();
        assert!(err
            .message
            .contains("no linked extension with build support"));
        assert!(err.hints.iter().any(|hint| {
            hint.message
                .contains("homeboy component set plain-package --extension")
        }));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Use `scripts.build` for component-owned build commands")));
    }

    #[test]
    fn deploy_build_rejects_legacy_build_command_before_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "artifact-component".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            build_artifact: Some("dist/component.zip".to_string()),
            build_command: Some(
                "mkdir -p dist && printf explicit > dist/component.zip".to_string(),
            ),
            scripts: Some(ComponentScriptsConfig {
                build: vec!["mkdir -p dist && printf generic > dist/generic.zip".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        };

        let (exit_code, error) = build_component(&component);

        assert_eq!(exit_code, Some(1));
        let error = error.expect("legacy build_command should fail");
        assert!(
            error.contains("unsupported legacy build_command"),
            "{error}"
        );
        assert!(error.contains("Use scripts.build instead"), "{error}");
        assert!(!temp.path().join("dist/component.zip").exists());
        assert!(!temp.path().join("dist/generic.zip").exists());
    }

    #[test]
    fn build_run_rejects_legacy_build_command_before_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "artifact-component".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            build_artifact: Some("packages/content-plugin/dist/component.zip".to_string()),
            build_command: Some(
                "mkdir -p packages/content-plugin/dist && printf artifact > packages/content-plugin/dist/component.zip".to_string(),
            ),
            scripts: Some(ComponentScriptsConfig {
                build: vec!["mkdir -p dist && printf generic > dist/generic.zip".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        };

        let err = run_component(&component).expect_err("legacy build_command should fail");

        assert!(err.message.contains("unsupported legacy build_command"));
        assert!(err.message.contains("Use scripts.build instead"));
        assert!(!temp
            .path()
            .join("packages/content-plugin/dist/component.zip")
            .exists());
        assert!(!temp.path().join("dist/generic.zip").exists());
    }

    #[test]
    fn provider_changed_scope_can_report_no_op() {
        let decision = provider_scope_decision(
            "origin/main",
            Some("fixture".to_string()),
            r#"{"outcome":"no-op","reason":"docs only"}"#,
        );

        assert!(matches!(decision.outcome, BuildChangedScopeOutcome::NoOp));
        assert_eq!(decision.report.outcome, "no-op");
        assert_eq!(decision.report.reason, "docs only");
        assert_eq!(decision.report.provider.as_deref(), Some("fixture"));
    }

    #[test]
    fn provider_changed_scope_can_report_scoped_build_args() {
        let decision = provider_scope_decision(
            "origin/main",
            Some("fixture".to_string()),
            r#"{"outcome":"scoped","reason":"package changed","build_args":["package-a","--fast"]}"#,
        );

        match &decision.outcome {
            BuildChangedScopeOutcome::Scoped { build_args } => {
                assert_eq!(
                    build_args,
                    &vec!["package-a".to_string(), "--fast".to_string()]
                );
            }
            _ => panic!("expected scoped decision"),
        }
        assert_eq!(decision.report.outcome, "scoped");
        assert_eq!(decision.report.build_args, vec!["package-a", "--fast"]);
    }

    #[test]
    fn provider_changed_scope_falls_back_to_full_for_invalid_json() {
        let decision =
            provider_scope_decision("origin/main", Some("fixture".to_string()), "not-json");

        assert!(matches!(decision.outcome, BuildChangedScopeOutcome::Full));
        assert_eq!(decision.report.outcome, "full");
        assert!(decision.report.reason.contains("valid JSON"));
    }

    #[test]
    fn provider_changed_scope_falls_back_to_full_for_unknown_outcome() {
        let decision = provider_scope_decision(
            "origin/main",
            Some("fixture".to_string()),
            r#"{"outcome":"maybe","reason":"uncertain"}"#,
        );

        assert!(matches!(decision.outcome, BuildChangedScopeOutcome::Full));
        assert_eq!(decision.report.outcome, "full");
        assert!(decision.report.reason.contains("unknown outcome"));
    }

    #[test]
    fn changed_scope_env_exposes_changed_since_and_outcome() {
        let decision = full_scope_decision("origin/main", Some("fixture".to_string()), "fallback");
        let env = build_env(Some("origin/main"), Some(&decision));

        assert!(env.contains(&(
            "HOMEBOY_CHANGED_SINCE".to_string(),
            "origin/main".to_string()
        )));
        assert!(env.contains(&(
            "HOMEBOY_BUILD_SCOPE_OUTCOME".to_string(),
            "full".to_string()
        )));
    }
}
