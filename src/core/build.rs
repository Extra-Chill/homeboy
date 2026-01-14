use serde::Serialize;

use crate::component;
use crate::error::{Error, Result};
use crate::json::{is_json_input, parse_bulk_ids, BulkResult, BulkSummary, ItemOutcome};
use crate::ssh::execute_local_command_in_dir;

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
    let Some(build_cmd) = component.build_command.clone() else {
        return (
            Some(1),
            Some(format!(
                "Component '{}' has no buildCommand configured. Configure one with: homeboy component set {} --json '{{\"buildCommand\": \"<command>\"}}'",
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

    let build_cmd = comp.build_command.clone().ok_or_else(|| {
        Error::other(format!(
            "Component '{}' has no buildCommand configured. Configure one with: homeboy component set {} --json '{{\"buildCommand\": \"<command>\"}}'",
            component_id,
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
