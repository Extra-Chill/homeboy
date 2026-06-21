use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::{Error, Result};

pub const REPO_LOCAL_GATE_EXECUTION_KIND: &str = "repo_local_gate";
const LEGACY_NODE_SCRIPT_EXECUTION_KIND: &str = "node_script";
const GATE_ARTIFACT_KIND: &str = "repo_local_gate_output";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoLocalGateExecution {
    pub execution_kind: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub inputs: BTreeMap<String, Value>,
    pub artifact_outputs: BTreeMap<String, RepoLocalGateArtifactOutput>,
    pub env: BTreeMap<String, String>,
    pub artifact_root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoLocalGateArtifactOutput {
    #[serde(default, rename = "type", alias = "artifact_type")]
    pub artifact_type: Option<String>,
    #[serde(default, alias = "schema", alias = "artifact_schema")]
    pub artifact_schema: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

pub fn is_repo_local_gate_request(request: &AgentTaskRequest) -> bool {
    matches!(
        execution_kind(request).as_deref(),
        Some(REPO_LOCAL_GATE_EXECUTION_KIND) | Some(LEGACY_NODE_SCRIPT_EXECUTION_KIND)
    )
}

pub fn run_repo_local_gate_task(request: &AgentTaskRequest) -> AgentTaskOutcome {
    match build_execution(request).and_then(|execution| run_execution(request, execution)) {
        Ok(outcome) => outcome,
        Err(error) => gate_failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::InvalidInput,
            "agent_task.repo_local_gate.invalid_config",
            error.to_string(),
            Value::Null,
        ),
    }
}

fn build_execution(request: &AgentTaskRequest) -> Result<RepoLocalGateExecution> {
    let config = &request.executor.config;
    let execution_kind = execution_kind(request).ok_or_else(|| {
        Error::validation_invalid_argument(
            "execution_kind",
            "repo-local gate task requires execution_kind",
            Some(request.task_id.clone()),
            None,
        )
    })?;
    let workspace_root = request.workspace.root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.root",
            "repo-local gate task requires request.workspace.root",
            Some(request.task_id.clone()),
            None,
        )
    })?;
    let cwd = crate::core::resolve_contained_local_path(
        workspace_root,
        config.get("cwd").and_then(Value::as_str).unwrap_or("."),
        "cwd",
    )?;
    let argv = gate_argv(&cwd, config, &execution_kind)?;
    let inputs = object_field(config, "inputs")
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let artifact_outputs = parse_artifact_outputs(config)?;
    let env = object_field(config, "env")
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let artifact_root = config
        .get("artifact_root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("homeboy-repo-local-gates")
                .join(safe_path_segment(&request.task_id))
        });

    Ok(RepoLocalGateExecution {
        execution_kind,
        cwd,
        argv,
        inputs,
        artifact_outputs,
        env,
        artifact_root,
    })
}

fn gate_argv(cwd: &Path, config: &Value, execution_kind: &str) -> Result<Vec<String>> {
    if let Some(argv) = config.get("argv").and_then(Value::as_array) {
        let argv: Vec<String> = argv
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
        if argv.is_empty() {
            return Err(Error::validation_invalid_argument(
                "argv",
                "repo-local gate argv must contain at least one string",
                None,
                None,
            ));
        }
        return Ok(argv);
    }

    let Some(script) = config.get("script").and_then(Value::as_str) else {
        return Err(Error::validation_invalid_argument(
            "argv",
            "repo-local gate task requires argv or a relative script path",
            None,
            None,
        ));
    };
    let script_path = crate::core::resolve_contained_local_path(cwd, script, "script")?;
    if script_path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "script",
            "repo-local gate script must be a file path",
            Some(script.to_string()),
            None,
        ));
    }
    let script_arg = pathdiff(&script_path, cwd);
    let program = if execution_kind == LEGACY_NODE_SCRIPT_EXECUTION_KIND {
        "node"
    } else {
        config
            .get("interpreter")
            .and_then(Value::as_str)
            .unwrap_or("node")
    };
    Ok(vec![program.to_string(), script_arg])
}

fn run_execution(
    request: &AgentTaskRequest,
    execution: RepoLocalGateExecution,
) -> Result<AgentTaskOutcome> {
    fs::create_dir_all(&execution.artifact_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create repo-local gate artifact root {}",
                execution.artifact_root.display()
            )),
        )
    })?;
    let input_paths = materialize_inputs(&execution)?;
    let output_paths = materialize_output_paths(&execution)?;
    let mut command = Command::new(&execution.argv[0]);
    command
        .args(&execution.argv[1..])
        .current_dir(&execution.cwd);
    command.env(
        "HOMEBOY_REPO_LOCAL_GATE_INPUTS_JSON",
        serde_json::to_string(&execution.inputs).unwrap_or_else(|_| "{}".to_string()),
    );
    command.env(
        "HOMEBOY_REPO_LOCAL_GATE_OUTPUT_DIR",
        &execution.artifact_root,
    );
    for (key, path) in &input_paths {
        command.env(env_path_name(key), path);
    }
    for (key, path) in &output_paths {
        command.env(env_path_name(key), path);
    }
    for (key, value) in &execution.env {
        command.env(key, value);
    }

    let output = command.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run repo-local gate {}", execution.argv.join(" "))),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut diagnostics = vec![AgentTaskDiagnostic {
        class: "agent_task.repo_local_gate.executed".to_string(),
        message: format!("repo-local gate exited with code {exit_code}"),
        data: json!({
            "execution_kind": execution.execution_kind,
            "argv": execution.argv,
            "cwd": execution.cwd,
            "exit_code": exit_code,
            "stdout_tail": crate::core::agent_task_gate::text_tail(&stdout, 20),
            "stderr_tail": crate::core::agent_task_gate::text_tail(&stderr, 20),
        }),
    }];
    let (outputs, artifacts, typed_artifacts) =
        collect_outputs(&execution, &output_paths, &stdout)?;
    if !output.status.success() {
        return Ok(gate_failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::Provider,
            "agent_task.repo_local_gate.failed",
            format!("repo-local gate failed with exit code {exit_code}"),
            diagnostics.remove(0).data,
        ));
    }

    Ok(AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("repo-local deterministic gate passed".to_string()),
        failure_classification: None,
        artifacts,
        typed_artifacts,
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "repo-local-gate".to_string(),
            uri: execution.artifact_root.display().to_string(),
            label: Some("repo-local gate artifacts".to_string()),
        }],
        diagnostics,
        outputs,
        workflow: None,
        follow_up: None,
        metadata: json!({
            "execution_kind": REPO_LOCAL_GATE_EXECUTION_KIND,
            "artifact_root": execution.artifact_root,
        }),
    })
}

fn materialize_inputs(execution: &RepoLocalGateExecution) -> Result<BTreeMap<String, PathBuf>> {
    let input_dir = execution.artifact_root.join("inputs");
    fs::create_dir_all(&input_dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", input_dir.display())),
        )
    })?;
    let mut paths = BTreeMap::new();
    for (key, value) in &execution.inputs {
        let path = input_dir.join(format!("{}.json", safe_path_segment(key)));
        fs::write(
            &path,
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string()),
        )
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("write {}", path.display())))
        })?;
        paths.insert(key.clone(), path);
    }
    Ok(paths)
}

fn materialize_output_paths(
    execution: &RepoLocalGateExecution,
) -> Result<BTreeMap<String, PathBuf>> {
    let output_dir = execution.artifact_root.join("outputs");
    fs::create_dir_all(&output_dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", output_dir.display())),
        )
    })?;
    let mut paths = BTreeMap::new();
    for (key, declaration) in &execution.artifact_outputs {
        let path = declaration
            .path
            .as_deref()
            .map(|path| {
                crate::core::resolve_contained_local_path(
                    &execution.artifact_root,
                    path,
                    "artifact_outputs[].path",
                )
            })
            .transpose()?
            .unwrap_or_else(|| output_dir.join(format!("{}.json", safe_path_segment(key))));
        paths.insert(key.clone(), path);
    }
    Ok(paths)
}

fn collect_outputs(
    execution: &RepoLocalGateExecution,
    output_paths: &BTreeMap<String, PathBuf>,
    stdout: &str,
) -> Result<(Value, Vec<AgentTaskArtifact>, Vec<AgentTaskTypedArtifact>)> {
    let mut outputs = serde_json::Map::new();
    let mut artifacts = Vec::new();
    let mut typed_artifacts = Vec::new();
    if output_paths.is_empty() {
        let value = serde_json::from_str(stdout).unwrap_or_else(|_| json!({ "stdout": stdout }));
        outputs.insert("result".to_string(), value);
        return Ok((Value::Object(outputs), artifacts, typed_artifacts));
    }
    for (key, path) in output_paths {
        let declaration = execution
            .artifact_outputs
            .get(key)
            .cloned()
            .unwrap_or_default();
        let payload = fs::read_to_string(path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or(Value::Null);
        outputs.insert(key.clone(), payload.clone());
        let artifact = AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: key.clone(),
            kind: declaration
                .kind
                .clone()
                .unwrap_or_else(|| GATE_ARTIFACT_KIND.to_string()),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            label: None,
            role: None,
            semantic_key: None,
            path: Some(path.display().to_string()),
            url: None,
            mime: Some("application/json".to_string()),
            size_bytes: fs::metadata(path).ok().map(|metadata| metadata.len()),
            sha256: None,
            metadata: json!({ "execution_kind": REPO_LOCAL_GATE_EXECUTION_KIND }),
        };
        typed_artifacts.push(AgentTaskTypedArtifact {
            name: key.clone(),
            artifact_type: declaration.artifact_type.clone(),
            artifact_schema: declaration.artifact_schema.clone(),
            payload,
            artifact: Some(artifact.clone()),
            metadata: Value::Null,
        });
        artifacts.push(artifact);
    }
    Ok((Value::Object(outputs), artifacts, typed_artifacts))
}

fn parse_artifact_outputs(config: &Value) -> Result<BTreeMap<String, RepoLocalGateArtifactOutput>> {
    let Some(map) = object_field(config, "artifact_outputs") else {
        return Ok(BTreeMap::new());
    };
    let mut outputs = BTreeMap::new();
    for (key, value) in map {
        let declaration: RepoLocalGateArtifactOutput = serde_json::from_value(value.clone())
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "artifact_outputs",
                    error.to_string(),
                    Some(key.clone()),
                    None,
                )
            })?;
        outputs.insert(key.clone(), declaration);
    }
    Ok(outputs)
}

fn execution_kind(request: &AgentTaskRequest) -> Option<String> {
    request
        .executor
        .config
        .get("execution_kind")
        .or_else(|| request.executor.config.get("executionKind"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn object_field<'a>(value: &'a Value, field: &str) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(field).and_then(Value::as_object)
}

fn env_path_name(key: &str) -> String {
    format!("{}_PATH", key.to_ascii_uppercase().replace(['-', '.'], "_"))
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn pathdiff(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn gate_failure_outcome(
    request: &AgentTaskRequest,
    status: AgentTaskOutcomeStatus,
    classification: AgentTaskFailureClassification,
    diagnostic_class: &str,
    message: String,
    data: Value,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status,
        summary: Some(message.clone()),
        failure_classification: Some(classification),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: vec![AgentTaskDiagnostic {
            class: diagnostic_class.to_string(),
            message,
            data,
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AgentTaskWorkspaceMode,
    };

    #[test]
    fn repo_local_gate_materializes_inputs_and_typed_outputs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("gate.mjs");
        fs::write(
            &script,
            "import fs from 'node:fs'; const input = JSON.parse(fs.readFileSync(process.env.INPUT_PACKET_PATH, 'utf8')); fs.writeFileSync(process.env.GATE_RESULT_PATH, JSON.stringify({publish_allowed: input.ok}));",
        )
        .expect("write script");
        let request = request(
            temp.path(),
            json!({
                "execution_kind": "repo_local_gate",
                "script": "gate.mjs",
                "inputs": { "input_packet": { "ok": true } },
                "artifact_outputs": {
                    "gate_result": { "schema": "example/GateResult/v1", "type": "GateResult" }
                }
            }),
        );

        let outcome = run_repo_local_gate_task(&request);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.outputs["gate_result"]["publish_allowed"], true);
        assert_eq!(outcome.typed_artifacts.len(), 1);
        assert_eq!(outcome.typed_artifacts[0].name, "gate_result");
        assert_eq!(
            outcome.typed_artifacts[0].artifact_schema.as_deref(),
            Some("example/GateResult/v1")
        );
    }

    #[test]
    fn legacy_node_script_uses_node_without_shell() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("gate.mjs"),
            "import fs from 'node:fs'; fs.writeFileSync(process.env.RESULT_PATH, JSON.stringify({ok:true}));",
        )
        .expect("write script");
        let request = request(
            temp.path(),
            json!({
                "execution_kind": "node_script",
                "script": "gate.mjs",
                "artifact_outputs": { "result": { "schema": "example/Result/v1" } }
            }),
        );

        let execution = build_execution(&request).expect("execution");

        assert_eq!(execution.argv[0], "node");
        assert_eq!(execution.argv[1], "gate.mjs");
    }

    #[test]
    fn repo_local_gate_rejects_script_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let request = request(
            temp.path(),
            json!({
                "execution_kind": "repo_local_gate",
                "script": "../outside.mjs"
            }),
        );

        let error = build_execution(&request).expect_err("escape should fail");

        assert!(error.to_string().contains("script"));
    }

    fn request(root: &Path, config: Value) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: crate::core::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "gate-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "agent-task".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config,
            },
            instructions: String::new(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Existing,
                root: Some(root.display().to_string()),
                ..AgentTaskWorkspace::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }
    }
}
