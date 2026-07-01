//! Generic artifact postprocess runner binding.
//!
//! Workload owners declare helper/action/input/output/parameters. Core executes
//! that portable contract and records produced files without interpreting the
//! workload domain.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::core::artifact_manifest::{self, ARTIFACT_MANIFEST_FILE};
use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, ObservationStore};

pub const ARTIFACT_POSTPROCESS_SCHEMA: &str = "homeboy/artifact-postprocess/v1";
pub const ARTIFACT_POSTPROCESS_PLAN_SCHEMA: &str = ARTIFACT_POSTPROCESS_SCHEMA;
pub const ARTIFACT_POSTPROCESS_RESULT_SCHEMA: &str = "homeboy/artifact-postprocess-result/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPostprocessPlan {
    #[serde(default = "artifact_postprocess_plan_schema")]
    pub schema: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_roots: Vec<ArtifactPostprocessRoot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ArtifactPostprocessAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewer_refs: Vec<ArtifactPostprocessReviewerRef>,
    #[serde(default, skip_serializing_if = "serde_json_value_is_empty")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPostprocessRoot {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPostprocessAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub helper: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    pub output: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, serde_json::Value>,
    #[serde(default = "default_artifact_postprocess_required")]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPostprocessReviewerRef {
    pub kind: String,
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPostprocessPlanDescription {
    pub schema: String,
    pub plan_id: String,
    pub artifact_root_count: usize,
    pub action_count: usize,
    pub reviewer_ref_count: usize,
    pub required_action_count: usize,
    pub output_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPostprocessResult {
    pub schema: String,
    pub plan_id: String,
    pub success: bool,
    pub outputs: Vec<ArtifactPostprocessOutput>,
    pub reviewer_refs: Vec<ArtifactPostprocessReviewerRef>,
}

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

pub fn validate_artifact_postprocess_plan(plan: &ArtifactPostprocessPlan) -> Result<()> {
    if plan.schema != ARTIFACT_POSTPROCESS_PLAN_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.schema",
            format!("artifact postprocess plan schema must be {ARTIFACT_POSTPROCESS_PLAN_SCHEMA}"),
            Some(plan.schema.clone()),
            None,
        ));
    }
    validate_non_empty("artifact_postprocess.plan_id", &plan.plan_id)?;
    if plan.artifact_roots.is_empty() {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.artifact_roots",
            "artifact postprocess plan must declare at least one persisted artifact root",
            None,
            None,
        ));
    }
    for root in &plan.artifact_roots {
        validate_non_empty("artifact_postprocess.artifact_roots.id", &root.id)?;
        validate_non_empty("artifact_postprocess.artifact_roots.path", &root.path)?;
        if let Some(persisted_ref) = &root.persisted_ref {
            validate_non_empty(
                "artifact_postprocess.artifact_roots.persisted_ref",
                persisted_ref,
            )?;
        }
        if let Some(manifest_path) = &root.manifest_path {
            validate_non_empty(
                "artifact_postprocess.artifact_roots.manifest_path",
                manifest_path,
            )?;
        }
    }
    for action in &plan.actions {
        validate_artifact_postprocess_action(action)?;
    }
    for reviewer_ref in &plan.reviewer_refs {
        validate_reviewer_ref(reviewer_ref)?;
    }
    if !plan.metadata.is_object() && !plan.metadata.is_null() {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.metadata",
            "artifact postprocess plan metadata must be an object",
            None,
            None,
        ));
    }
    Ok(())
}

pub fn describe_artifact_postprocess_plan(
    plan: &ArtifactPostprocessPlan,
) -> Result<ArtifactPostprocessPlanDescription> {
    validate_artifact_postprocess_plan(plan)?;
    Ok(ArtifactPostprocessPlanDescription {
        schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        artifact_root_count: plan.artifact_roots.len(),
        action_count: plan.actions.len(),
        reviewer_ref_count: plan.reviewer_refs.len(),
        required_action_count: plan.actions.iter().filter(|action| action.required).count(),
        output_paths: plan
            .actions
            .iter()
            .map(|action| action.output.clone())
            .collect(),
    })
}

pub fn run_artifact_postprocess_plan(
    plan: &ArtifactPostprocessPlan,
    context: &ArtifactPostprocessContext<'_>,
) -> Result<ArtifactPostprocessResult> {
    validate_artifact_postprocess_plan(plan)?;
    let outputs = run_artifact_postprocess_steps(&plan.actions, context)?;
    Ok(ArtifactPostprocessResult {
        schema: ARTIFACT_POSTPROCESS_RESULT_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        success: outputs
            .iter()
            .all(|output| output.success || !output.required),
        outputs,
        reviewer_refs: plan.reviewer_refs.clone(),
    })
}

pub fn run_artifact_postprocess_steps(
    steps: &[ArtifactPostprocessAction],
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
    step: &ArtifactPostprocessAction,
    context: &ArtifactPostprocessContext<'_>,
) -> Result<ArtifactPostprocessOutput> {
    validate_artifact_postprocess_action(step)?;
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
    let artifact_root = canonical_or_current_artifact_root(artifact_root)?;
    let trimmed = output.trim();
    let path = Path::new(trimmed);
    if trimmed.is_empty() || path.is_absolute() || path.components().any(disallowed_component) {
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

fn validate_artifact_postprocess_action(action: &ArtifactPostprocessAction) -> Result<()> {
    if let Some(id) = &action.id {
        validate_non_empty("artifact_postprocess.actions.id", id)?;
    }
    validate_non_empty("artifact_postprocess.actions.helper", &action.helper)?;
    validate_non_empty("artifact_postprocess.actions.action", &action.action)?;
    if let Some(input) = &action.input {
        validate_non_empty("artifact_postprocess.actions.input", input)?;
    }
    validate_relative_output_path(&action.output)
}

fn validate_relative_output_path(output: &str) -> Result<()> {
    let trimmed = output.trim();
    let path = Path::new(trimmed);
    if trimmed.is_empty() || path.is_absolute() || path.components().any(disallowed_component) {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.output",
            "artifact postprocess output must be a non-empty relative path confined to the artifact root",
            Some(output.to_string()),
            None,
        ));
    }
    Ok(())
}

fn validate_reviewer_ref(reviewer_ref: &ArtifactPostprocessReviewerRef) -> Result<()> {
    validate_non_empty(
        "artifact_postprocess.reviewer_refs.kind",
        &reviewer_ref.kind,
    )?;
    validate_non_empty(
        "artifact_postprocess.reviewer_refs.label",
        &reviewer_ref.label,
    )?;
    validate_non_empty("artifact_postprocess.reviewer_refs.url", &reviewer_ref.url)?;
    let url = reviewer_ref.url.trim();
    if url.starts_with('/')
        || url.starts_with("file:")
        || url.contains("localhost")
        || url.contains("127.0.0.1")
        || url.contains("[::1]")
    {
        return Err(Error::validation_invalid_argument(
            "artifact_postprocess.reviewer_refs.url",
            "reviewer-facing artifact refs must not use local-only URLs or filesystem paths",
            Some(reviewer_ref.url.clone()),
            None,
        ));
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "value cannot be empty",
            None,
            None,
        ));
    }
    Ok(())
}

fn canonical_or_current_artifact_root(artifact_root: &Path) -> Result<PathBuf> {
    if artifact_root.exists() {
        return artifact_root.canonicalize().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "canonicalize artifact root {}",
                    artifact_root.display()
                )),
            )
        });
    }
    let parent = artifact_root.parent().unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "canonicalize artifact root parent {}",
                parent.display()
            )),
        )
    })?;
    Ok(parent.join(artifact_root.file_name().unwrap_or_default()))
}

fn disallowed_component(component: Component<'_>) -> bool {
    matches!(
        component,
        Component::Prefix(_) | Component::RootDir | Component::ParentDir | Component::CurDir
    )
}

fn artifact_postprocess_plan_schema() -> String {
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string()
}

fn default_artifact_postprocess_required() -> bool {
    true
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
            let step: ArtifactPostprocessAction = serde_json::from_value(serde_json::json!({
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

    #[test]
    fn rejects_output_path_escape_before_helper_execution() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            let step: ArtifactPostprocessAction = serde_json::from_value(serde_json::json!({
                "id": "escape",
                "helper": "sh",
                "action": "-c",
                "output": "reports/../../escape.txt",
                "parameters": {
                    "args": ["printf should-not-run > /dev/null"]
                }
            }))
            .expect("step");

            let err = run_artifact_postprocess_steps(
                &[step],
                &ArtifactPostprocessContext {
                    artifact_root: &artifact_root,
                    input_root: None,
                    path_expander: None,
                },
            )
            .expect_err("path escape should fail");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("artifact_postprocess.output"));
        });
    }

    #[test]
    fn validates_and_describes_generic_plan_outputs() {
        let plan = ArtifactPostprocessPlan {
            schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
            plan_id: "plan-1".to_string(),
            artifact_roots: vec![ArtifactPostprocessRoot {
                id: "run-artifacts".to_string(),
                path: "runner-artifact://run/123/root".to_string(),
                persisted_ref: Some("runner-artifact://run/123/root".to_string()),
                manifest_path: Some("homeboy-artifact-manifest.json".to_string()),
            }],
            actions: vec![ArtifactPostprocessAction {
                id: Some("summary".to_string()),
                helper: "artifact-helper".to_string(),
                action: "summarize".to_string(),
                input: Some("${run.artifacts}/raw.json".to_string()),
                output: "summary/result.json".to_string(),
                parameters: BTreeMap::new(),
                required: true,
            }],
            reviewer_refs: vec![ArtifactPostprocessReviewerRef {
                kind: "artifact_index".to_string(),
                label: "Artifact index".to_string(),
                url: "https://artifacts.example.test/runs/123".to_string(),
            }],
            metadata: serde_json::json!({ "producer": "test" }),
        };

        let description = describe_artifact_postprocess_plan(&plan).expect("description");

        assert_eq!(description.schema, ARTIFACT_POSTPROCESS_PLAN_SCHEMA);
        assert_eq!(description.plan_id, "plan-1");
        assert_eq!(description.artifact_root_count, 1);
        assert_eq!(description.action_count, 1);
        assert_eq!(description.required_action_count, 1);
        assert_eq!(description.output_paths, vec!["summary/result.json"]);
    }

    #[test]
    fn rejects_local_only_reviewer_refs() {
        let plan = ArtifactPostprocessPlan {
            schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
            plan_id: "plan-1".to_string(),
            artifact_roots: vec![ArtifactPostprocessRoot {
                id: "run-artifacts".to_string(),
                path: "runner-artifact://run/123/root".to_string(),
                persisted_ref: None,
                manifest_path: None,
            }],
            actions: Vec::new(),
            reviewer_refs: vec![ArtifactPostprocessReviewerRef {
                kind: "artifact_index".to_string(),
                label: "Local artifact index".to_string(),
                url: "http://127.0.0.1:7350/runs/123".to_string(),
            }],
            metadata: serde_json::json!({}),
        };

        let err = validate_artifact_postprocess_plan(&plan).expect_err("local URL should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("reviewer_refs.url"));
    }
}
