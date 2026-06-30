#![cfg(test)]

use super::*;
use serde_json::{json, Value};

#[test]
fn request_round_trips_generic_agent_task_shape() {
    let request = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: "task-1".to_string(),
        group_key: Some("audit-batch".to_string()),
        parent_plan_id: Some("plan-1".to_string()),
        executor: AgentTaskExecutor {
            backend: "browser_sandbox".to_string(),
            selector: Some("lab-a".to_string()),
            runtime_selection: None,
            required_capabilities: vec!["structured_output".to_string()],
            secret_env: Vec::new(),
            model: Some("quality-model".to_string()),
            config: json!({ "account": "team-a" }),
        },
        instructions: "Fix the scoped finding and return artifacts.".to_string(),
        inputs: json!({ "finding_id": "finding-1" }),
        source_refs: vec![AgentTaskSourceRef {
            kind: "git".to_string(),
            uri: "https://example.test/repo.git".to_string(),
            revision: Some("abc123".to_string()),
        }],
        workspace: AgentTaskWorkspace {
            kind: None,
            mode: AgentTaskWorkspaceMode::Materialized,
            root: Some("/workspace/repo".to_string()),
            slug: Some("repo".to_string()),
            component_id: None,
            branch: None,
            base_ref: None,
            task_url: None,
            cleanup: None,
            materialization: json!({ "component": "repo" }),
        },
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy {
            read: "workspace".to_string(),
            write: "workspace".to_string(),
            apply: "propose_only".to_string(),
            tools: AgentToolPolicy::default(),
        },
        limits: AgentTaskLimits {
            timeout_ms: Some(300_000),
            max_runtime_ms: Some(240_000),
            max_output_bytes: Some(1_000_000),
        },
        expected_artifacts: vec!["patch".to_string(), "report".to_string()],
        artifact_declarations: vec![AgentTaskArtifactDeclaration {
            name: "analysis_report".to_string(),
            artifact_type: Some("AnalysisReport".to_string()),
            artifact_schema: Some("example/analysis-report/v1".to_string()),
            path: Some("artifacts/analysis-report.json".to_string()),
            required: true,
            description: Some("Structured analysis output".to_string()),
            metadata: json!({ "audience": "reviewer" }),
        }],
        metadata: json!({ "batch": 1 }),
    };

    let encoded = serde_json::to_string(&request).expect("serialize request");
    let decoded: AgentTaskRequest = serde_json::from_str(&encoded).expect("decode request");

    assert_eq!(decoded, request);
    assert_eq!(decoded.schema, AGENT_TASK_REQUEST_SCHEMA);
}

#[test]
fn agent_tool_contract_round_trips_policy_request_and_result() {
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
    let request = AgentToolRequest {
        schema: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
        request_id: "tool-request-1".to_string(),
        task_id: "task-1".to_string(),
        tool: "repo.status".to_string(),
        input: json!({ "path": "/workspace/repo" }),
        timeout_ms: Some(30_000),
        metadata: json!({ "attempt": 1 }),
    };
    let result = AgentToolResult {
        schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        tool: request.tool.clone(),
        status: AgentToolResultStatus::Succeeded,
        output: json!({ "clean": true }),
        diagnostics: Vec::new(),
        metadata: json!({ "execution_location": "control_plane" }),
    };

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
    assert_eq!(
        serde_json::from_value::<AgentToolRequest>(
            serde_json::to_value(&request).expect("serialize request")
        )
        .expect("decode request"),
        request
    );
    assert_eq!(
        serde_json::from_value::<AgentToolResult>(
            serde_json::to_value(&result).expect("serialize result")
        )
        .expect("decode result"),
        result
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
fn executor_runtime_selection_synthesizes_legacy_fields() {
    let executor = AgentTaskExecutor {
        backend: "sample-runtime".to_string(),
        selector: Some("claude-code".to_string()),
        runtime_selection: None,
        required_capabilities: Vec::new(),
        secret_env: Vec::new(),
        model: Some("opus-4.7".to_string()),
        config: json!({ "provider": "claude-code" }),
    };

    let selection = executor.runtime_selection();

    assert_eq!(selection.runtime_id, None);
    assert_eq!(
        selection.executor_backend.as_deref(),
        Some("sample-runtime")
    );
    assert_eq!(
        selection.executor_provider_id.as_deref(),
        Some("claude-code")
    );
    assert_eq!(selection.ai_provider_id.as_deref(), Some("claude-code"));
    assert_eq!(selection.model.as_deref(), Some("opus-4.7"));
    assert_eq!(selection.substrate_ref, None);
    assert_eq!(executor.executor_backend(), "sample-runtime");
    assert_eq!(executor.executor_provider_id(), Some("claude-code"));
    assert_eq!(executor.provider(), Some("claude-code"));
    assert_eq!(executor.model(), Some("opus-4.7"));
}

#[test]
fn executor_runtime_selection_round_trips_aliases() {
    let value = json!({
        "backend": "legacy-backend",
        "selector": "legacy-selector",
        "runtime": {
            "runtime_id": "runtime-a",
            "backend": "runtime-backend",
            "selector": "runtime-selector",
            "provider": "example-oauth",
            "model": "gpt-5.5",
            "substrate_ref": "sample-runtime://sandbox/123"
        }
    });

    let executor: AgentTaskExecutor = serde_json::from_value(value).expect("decode executor");
    let selection = executor.runtime_selection();
    let serialized = serde_json::to_value(&executor).expect("serialize executor");

    assert_eq!(executor.runtime_id(), Some("runtime-a"));
    assert_eq!(executor.executor_backend(), "runtime-backend");
    assert_eq!(executor.executor_provider_id(), Some("runtime-selector"));
    assert_eq!(executor.provider(), Some("example-oauth"));
    assert_eq!(executor.model(), Some("gpt-5.5"));
    assert_eq!(
        executor.substrate_ref(),
        Some("sample-runtime://sandbox/123")
    );
    assert_eq!(
        selection.executor_backend.as_deref(),
        Some("runtime-backend")
    );
    assert_eq!(
        selection.executor_provider_id.as_deref(),
        Some("runtime-selector")
    );
    assert_eq!(
        serialized["runtime_selection"]["executor_backend"],
        "runtime-backend"
    );
    assert_eq!(
        serialized["runtime_selection"]["executor_provider_id"],
        "runtime-selector"
    );
    assert_eq!(
        serialized["runtime_selection"]["ai_provider_id"],
        "example-oauth"
    );
    assert!(serialized.get("runtime").is_none());
}

#[test]
fn request_deserializes_legacy_expected_artifacts_and_declaration_alias() {
    let request: AgentTaskRequest = serde_json::from_value(json!({
        "schema": AGENT_TASK_REQUEST_SCHEMA,
        "task_id": "task-typed-artifacts",
        "executor": { "backend": "sample-runtime" },
        "instructions": "Return the declared typed report.",
        "expected_artifacts": ["legacy-report.json"],
        "artifactDeclarations": [{
            "name": "analysis_report",
            "type": "AnalysisReport",
            "artifact_schema": "example/analysis-report/v1",
            "path": "artifacts/analysis-report.json",
            "required": true
        }]
    }))
    .expect("decode request with typed artifact declarations");

    assert_eq!(request.expected_artifacts, vec!["legacy-report.json"]);
    assert_eq!(request.artifact_declarations.len(), 1);
    assert_eq!(request.artifact_declarations[0].name, "analysis_report");
    assert_eq!(
        request.artifact_declarations[0].artifact_type.as_deref(),
        Some("AnalysisReport")
    );
    assert!(request.artifact_declarations[0].required);
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
            "kind": "AnalysisReport",
            "contentSchema": "example/analysis-report/v1",
            "required": false
        }]
    }))
    .expect("decode request with artifact aliases");

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
fn outcome_round_trips_success_noop_timeout_and_follow_up_shapes() {
    let statuses = [
        AgentTaskOutcomeStatus::Succeeded,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskOutcomeStatus::UnableToRemediate,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskOutcomeStatus::Timeout,
        AgentTaskOutcomeStatus::FollowUpIssue,
    ];

    for status in statuses {
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            status,
            summary: Some("completed".to_string()),
            failure_classification: match status {
                AgentTaskOutcomeStatus::ProviderError => {
                    Some(AgentTaskFailureClassification::Provider)
                }
                AgentTaskOutcomeStatus::Timeout => Some(AgentTaskFailureClassification::Timeout),
                _ => None,
            },
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "artifact-1".to_string(),
                kind: "patch".to_string(),
                name: Some("fix.patch".to_string()),
                label: Some("Fix patch".to_string()),
                role: Some("patch".to_string()),
                semantic_key: Some("task.fix_patch".to_string()),
                path: Some("artifacts/fix.patch".to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: Some(128),
                sha256: Some("sha256:abc".to_string()),
                metadata: json!({}),
            }],
            typed_artifacts: vec![AgentTaskTypedArtifact {
                name: "issue_summary".to_string(),
                artifact_type: Some("IssueSummary".to_string()),
                artifact_schema: Some("example/issue-summary/v1".to_string()),
                payload: json!({ "issue_number": 3447 }),
                artifact: None,
                metadata: json!({}),
            }],
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "log".to_string(),
                uri: "artifact://run/log".to_string(),
                label: Some("runner log".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "provider".to_string(),
                message: "provider returned retryable error".to_string(),
                data: json!({}),
            }],
            outputs: json!({ "issue_number": 3447 }),
            workflow: None,
            follow_up: Some(AgentTaskFollowUp {
                kind: "issue_report".to_string(),
                title: "Needs human decision".to_string(),
                body: Some("The requested fix needs product direction.".to_string()),
                uri: None,
            }),
            metadata: json!({}),
        };

        let value = serde_json::to_value(&outcome).expect("serialize outcome");
        let decoded: AgentTaskOutcome = serde_json::from_value(value).expect("decode outcome");

        assert_eq!(decoded, outcome);
        assert_eq!(decoded.schema, AGENT_TASK_OUTCOME_SCHEMA);
        assert_eq!(decoded.artifacts[0].schema, AGENT_TASK_ARTIFACT_SCHEMA);
    }
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
fn outcome_round_trips_nested_workflow_step_evidence() {
    let outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "model-kimi-site-a".to_string(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some("diagnose step failed".to_string()),
        failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
        artifacts: vec![AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "screenshot-1".to_string(),
            kind: "screenshot".to_string(),
            name: Some("homepage.png".to_string()),
            label: None,
            role: None,
            semantic_key: None,
            path: Some("artifacts/homepage.png".to_string()),
            url: None,
            mime: Some("image/png".to_string()),
            size_bytes: Some(2048),
            sha256: Some("sha256:def".to_string()),
            metadata: json!({ "viewport": "desktop" }),
        }],
        typed_artifacts: vec![AgentTaskTypedArtifact {
            name: "screenshot_summary".to_string(),
            artifact_type: Some("ScreenshotSummary".to_string()),
            artifact_schema: Some("example/screenshot-summary/v1".to_string()),
            payload: json!({ "viewport": "desktop", "has_regression": true }),
            artifact: None,
            metadata: json!({ "source": "diagnose" }),
        }],
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: Some(AgentTaskWorkflowEvidence {
            schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
            id: "site-build".to_string(),
            label: Some("Site build".to_string()),
            steps: vec![
                AgentTaskWorkflowStepEvidence {
                    id: "generate".to_string(),
                    label: Some("Generate artifact".to_string()),
                    status: AgentTaskWorkflowStepStatus::Succeeded,
                    depends_on: Vec::new(),
                    started_at: Some("2026-05-31T23:00:00Z".to_string()),
                    finished_at: Some("2026-05-31T23:00:03Z".to_string()),
                    duration_ms: Some(3_000),
                    metrics: json!({ "tokens": 1200 }),
                    artifact_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    suggestions: Vec::new(),
                    metadata: json!({}),
                },
                AgentTaskWorkflowStepEvidence {
                    id: "diagnose".to_string(),
                    label: Some("Diagnose imported site".to_string()),
                    status: AgentTaskWorkflowStepStatus::Failed,
                    depends_on: vec!["generate".to_string(), "screenshot".to_string()],
                    started_at: Some("2026-05-31T23:00:04Z".to_string()),
                    finished_at: Some("2026-05-31T23:00:05Z".to_string()),
                    duration_ms: Some(1_000),
                    metrics: json!({ "fallback_blocks": 2 }),
                    artifact_refs: vec![AgentTaskEvidenceRef {
                        kind: "artifact".to_string(),
                        uri: "artifact://screenshot-1".to_string(),
                        label: Some("Desktop screenshot".to_string()),
                    }],
                    diagnostics: vec![AgentTaskDiagnostic {
                        class: "visual_regression".to_string(),
                        message: "fallback blocks remain".to_string(),
                        data: json!({ "count": 2 }),
                    }],
                    suggestions: vec![AgentTaskWorkflowStepSuggestion {
                        kind: "repair".to_string(),
                        title: "Run import repair".to_string(),
                        body: Some("Repair unsupported fallback blocks.".to_string()),
                        uri: Some("homeboy://tasks/model-kimi-site-a/repair".to_string()),
                    }],
                    metadata: json!({ "phase": "diagnostics" }),
                },
            ],
            metadata: json!({ "executor": "custom-provider" }),
        }),
        follow_up: None,
        metadata: json!({}),
    };

    let value = serde_json::to_value(&outcome).expect("serialize outcome");
    let decoded: AgentTaskOutcome = serde_json::from_value(value).expect("decode outcome");

    assert_eq!(decoded, outcome);
    let workflow = decoded.workflow.expect("workflow evidence");
    assert_eq!(workflow.schema, AGENT_TASK_WORKFLOW_SCHEMA);
    assert_eq!(
        workflow.steps[0].status,
        AgentTaskWorkflowStepStatus::Succeeded
    );
    assert_eq!(workflow.steps[1].depends_on, vec!["generate", "screenshot"]);
    assert_eq!(
        workflow.steps[1].artifact_refs[0].uri,
        "artifact://screenshot-1"
    );
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
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
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
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
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
        follow_up: None,
        outputs: json!({ "api_key": "secret-value", "safe": "value" }),
        metadata: json!({ "safe": "value", "password": "hunter2" }),
    };

    let redacted = serde_json::to_value(outcome.redacted()).expect("redacted json");

    assert!(!redacted.to_string().contains("hunter2"));
    assert!(!redacted.to_string().contains("abc123"));
    assert!(!redacted.to_string().contains("session=secret"));
    assert_eq!(redacted["metadata"]["safe"], json!("value"));
}

#[test]
fn executor_contract_round_trips_provider_neutral_lifecycle_shapes() {
    let capabilities = AgentTaskExecutorCapabilities {
        backend: "local_session".to_string(),
        selector: Some("default".to_string()),
        capabilities: vec!["workspace_write".to_string(), "artifacts".to_string()],
        supports_sync_completion: true,
        supports_async_polling: true,
        supports_streaming: true,
        supports_cancel: true,
    };

    let handle = AgentTaskExecutionHandle {
        kind: AgentTaskExecutionHandleKind::ProviderRun,
        task_id: "task-1".to_string(),
        backend: capabilities.backend.clone(),
        run_id: "run-1".to_string(),
        stream_uri: Some("event://run-1".to_string()),
        metadata: json!({ "attempt": 1 }),
    };

    let progress = AgentTaskProgress {
        handle,
        state: AgentTaskExecutionState::Running,
        events: vec![AgentTaskProgressEvent {
            kind: "log".to_string(),
            message: "started".to_string(),
            data: json!({ "sequence": 1 }),
        }],
        provider_payload: None,
        outcome: None,
    };

    let encoded = serde_json::to_value((&capabilities, &progress)).expect("serialize");
    let (decoded_capabilities, decoded_progress): (
        AgentTaskExecutorCapabilities,
        AgentTaskProgress,
    ) = serde_json::from_value(encoded).expect("decode");

    assert_eq!(decoded_capabilities, capabilities);
    assert_eq!(decoded_progress, progress);
    assert!(!decoded_progress.state.is_terminal());
}
