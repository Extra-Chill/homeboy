use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::component;
use crate::error::{Error, Result};
use crate::json::{is_json_input, parse_bulk_ids, BulkResult, BulkSummary, ItemOutcome};
use crate::module::{load_all_modules, ModuleManifest};
use crate::ssh::execute_local_command_in_dir;
use crate::template::{render, TemplateVars};

// === Public API ===

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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
pub fn build_component(component: &component::Component) -> (Option<i32>, Option<String>) {
    let build_cmd = component.build_command.clone().or_else(|| {
        detect_build_command(&component.local_path, &component.build_artifact).map(|c| c.command)
    });

    let Some(build_cmd) = build_cmd else {
        return (
            Some(1),
            Some(format!(
                "Component '{}' has no build command configured. Configure one with: homeboy component set {} --build-command '<command>'",
                component.id,
                component.id
            )),
        );
    };

    let output = execute_local_command_in_dir(&build_cmd, Some(&component.local_path));

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

    let build_cmd = comp.build_command.clone().or_else(|| {
        detect_build_command(&comp.local_path, &comp.build_artifact).map(|c| c.command)
    });

    let build_cmd = build_cmd.ok_or_else(|| {
        Error::other(format!(
            "Component '{}' has no build_command configured and no build script was detected",
            component_id
        ))
    })?;

    let output = execute_local_command_in_dir(&build_cmd, Some(&comp.local_path));

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

// === Build command detection ===

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildCommandSource {
    Module,
}

pub struct BuildCommandCandidate {
    pub source: BuildCommandSource,
    pub command: String,
}

fn file_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// Detect build command using module configuration.
pub fn detect_build_command(local_path: &str, build_artifact: &str) -> Option<BuildCommandCandidate> {
    let root = PathBuf::from(local_path);

    for module in load_all_modules() {
        if let Some(candidate) = detect_build_from_module(&root, build_artifact, &module) {
            return Some(candidate);
        }
    }

    None
}

fn detect_build_from_module(
    root: &Path,
    build_artifact: &str,
    module: &ModuleManifest,
) -> Option<BuildCommandCandidate> {
    let build_config = module.build.as_ref()?;

    let artifact_lower = build_artifact.to_ascii_lowercase();
    let matches_artifact = build_config
        .artifact_extensions
        .iter()
        .any(|ext| artifact_lower.ends_with(&ext.to_ascii_lowercase()));

    if !matches_artifact {
        return None;
    }

    for script_name in &build_config.script_names {
        let script_path = root.join(script_name);
        if file_exists(&script_path) {
            let command = build_config
                .command_template
                .as_ref()
                .map(|tpl| render(tpl, &[(TemplateVars::SCRIPT, script_name)]))
                .unwrap_or_else(|| format!("sh {}", script_name));

            return Some(BuildCommandCandidate {
                source: BuildCommandSource::Module,
                command,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_detection_for_unknown_artifact_type() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("build.sh"), "#!/bin/sh\necho ok\n").unwrap();

        // Unknown artifact extension should not match any module
        let candidate =
            detect_build_command(temp_dir.path().to_str().unwrap(), "dist/app.unknownext");
        assert!(candidate.is_none());
    }

    #[test]
    fn is_json_input_detects_json() {
        assert!(is_json_input(r#"{"componentIds": ["a"]}"#));
        assert!(is_json_input(r#"  {"componentIds": ["a"]}"#));
        assert!(!is_json_input("extrachill-api"));
        assert!(!is_json_input("some-component-id"));
    }
}
