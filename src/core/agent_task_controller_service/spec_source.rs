//! Controller materialize spec-source resolution.
//!
//! Resolves the `materialize` command's `--spec` input into a concrete repo
//! loop spec. The input may be a literal spec or a generator manifest that
//! declares a command to run; in the latter case this module executes the
//! generator (process execution), reads the generated spec from disk, validates
//! it, and assembles generator evidence. This orchestration belongs in core so
//! the CLI adapter stays thin.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task_loop_definition::{
    materialize_repo_loop_spec, AgentTaskLoopSpecMaterializationRequest,
};
use crate::core::config;
use crate::core::proof::validate_proof_value;
use crate::core::{Error, Result};

use super::AgentTaskRepoLoopSpec;

const CONTROLLER_SPEC_GENERATOR_SCHEMA: &str = "homeboy/agent-task-loop-spec-generator/v1";

/// Resolved controller materialize spec plus optional generator evidence.
pub struct MaterializeSpecSource {
    pub spec: AgentTaskRepoLoopSpec,
    pub generator_evidence: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ControllerSpecGeneratorManifest {
    schema: String,
    command: Vec<String>,
    output_path: String,
    #[serde(default)]
    inputs: Value,
}

/// Load the materialize spec source, executing a generator manifest if the
/// input declares one rather than an inline spec.
pub fn load_materialize_spec_source(source: &str) -> Result<MaterializeSpecSource> {
    let raw = config::read_json_spec_to_string(source)?;
    match serde_json::from_str::<AgentTaskRepoLoopSpec>(&raw) {
        Ok(spec) => Ok(MaterializeSpecSource {
            spec,
            generator_evidence: None,
        }),
        Err(spec_error) => {
            let manifest: ControllerSpecGeneratorManifest =
                serde_json::from_str(&raw).map_err(|_| {
                    Error::validation_invalid_argument(
                        "spec",
                        spec_error.to_string(),
                        Some(source.to_string()),
                        None,
                    )
                })?;
            load_generated_materialize_spec(source, manifest)
        }
    }
}

fn load_generated_materialize_spec(
    source: &str,
    manifest: ControllerSpecGeneratorManifest,
) -> Result<MaterializeSpecSource> {
    validate_generator_manifest(source, &manifest)?;
    let manifest_dir = manifest_base_dir(source);
    let output_path = resolve_manifest_path(manifest_dir.as_deref(), &manifest.output_path);
    let command_status = run_spec_generator_command(&manifest, manifest_dir.as_deref())?;
    let generated_raw = std::fs::read_to_string(&output_path).map_err(|error| {
        Error::validation_invalid_argument(
            "spec.output_path",
            format!(
                "generator command completed but generated spec was not found at {}; rerun the manifest command or update output_path: {error}",
                output_path.display()
            ),
            Some(source.to_string()),
            Some(vec![format!(
                "{} must write {}",
                manifest.command.join(" "),
                manifest.output_path
            )]),
        )
    })?;
    let spec: AgentTaskRepoLoopSpec = serde_json::from_str(&generated_raw).map_err(|error| {
        Error::validation_invalid_argument(
            "spec.output_path",
            format!("generated spec JSON is invalid: {error}"),
            Some(output_path.display().to_string()),
            None,
        )
    })?;
    let materialized = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
        spec: &spec,
        run_inputs: &Value::Null,
        policy_results: &[],
    })?;
    let validation_result =
        validate_proof_value(serde_json::to_value(materialized).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("controller.materialize.generator.serialize".to_string()),
            )
        })?);
    let spec_hash = format!("{:x}", Sha256::digest(generated_raw.as_bytes()));

    Ok(MaterializeSpecSource {
        spec,
        generator_evidence: Some(serde_json::json!({
            "schema": "homeboy/agent-task-loop-spec-generator-evidence/v1",
            "manifest": source,
            "command": manifest.command,
            "inputs": manifest.inputs,
            "output_path": output_path.display().to_string(),
            "spec_hash": spec_hash,
            "validation_result": validation_result,
            "status": command_status,
        })),
    })
}

fn validate_generator_manifest(
    source: &str,
    manifest: &ControllerSpecGeneratorManifest,
) -> Result<()> {
    if manifest.schema != CONTROLLER_SPEC_GENERATOR_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "spec.schema",
            format!(
                "controller materialize generator manifests must use schema {CONTROLLER_SPEC_GENERATOR_SCHEMA}"
            ),
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.command.is_empty() || manifest.command.iter().any(|part| part.trim().is_empty()) {
        return Err(Error::validation_invalid_argument(
            "spec.command",
            "generator command must be a non-empty array of program and arguments",
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.output_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "spec.output_path",
            "generator manifest must declare the spec file path written by the command",
            Some(source.to_string()),
            None,
        ));
    }
    if !manifest.inputs.is_null() && !manifest.inputs.is_object() {
        return Err(Error::validation_invalid_argument(
            "spec.inputs",
            "generator manifest inputs must be a JSON object when present",
            Some(source.to_string()),
            None,
        ));
    }
    Ok(())
}

fn manifest_base_dir(source: &str) -> Option<PathBuf> {
    let file = source.strip_prefix('@')?;
    if file == "-" {
        return None;
    }
    Path::new(file).parent().map(Path::to_path_buf)
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        path
    }
}

fn run_spec_generator_command(
    manifest: &ControllerSpecGeneratorManifest,
    manifest_dir: Option<&Path>,
) -> Result<Value> {
    let mut command = Command::new(&manifest.command[0]);
    command.args(&manifest.command[1..]);
    if let Some(manifest_dir) = manifest_dir {
        command.current_dir(manifest_dir);
    }
    let output = command.output().map_err(|error| {
        Error::validation_invalid_argument(
            "spec.command",
            format!("failed to execute generator command: {error}"),
            Some(manifest.command.join(" ")),
            None,
        )
    })?;
    let code = output.status.code();
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "spec.command",
            format!(
                "generator command exited with status {}; stderr: {}",
                code.map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string()),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(manifest.command.join(" ")),
            None,
        ));
    }
    Ok(serde_json::json!({
        "exit_code": code,
        "stdout": String::from_utf8_lossy(&output.stdout).trim(),
        "stderr": String::from_utf8_lossy(&output.stderr).trim(),
    }))
}
