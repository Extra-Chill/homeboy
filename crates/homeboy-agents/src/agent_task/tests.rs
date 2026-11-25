#![cfg(test)]

use super::*;
use serde_json::json;

#[test]
fn agent_tool_policy_resolves_execution_location_per_tool() {
    let policy: AgentToolPolicy = serde_json::from_value(json!({
        "schema": AGENT_TOOL_POLICY_SCHEMA,
        "default_location": "disabled",
        "tools": {
            "repo.status": {
                "execution_location": "control_plane",
                "timeout_ms": 30_000,
                "reason": "controller owns this credential boundary"
            },
            "format.check": {
                "execution_location": "runner"
            }
        }
    }))
    .expect("decode tool policy");
    assert_eq!(
        policy.execution_location_for("repo.status"),
        AgentToolExecutionLocation::ControlPlane
    );
    assert_eq!(
        policy.execution_location_for("format.check"),
        AgentToolExecutionLocation::Runner
    );
    assert_eq!(
        policy.execution_location_for("unknown.tool"),
        AgentToolExecutionLocation::Disabled
    );
}

#[test]
fn agent_tool_evidence_redaction_removes_sensitive_values() {
    let request = AgentToolRequest {
        schema: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
        request_id: "tool-request-secret".to_string(),
        task_id: "task-secret".to_string(),
        tool: "repo.status".to_string(),
        input: json!({ "authorization": "Bearer abc123", "safe": "value" }),
        timeout_ms: None,
        metadata: json!({ "refresh_token": "secret-refresh" }),
    };
    let result = AgentToolResult {
        schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        tool: request.tool.clone(),
        status: AgentToolResultStatus::Failed,
        output: json!({ "api_key": "secret-value", "safe": "value" }),
        diagnostics: vec![AgentTaskDiagnostic {
            class: "tool".to_string(),
            message: "Authorization: Bearer abc123".to_string(),
            data: json!({ "password": "hunter2" }),
        }],
        metadata: json!({ "client_secret": "secret" }),
    };

    let redacted_request = serde_json::to_value(request.redacted()).expect("request json");
    let redacted_result = serde_json::to_value(result.redacted()).expect("result json");

    assert!(!redacted_request.to_string().contains("abc123"));
    assert!(!redacted_request.to_string().contains("secret-refresh"));
    assert_eq!(redacted_request["input"]["safe"], json!("value"));
    assert!(!redacted_result.to_string().contains("secret-value"));
    assert!(!redacted_result.to_string().contains("hunter2"));
    assert!(!redacted_result.to_string().contains("abc123"));
    assert_eq!(redacted_result["output"]["safe"], json!("value"));
}

#[test]
fn request_canonicalizes_artifact_declarations_from_expected_artifacts() {
    let mut request: AgentTaskRequest = serde_json::from_value(json!({
        "schema": AGENT_TASK_REQUEST_SCHEMA,
        "task_id": "task-artifact-normalization",
        "executor": { "backend": "sample-runtime" },
        "instructions": "Return artifacts.",
        "expected_artifacts": [" patch ", "analysis_report", ""],
        "artifact_declarations": [{
            "name": " analysis_report ",
            "type": "AnalysisReport",
            "artifact_schema": "example/analysis-report/v1",
            "required": false
        }]
    }))
    .expect("decode request with artifact declarations");

    request.normalize_artifact_declarations();

    assert_eq!(request.artifact_declarations.len(), 2);
    assert_eq!(request.artifact_declarations[0].name, "analysis_report");
    assert_eq!(
        request.artifact_declarations[0].artifact_type.as_deref(),
        Some("AnalysisReport")
    );
    assert_eq!(
        request.artifact_declarations[0].artifact_schema.as_deref(),
        Some("example/analysis-report/v1")
    );
    assert!(!request.artifact_declarations[0].required);
    assert_eq!(request.artifact_declarations[1].name, "patch");
    assert!(request.artifact_declarations[1].required);
}

#[test]
fn artifact_role_helpers_fall_back_to_metadata() {
    let artifact = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "artifact-1".to_string(),
        kind: "json".to_string(),
        name: Some("result.json".to_string()),
        label: None,
        role: None,
        semantic_key: None,
        path: Some("artifacts/result.json".to_string()),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: None,
        sha256: None,
        metadata: json!({
            "role": "summary",
            "semantic_key": "task.summary"
        }),
    };

    assert_eq!(artifact.display_label(), Some("result.json"));
    assert_eq!(artifact.declared_role(), Some("summary"));
    assert_eq!(artifact.declared_semantic_key(), Some("task.summary"));
}

#[test]
fn redacted_request_removes_sensitive_fields() {
    let request = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: "task-secret".to_string(),
        group_key: None,
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "cli_agent".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: json!({ "api_key": "secret-value" }),
        },
        instructions: "Use token=abc123 while testing.".to_string(),
        inputs: json!({ "authorization": "Bearer abc123", "safe": "value" }),
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        output_declarations: Vec::new(),
        runtime_tools: Vec::new(),
        metadata: json!({ "refresh_token": "secret-refresh" }),
    };

    let redacted = serde_json::to_value(request.redacted()).expect("redacted json");

    assert!(!redacted.to_string().contains("secret-value"));
    assert!(!redacted.to_string().contains("abc123"));
    assert!(!redacted.to_string().contains("secret-refresh"));
    assert_eq!(redacted["inputs"]["safe"], json!("value"));
}

#[test]
fn redacted_outcome_removes_sensitive_artifact_and_diagnostic_data() {
    let outcome = AgentTaskOutcome {
        task_id: "task-secret".to_string(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some("failed with password=hunter2".to_string()),
        failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
        artifacts: vec![AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "log".to_string(),
            kind: "log".to_string(),
            name: None,
            label: Some("secret log".to_string()),
            role: Some("log".to_string()),
            semantic_key: Some("secret.log".to_string()),
            path: None,
            url: Some("https://example.test/log?token=abc123".to_string()),
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: json!({ "cookie": "session=secret" }),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "provider".to_string(),
            message: "Authorization: Bearer abc123".to_string(),
            data: json!({ "client_secret": "secret" }),
        }],
        workflow: Some(AgentTaskWorkflowEvidence {
            schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
            id: "secret-workflow".to_string(),
            label: Some("Use token=abc123".to_string()),
            steps: vec![AgentTaskWorkflowStepEvidence {
                id: "diagnose".to_string(),
                label: Some("Inspect password=hunter2".to_string()),
                status: AgentTaskWorkflowStepStatus::Failed,
                depends_on: Vec::new(),
                started_at: None,
                finished_at: None,
                duration_ms: None,
                metrics: json!({ "api_key": "secret-value" }),
                artifact_refs: Vec::new(),
                diagnostics: vec![AgentTaskDiagnostic {
                    class: "workflow".to_string(),
                    message: "Authorization: Bearer abc123".to_string(),
                    data: json!({ "password": "hunter2" }),
                }],
                suggestions: vec![AgentTaskWorkflowStepSuggestion {
                    kind: "repair".to_string(),
                    title: "Use token=abc123".to_string(),
                    body: Some("password=hunter2".to_string()),
                    uri: Some("https://example.test/repair?token=abc123".to_string()),
                }],
                metadata: json!({ "refresh_token": "secret-refresh" }),
            }],
            metadata: json!({ "client_secret": "secret" }),
        }),
        outputs: json!({ "api_key": "secret-value", "safe": "value" }),
        metadata: json!({ "safe": "value", "password": "hunter2" }),
        ..Default::default()
    };

    let redacted = serde_json::to_value(outcome.redacted()).expect("redacted json");

    assert!(!redacted.to_string().contains("hunter2"));
    assert!(!redacted.to_string().contains("abc123"));
    assert!(!redacted.to_string().contains("session=secret"));
    assert_eq!(redacted["metadata"]["safe"], json!("value"));
}
