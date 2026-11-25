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
