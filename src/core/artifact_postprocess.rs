//! Generic artifact postprocess runner binding.
//!
//! Workload owners declare helper/action/input/output/parameters. Core executes
//! that portable contract and records produced files without interpreting the
//! workload domain.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::core::artifact_manifest::{self, ARTIFACT_MANIFEST_FILE};
use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, ObservationStore};
use crate::core::rig::ArtifactPostprocessSpec;

#[derive(Clone)]
pub struct ArtifactPostprocessContext<'a> {
    pub artifact_root: &'a Path,
    pub input_root: Option<&'a Path>,
    pub path_expander: Option<&'a dyn Fn(&str) -> PathBuf>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPostprocessOutput {
    pub id: String,
    pub helper: String,
    pub action: String,
    pub input: Option<String>,
    pub output: String,
    pub required: bool,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactPostprocessProducedArtifact>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPostprocessProducedArtifact {
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json_value_is_empty")]
    pub metadata: serde_json::Value,
}

pub fn run_artifact_postprocess_steps(
    steps: &[ArtifactPostprocessSpec],
    context: &ArtifactPostprocessContext<'_>,
) -> Result<Vec<ArtifactPostprocessOutput>> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(context.artifact_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(context.artifact_root.display().to_string()),
        )
    })?;

    steps
        .iter()
        .enumerate()
        .map(|(index, step)| run_artifact_postprocess_step(index, step, context))
        .collect()
}

pub fn record_artifact_postprocess_outputs(
    store: &ObservationStore,
    run_id: &str,
    outputs: &[ArtifactPostprocessOutput],
) -> Result<Vec<ArtifactRecord>> {
    let mut records = Vec::new();
    for output in outputs {
        for artifact in &output.artifacts {
            let mut metadata = artifact.metadata.clone();
            merge_object_metadata(
                &mut metadata,
                serde_json::json!({
                    "source": "homeboy.artifact-postprocess",
                    "postprocess_id": output.id,
                    "helper": output.helper,
                    "action": output.action,
                    "declared_artifact_id": artifact.id,
                    "declared_path": artifact.path,
                }),
            );
            records.push(store.record_artifact_with_metadata(
                run_id,
                &artifact.kind,
                &artifact.path,
                metadata,
            )?);
        }
    }
    Ok(records)
}

fn run_artifact_postprocess_step(
    index: usize,
    step: &ArtifactPostprocessSpec,
    context: &ArtifactPostprocessContext<'_>,
) -> Result<ArtifactPostprocessOutput> {
    let id = step
        .id
        .clone()
        .unwrap_or_else(|| format!("artifact-postprocess-{}", index + 1));
    let output_path = resolve_postprocess_output_path(&step.output, context.artifact_root)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let input = step
        .input
        .as_ref()
        .map(|input| expand_postprocess_path(input, context));
    let parameters_json = serde_json::to_string(&step.parameters).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize artifact postprocess parameters".to_string()),
        )
    })?;

    let mut command = Command::new(&step.helper);
    command.arg(&step.action);
    if let Some(args) = step
        .parameters
        .get("args")
        .and_then(serde_json::Value::as_array)
    {
        for arg in args.iter().filter_map(serde_json::Value::as_str) {
            command.arg(arg);
        }
    }
    command.env("HOMEBOY_ARTIFACT_POSTPROCESS_ID", &id);
    command.env("HOMEBOY_ARTIFACT_POSTPROCESS_HELPER", &step.helper);
    command.env("HOMEBOY_ARTIFACT_POSTPROCESS_ACTION", &step.action);
    command.env("HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT", &output_path);
    command.env(
        "HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT",
        context.artifact_root,
    );
    command.env("HOMEBOY_ARTIFACT_POSTPROCESS_PARAMETERS", parameters_json);
    if let Some(input) = input.as_ref() {
        command.env("HOMEBOY_ARTIFACT_POSTPROCESS_INPUT", input);
    }
    for (key, value) in &step.parameters {
        if let Some(value) = postprocess_parameter_env_value(value) {
            command.env(postprocess_parameter_env_key(key), value);
        }
    }

    let output = command.output().map_err(|error| {
        Error::internal_io(
            format!(
                "failed to run artifact postprocess `{}` via helper `{}`: {error}",
                id, step.helper
            ),
            Some("homeboy.artifact-postprocess".to_string()),
        )
    })?;
    let exit_code = output.status.code();
    let mut success = output.status.success();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut error = (!success).then(|| {
        format!(
            "artifact postprocess `{id}` failed with exit code {}",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    });
    if success && step.required && !output_path.exists() {
        success = false;
        error = Some(format!(
            "artifact postprocess `{id}` did not create required output {}",
            output_path.display()
        ));
    }

    let artifacts = if output_path.exists() {
        produced_artifacts(&id, &output_path)?
    } else {
        Vec::new()
    };

    Ok(ArtifactPostprocessOutput {
        id,
        helper: step.helper.clone(),
        action: step.action.clone(),
        input: input.map(|path| path.to_string_lossy().to_string()),
        output: output_path.to_string_lossy().to_string(),
        required: step.required,
        exit_code,
        success,
        stdout,
        stderr,
        error,
        artifacts,
    })
}

fn produced_artifacts(
    postprocess_id: &str,
    output_path: &Path,
) -> Result<Vec<ArtifactPostprocessProducedArtifact>> {
    if output_path.is_file() {
        return Ok(vec![ArtifactPostprocessProducedArtifact {
            kind: postprocess_id.to_string(),
            path: output_path.to_string_lossy().to_string(),
            id: None,
            metadata: serde_json::json!({}),
        }]);
    }

    if !output_path.is_dir() {
        return Ok(Vec::new());
    }

    let manifest_path = output_path.join(ARTIFACT_MANIFEST_FILE);
    let manifest = if manifest_path.is_file() {
        artifact_manifest::read_manifest_from_root(output_path)?
    } else {
        artifact_manifest::manifest_for_existing_files(output_path)?
    };
    manifest
        .validate_under(output_path)?
        .into_iter()
        .map(|validated| {
            let mut metadata = validated.entry.metadata.clone();
            merge_object_metadata(
                &mut metadata,
                serde_json::json!({
                    "manifest_path": manifest_path.is_file().then(|| manifest_path.to_string_lossy().to_string()),
                    "manifest_entry_path": validated.entry.path,
                    "role": validated.entry.role,
                    "label": validated.entry.label,
                    "semantic_key": validated.entry.semantic_key,
                }),
            );
            Ok(ArtifactPostprocessProducedArtifact {
                kind: validated.entry.kind,
                path: validated.absolute_path.to_string_lossy().to_string(),
                id: validated.entry.id,
                metadata,
            })
        })
        .collect()
}

fn resolve_postprocess_output_path(output: &str, artifact_root: &Path) -> Result<PathBuf> {
    let trimmed = output.trim();
    let path = Path::new(trimmed);
    if trimmed.is_empty() || path.is_absolute() || trimmed.starts_with("..") {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.output",
            "artifact postprocess output must be a non-empty path relative to HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT",
            Some(output.to_string()),
            None,
        ));
    }
    let resolved = artifact_root.join(path);
    if !resolved.starts_with(artifact_root) {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.output",
            "artifact postprocess output must stay within HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT",
            Some(output.to_string()),
            None,
        ));
    }
    Ok(resolved)
}

fn expand_postprocess_path(value: &str, context: &ArtifactPostprocessContext<'_>) -> PathBuf {
    let expanded = match context.input_root {
        Some(input_root) => value.replace("${run.input}", &input_root.to_string_lossy()),
        None => value.to_string(),
    }
    .replace("${run.artifacts}", &context.artifact_root.to_string_lossy());
    context
        .path_expander
        .map(|expand| expand(&expanded))
        .unwrap_or_else(|| PathBuf::from(expanded))
}

fn postprocess_parameter_env_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn postprocess_parameter_env_key(key: &str) -> String {
    let suffix: String = key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("HOMEBOY_ARTIFACT_POSTPROCESS_PARAM_{suffix}")
}

fn merge_object_metadata(target: &mut serde_json::Value, extra: serde_json::Value) {
    if !target.is_object() {
        *target = serde_json::json!({});
    }
    let Some(target) = target.as_object_mut() else {
        return;
    };
    let Some(extra) = extra.as_object() else {
        return;
    };
    for (key, value) in extra {
        if !value.is_null() {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn serde_json_value_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::NewRunRecord;
    use crate::test_support::with_isolated_home;

    #[test]
    fn executes_generic_helper_and_records_manifest_artifact() {
        with_isolated_home(|home| {
            let input = home.path().join("input.json");
            std::fs::write(&input, r#"{"status":"ok"}"#).expect("input");
            let artifact_root = home.path().join("artifacts");
            let step: ArtifactPostprocessSpec = serde_json::from_value(serde_json::json!({
                "id": "generic-report",
                "helper": "sh",
                "action": "-c",
                "input": "${run.input}",
                "output": "reports",
                "parameters": {
                    "args": ["mkdir -p \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\" && cp \"$HOMEBOY_ARTIFACT_POSTPROCESS_INPUT\" \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT/report.json\" && printf '{\"schema\":\"homeboy/artifact-manifest/v1\",\"artifacts\":[{\"id\":\"report\",\"path\":\"report.json\",\"kind\":\"proof_report\",\"metadata\":{\"custom\":\"yes\"}}]}\n' > \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT/homeboy-artifact-manifest.json\""]
                }
            }))
            .expect("step");

            let outputs = run_artifact_postprocess_steps(
                &[step],
                &ArtifactPostprocessContext {
                    artifact_root: &artifact_root,
                    input_root: Some(&input),
                    path_expander: None,
                },
            )
            .expect("postprocess");

            assert_eq!(outputs.len(), 1);
            assert!(outputs[0].success);
            assert_eq!(outputs[0].artifacts.len(), 1);
            assert_eq!(outputs[0].artifacts[0].kind, "proof_report");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("test")
                        .command("test")
                        .metadata(serde_json::json!({}))
                        .build(),
                )
                .expect("run");
            let records =
                record_artifact_postprocess_outputs(&store, &run.id, &outputs).expect("records");

            assert_eq!(records.len(), 1);
            assert_eq!(records[0].kind, "proof_report");
            assert_eq!(
                records[0].metadata_json["source"],
                "homeboy.artifact-postprocess"
            );
            assert_eq!(records[0].metadata_json["postprocess_id"], "generic-report");
            assert_eq!(records[0].metadata_json["declared_artifact_id"], "report");
            assert_eq!(records[0].metadata_json["custom"], "yes");
        });
    }
}
