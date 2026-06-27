use super::common::request;
use super::*;

#[test]
fn provider_runner_secret_env_contracts_are_applied_to_selected_plan_tasks() {
    let (mut request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    request_a.executor.backend = "provider-a".to_string();
    provider_a.backend = "provider-a".to_string();
    provider_a.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "provider-a.auth".to_string(),
        label: "Provider A auth".to_string(),
        secret_env: vec!["PROVIDER_A_TOKEN".to_string()],
        env_path: None,
        executable: None,
        remediation: Some("Configure provider A auth.".to_string()),
        extra: BTreeMap::new(),
    }];
    let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    request_b.executor.backend = "provider-b".to_string();
    request_b.executor.secret_env = vec!["EXPLICIT_SECRET".to_string()];
    provider_b.backend = "provider-b".to_string();
    provider_b.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "provider-b.auth".to_string(),
        label: "Provider B auth".to_string(),
        secret_env: vec![
            "PROVIDER_B_TOKEN".to_string(),
            "EXPLICIT_SECRET".to_string(),
        ],
        env_path: None,
        executable: None,
        remediation: None,
        extra: BTreeMap::new(),
    }];
    let mut plan = AgentTaskPlan::new("plan-a", vec![request_a, request_b]);

    apply_provider_runner_secret_env_contracts_with_providers(&mut plan, &[provider_a, provider_b]);

    assert_eq!(
        plan.tasks[0].executor.secret_env,
        vec!["PROVIDER_A_TOKEN".to_string()]
    );
    assert_eq!(
        plan.tasks[1].executor.secret_env,
        vec![
            "EXPLICIT_SECRET".to_string(),
            "PROVIDER_B_TOKEN".to_string()
        ]
    );
}

#[test]
fn discovers_agent_task_providers_from_agent_runtime_manifests() {
    crate::test_support::with_isolated_home(|home| {
        let runtime_dir = home
            .path()
            .join(".config/homeboy/agent-runtimes/custom-runtime");
        fs::create_dir_all(&runtime_dir).expect("runtime dir");
        fs::write(
            runtime_dir.join("custom-runtime.json"),
            serde_json::to_string(&json!({
                "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                "id": "custom-runtime",
                "name": "Custom Runtime",
                "version": "1.0.0",
                "agent_task_executors": [{
                    "schema": "homeboy/agent-task-executor-provider/v1",
                    "id": "custom.runtime.executor",
                    "backend": "custom",
                    "command": "node {{runtime_path}}/runner.cjs",
                    "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                    "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                }]
            }))
            .unwrap(),
        )
        .expect("runtime manifest");

        let providers = discover_agent_task_executor_providers();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "custom.runtime.executor");
        assert_eq!(providers[0].runtime_id.as_deref(), Some("custom-runtime"));
        assert_eq!(
            providers[0].runtime_path.as_deref(),
            Some(runtime_dir.to_string_lossy().as_ref())
        );
        assert_eq!(
            render_provider_command_display(&providers[0]),
            format!("node {}/runner.cjs", runtime_dir.display())
        );
    });
}

#[test]
fn provider_command_env_includes_runtime_identity() {
    let (request, mut provider) =
        request("task-a", "node {{runtime_path}}/provider.js".to_string());
    provider.runtime_id = Some("custom-runtime".to_string());
    provider.runtime_path = Some("/tmp/custom-runtime".to_string());

    let env = provider_command_env(&request, &provider).expect("provider env");

    assert!(env.contains(&(
        "HOMEBOY_AGENT_RUNTIME_ID".to_string(),
        "custom-runtime".to_string()
    )));
    assert!(env.contains(&(
        "HOMEBOY_AGENT_RUNTIME_PATH".to_string(),
        "/tmp/custom-runtime".to_string()
    )));
    assert_eq!(
        render_provider_command_display(&provider),
        "node /tmp/custom-runtime/provider.js"
    );
}

#[test]
fn provider_command_env_injects_declared_executable_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tool = temp.path().join("provider-tool");
    fs::write(&tool, "#!/bin/sh\nexit 0\n").expect("write tool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("chmod tool");
    }
    let (request, mut provider) = request("task-a", "node provider.js".to_string());
    provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "provider.tool".to_string(),
        label: "Provider tool".to_string(),
        secret_env: Vec::new(),
        env_path: None,
        executable: Some(AgentTaskProviderExecutableReadiness {
            env: vec!["HOMEBOY_TEST_PROVIDER_TOOL".to_string()],
            candidates: vec![tool.to_string_lossy().to_string()],
            version_command: vec!["--version".to_string()],
            install_hint: Some("Install provider-tool.".to_string()),
            extra: BTreeMap::new(),
        }),
        remediation: None,
        extra: BTreeMap::new(),
    }];
    std::env::remove_var("HOMEBOY_TEST_PROVIDER_TOOL");

    let env = provider_command_env(&request, &provider).expect("provider env");

    assert!(env.contains(&(
        "HOMEBOY_TEST_PROVIDER_TOOL".to_string(),
        tool.to_string_lossy().to_string()
    )));
}

#[test]
fn provider_command_env_prefers_declared_executable_env_value() {
    let (request, mut provider) = request("task-a", "node provider.js".to_string());
    provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "provider.tool".to_string(),
        label: "Provider tool".to_string(),
        secret_env: Vec::new(),
        env_path: None,
        executable: Some(AgentTaskProviderExecutableReadiness {
            env: vec!["HOMEBOY_TEST_PROVIDER_TOOL_ENV".to_string()],
            candidates: vec!["definitely-missing-provider-tool".to_string()],
            version_command: Vec::new(),
            install_hint: None,
            extra: BTreeMap::new(),
        }),
        remediation: None,
        extra: BTreeMap::new(),
    }];
    std::env::set_var("HOMEBOY_TEST_PROVIDER_TOOL_ENV", "/custom/provider-tool");

    let env = provider_command_env(&request, &provider).expect("provider env");

    std::env::remove_var("HOMEBOY_TEST_PROVIDER_TOOL_ENV");
    assert!(env.contains(&(
        "HOMEBOY_TEST_PROVIDER_TOOL_ENV".to_string(),
        "/custom/provider-tool".to_string()
    )));
}

#[test]
fn provider_outcome_roles_normalize_from_declared_aliases() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.role_aliases = serde_json::from_value(json!({
        "artifact_kinds": {
            "patch": ["custom-patch"]
        },
        "outputs": {
            "provider_run_result": ["custom_run_result"]
        }
    }))
    .expect("role aliases");
    let patch_path = std::env::temp_dir().join("custom.patch");
    fs::write(&patch_path, "diff --git a/a b/a\n").expect("patch");
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-a".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
        failure_classification: None,
        artifacts: vec![fixture_artifact(
            "patch",
            "custom-patch",
            &patch_path,
            Some("text/x-patch"),
        )],
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: json!({
            "custom_run_result": {
                "run_id": "custom-run-1"
            }
        }),
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert_eq!(outcome.artifacts[0].kind, "patch");
    assert_eq!(
        outcome.artifacts[0].metadata["provider_kind"],
        "custom-patch"
    );
    assert_eq!(
        outcome.outputs["provider_run_result"]["run_id"],
        "custom-run-1"
    );
    assert_eq!(
        outcome.outputs["custom_run_result"]["run_id"],
        "custom-run-1"
    );
}

#[test]
fn declared_sandbox_result_contract_rejects_private_runtime_result_shape() {
    let (_, mut provider) = request("task-sandbox-private", "node provider.js".to_string());
    provider.backend = "runtime-provider".to_string();
    provider.result_contract = sandbox_result_contract();
    let mut outcome = failed_outcome_with_run_result(json!({
        "agent_result": { "status": "succeeded" },
        "metadata": { "agent_runtime": { "id": "runtime-private" } }
    }));
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "sandbox.public_result_envelope_missing")
        .expect("missing public envelope diagnostic");
    assert_eq!(diagnostic.data["private_shape_detected"], true);
    assert!(outcome.typed_artifacts.is_empty());
}

#[test]
fn declared_sandbox_result_contract_consumes_public_typed_artifacts() {
    let (_, mut provider) = request("task-sandbox-public", "node provider.js".to_string());
    provider.backend = "runtime-provider".to_string();
    provider.result_contract = sandbox_result_contract();
    let mut outcome = failed_outcome_with_run_result(json!({
        "schema": "sample-runtime/artifact-result-envelope/v1",
        "status": "succeeded",
        "typed_artifacts": [{
            "name": "agent_result",
            "type": "agent_result",
            "artifact_schema": "sample-runtime/agent-result/v1",
            "payload": { "summary": "created patch" }
        }]
    }));
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome.diagnostics.is_empty());
    assert_eq!(outcome.typed_artifacts.len(), 1);
    assert_eq!(outcome.typed_artifacts[0].name, "agent_result");
    assert_eq!(
        outcome.typed_artifacts[0].artifact_schema.as_deref(),
        Some("sample-runtime/agent-result/v1")
    );
    assert_eq!(
        outcome.typed_artifacts[0].payload["summary"],
        "created patch"
    );
}

#[test]
fn declared_sandbox_result_contract_reports_missing_typed_artifacts() {
    let (_, mut provider) = request("task-sandbox-empty", "node provider.js".to_string());
    provider.backend = "runtime-provider".to_string();
    provider.result_contract = sandbox_result_contract();
    let mut outcome = failed_outcome_with_run_result(json!({
        "schema": "sample-runtime/artifact-result-envelope/v1",
        "status": "succeeded",
        "typed_artifacts": []
    }));
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "sandbox.public_result_typed_artifacts_missing"
            && diagnostic.message.contains("typed artifacts")
    }));
}

#[test]
fn unknown_provider_without_result_contract_keeps_generic_outcome() {
    let (_, provider) = request("task-generic-provider", "node provider.js".to_string());
    let mut outcome = failed_outcome_with_run_result(json!({
        "agent_result": { "status": "succeeded" },
        "metadata": { "agent_runtime": { "id": "runtime-private" } }
    }));
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(outcome.failure_classification, None);
    assert!(outcome.diagnostics.is_empty());
    assert!(outcome.typed_artifacts.is_empty());
}

fn sandbox_result_contract() -> AgentTaskProviderResultContract {
    serde_json::from_value(json!({
        "typed_artifact_envelope": {
            "schema": "sample-runtime/artifact-result-envelope/v1",
            "output": "provider_run_result",
            "provider_label": "Managed Sandbox",
            "diagnostic_class_prefix": "sandbox",
            "private_shape_markers": ["agent_result", "metadata.agent_runtime"],
            "require_typed_artifacts": true
        }
    }))
    .expect("sandbox result contract")
}

fn failed_outcome_with_run_result(run_result: Value) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "cook-conductor".to_string(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some("Provider agent task failed.".to_string()),
        failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: json!({ "provider_run_result": run_result }),
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[test]
fn empty_failed_run_result_surfaces_explanatory_diagnostic() {
    // Mirrors the #4105 repro: a failed run-result that is an empty shell.
    let mut outcome = failed_outcome_with_run_result(json!({
        "schema": "example-provider/agent-task-run-result/v1",
        "status": "failed",
        "failure_classification": "runtime",
        "artifacts": [],
        "diagnostics": [],
        "metadata": {
            "provider_error": {},
            "run_id": "",
            "run_status": "",
            "runtime_id": "",
            "runtime_status": ""
        },
        "refs": {
            "artifact_bundles": [],
            "changed_files": [],
            "logs": [],
            "patches": [],
            "runtimes": [],
            "transcripts": []
        }
    }));

    surface_provider_run_result_diagnostics(&mut outcome);

    assert_eq!(
        outcome.diagnostics.len(),
        1,
        "an empty failed run-result must still produce one reviewer-safe diagnostic"
    );
    assert_eq!(outcome.diagnostics[0].class, "provider.run_result_empty");
    assert!(outcome.diagnostics[0]
        .message
        .contains("no provider runtime or session"));
}

#[test]
fn populated_failed_run_result_surfaces_error_identity_and_refs() {
    let mut outcome = failed_outcome_with_run_result(json!({
        "schema": "example-provider/agent-task-run-result/v1",
        "status": "failed",
        "diagnostics": [
            { "class": "provider.api_error", "message": "runtime provisioning rejected" }
        ],
        "metadata": {
            "provider_error": { "code": "E_RUNTIME", "message": "quota exceeded" },
            "run_id": "run-123",
            "run_status": "errored",
            "runtime_id": "rt-456",
            "runtime_status": "failed"
        },
        "refs": {
            "logs": ["https://provider.example/logs/run-123"],
            "transcripts": [{ "uri": "https://provider.example/transcripts/rt-456" }],
            "artifact_bundles": []
        }
    }));

    surface_provider_run_result_diagnostics(&mut outcome);

    // The provider's own diagnostic is lifted up.
    assert!(outcome
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "provider.api_error"));
    // The structured identity becomes an actionable diagnostic.
    let identity = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "provider.run_result_failed")
        .expect("identity diagnostic surfaced");
    assert!(identity.message.contains("quota exceeded"));
    assert!(identity.message.contains("run_id=run-123"));
    assert!(identity.message.contains("runtime_id=rt-456"));
    // provider_error is mirrored onto outcome metadata.
    assert_eq!(
        outcome.metadata["provider_error"]["code"],
        json!("E_RUNTIME")
    );
    // Log + transcript refs become evidence refs.
    assert!(outcome
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "provider-log"
            && reference.uri == "https://provider.example/logs/run-123"));
    assert!(outcome
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "provider-transcript"
            && reference.uri == "https://provider.example/transcripts/rt-456"));
    // The empty-shell guard must NOT fire when real evidence exists.
    assert!(outcome
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.class != "provider.run_result_empty"));
}

#[test]
fn succeeded_run_result_is_not_mined_for_failure_diagnostics() {
    let mut outcome = failed_outcome_with_run_result(json!({
        "status": "succeeded",
        "metadata": { "run_id": "run-999" }
    }));
    // Even though the outcome status is failed, a succeeded run-result is
    // left untouched (the failure cause is elsewhere).
    surface_provider_run_result_diagnostics(&mut outcome);
    assert!(outcome.diagnostics.is_empty());
}

#[test]
fn non_failure_outcome_skips_run_result_mining() {
    let mut outcome = failed_outcome_with_run_result(json!({
        "status": "failed",
        "metadata": { "provider_error": { "message": "boom" } }
    }));
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    surface_provider_run_result_diagnostics(&mut outcome);
    assert!(outcome.diagnostics.is_empty());
    assert!(outcome.metadata.get("provider_error").is_none());
}
