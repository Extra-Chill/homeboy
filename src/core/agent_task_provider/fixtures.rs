use super::command_runner::failure_outcome;
use super::*;

pub(super) fn run_fixture_provider(request: &AgentTaskRequest) -> AgentTaskOutcome {
    let mode = request
        .executor
        .config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("success");
    let artifact_root = fixture_artifact_root(request);
    if let Err(error) = std::fs::create_dir_all(&artifact_root) {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.fixture_artifact_root_failed",
            error.to_string(),
            json!({ "artifact_root": artifact_root.display().to_string() }),
        );
    }

    match mode {
        "empty_patch" => fixture_empty_patch_outcome(request, &artifact_root),
        "empty_runtime_bundle" => fixture_empty_runtime_bundle_outcome(request, &artifact_root),
        _ => fixture_success_outcome(request, &artifact_root),
    }
}

fn fixture_artifact_root(request: &AgentTaskRequest) -> PathBuf {
    request
        .executor
        .config
        .get("artifact_root")
        .and_then(Value::as_str)
        .map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            }
        })
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("homeboy-agent-task-fixture-{}", request.task_id))
        })
}

fn fixture_success_outcome(request: &AgentTaskRequest, artifact_root: &Path) -> AgentTaskOutcome {
    let changed_file = request
        .executor
        .config
        .get("changed_file")
        .and_then(Value::as_str)
        .unwrap_or("docs/agent-task-smoke.md");
    let patch_path = artifact_root.join("changes.patch");
    let transcript_path = artifact_root.join("transcript.log");
    let result_path = artifact_root.join("agent-result.json");
    let patch = format!(
        "diff --git a/{changed_file} b/{changed_file}\n--- a/{changed_file}\n+++ b/{changed_file}\n@@ -1 +1 @@\n-before\n+after\n"
    );
    let _ = std::fs::write(&patch_path, patch);
    let _ = std::fs::write(
        &transcript_path,
        "fixture provider completed successfully\n",
    );

    let metadata = request
        .executor
        .config
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({ "fixture_mode": "success" }));
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("fixture provider wrote deterministic smoke artifacts".to_string()),
        failure_classification: None,
        artifacts: vec![
            fixture_artifact("patch", "patch", &patch_path, Some("text/x-patch")),
            fixture_artifact(
                "agent-result",
                "agent_result",
                &result_path,
                Some("application/json"),
            ),
        ],
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: transcript_path.display().to_string(),
            label: Some("fixture transcript".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.fixture_success".to_string(),
            message: "fixture provider generated patch, transcript, and result artifacts"
                .to_string(),
            data: json!({ "artifact_root": artifact_root.display().to_string() }),
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata,
    };
    let _ = std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string()),
    );
    outcome.artifacts[1] = fixture_artifact(
        "agent-result",
        "agent_result",
        &result_path,
        Some("application/json"),
    );
    outcome
}

fn fixture_empty_patch_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let patch_path = artifact_root.join("empty.patch");
    let _ = std::fs::write(&patch_path, "");
    failure_outcome(
        request,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskFailureClassification::InvalidInput,
        "agent_task.fixture_empty_patch",
        "fixture produced an empty patch; promotion should reject it".to_string(),
        json!({ "patch_path": patch_path.display().to_string() }),
    )
}

fn fixture_empty_runtime_bundle_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let bundle_path = artifact_root.join("runtime-bundle");
    let _ = std::fs::create_dir_all(&bundle_path);
    let mut outcome = failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Provider,
        "agent_task.fixture_empty_runtime_bundle",
        "fixture produced an empty runtime bundle".to_string(),
        json!({ "bundle_path": bundle_path.display().to_string() }),
    );
    outcome.artifacts.push(fixture_artifact(
        "runtime-bundle",
        "runtime_bundle",
        &bundle_path,
        None,
    ));
    outcome
}

pub(super) fn fixture_artifact(
    id: &str,
    kind: &str,
    path: &PathBuf,
    mime: Option<&str>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string()),
        label: None,
        role: None,
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: mime.map(str::to_string),
        size_bytes: std::fs::metadata(path).ok().map(|metadata| metadata.len()),
        sha256: None,
        metadata: json!({ "fixture": true }),
    }
}
