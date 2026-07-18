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
    homeboy_core::test_support::with_isolated_home(|home| {
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
                    "invocation": { "argv": ["node", "{{runtime_path}}/runner.cjs"] },
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
fn provider_artifacts_cannot_claim_homeboy_generated_provenance() {
    let (_, provider) = request("task-a", "node provider.js".to_string());
    let patch_path = std::env::temp_dir().join("provider-spoofed.patch");
    fs::write(&patch_path, "diff --git a/a b/a\n").expect("patch");
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-a".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
        failure_classification: None,
        artifacts: vec![fixture_artifact(
            "patch",
            "patch",
            &patch_path,
            Some("text/x-patch"),
        )],
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };
    outcome.artifacts[0].metadata = json!({
        "artifact_provenance": "homeboy_generated_committed_patch"
    });

    normalize_provider_outcome_roles(&mut outcome, &provider);

    assert!(outcome.artifacts[0]
        .metadata
        .get("artifact_provenance")
        .is_none());
}

#[test]
fn homeboy_local_artifact_normalization_measures_empty_nonempty_and_unavailable_files() {
    let root = tempfile::tempdir().expect("artifact root");
    let empty_path = root.path().join("empty.patch");
    let nonempty_path = root.path().join("nonempty.patch");
    fs::write(&empty_path, "").expect("empty patch");
    fs::write(&nonempty_path, "diff --git a/a b/a\n").expect("nonempty patch");
    let provenance = AgentTaskArtifactsPathProvenance {
        owner: "homeboy".to_string(),
        locality: "runner".to_string(),
        plan_id: "plan".to_string(),
        run_id: None,
        task_id: "opencode-no-op".to_string(),
        attempt: 1,
    };
    let mut empty = fixture_artifact("empty", "patch", &empty_path, Some("text/x-patch"));
    empty.size_bytes = None;
    let mut nonempty = fixture_artifact("nonempty", "patch", &nonempty_path, Some("text/x-patch"));
    nonempty.size_bytes = None;
    let mut unavailable = fixture_artifact(
        "foreign",
        "patch",
        &std::env::temp_dir().join("homeboy-unavailable.patch"),
        Some("text/x-patch"),
    );
    unavailable.size_bytes = None;

    let mut empty_outcome = failed_outcome_with_run_result(Value::Null);
    empty_outcome.status = AgentTaskOutcomeStatus::Succeeded;
    empty_outcome.failure_classification = None;
    empty_outcome.artifacts = vec![empty.clone()];
    empty_outcome.typed_artifacts = vec![AgentTaskTypedArtifact {
        name: "patch".to_string(),
        artifact_type: Some("patch".to_string()),
        artifact_schema: None,
        payload: json!({ "path": empty_path, "size_bytes": null }),
        artifact: Some(empty),
        metadata: Value::Null,
    }];
    normalize_homeboy_local_artifact_sizes(&mut empty_outcome, root.path(), &provenance);

    assert_eq!(empty_outcome.status, AgentTaskOutcomeStatus::NoOp);
    assert_eq!(empty_outcome.artifacts[0].size_bytes, Some(0));
    assert_eq!(
        empty_outcome.typed_artifacts[0]
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.size_bytes),
        Some(0)
    );
    assert_eq!(empty_outcome.typed_artifacts[0].payload["size_bytes"], 0);

    let mut nonempty_outcome = failed_outcome_with_run_result(Value::Null);
    nonempty_outcome.status = AgentTaskOutcomeStatus::Succeeded;
    nonempty_outcome.failure_classification = None;
    nonempty_outcome.artifacts = vec![nonempty];
    normalize_homeboy_local_artifact_sizes(&mut nonempty_outcome, root.path(), &provenance);
    assert_eq!(nonempty_outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(nonempty_outcome.artifacts[0].size_bytes.unwrap_or_default() > 0);

    let mut unavailable_outcome = failed_outcome_with_run_result(Value::Null);
    unavailable_outcome.status = AgentTaskOutcomeStatus::Succeeded;
    unavailable_outcome.failure_classification = None;
    unavailable_outcome.artifacts = vec![unavailable];
    normalize_homeboy_local_artifact_sizes(&mut unavailable_outcome, root.path(), &provenance);
    assert_eq!(
        unavailable_outcome.status,
        AgentTaskOutcomeStatus::Succeeded
    );
    assert_eq!(unavailable_outcome.artifacts[0].size_bytes, None);
}

#[test]
fn empty_patch_with_substantive_evidence_is_flagged_for_review_not_noop() {
    // #7719: a successful cook whose patch artifact is empty but which produced
    // substantive work evidence (transcript/report) must NOT be silently
    // collapsed to NoOp, which would discard real (possibly committed) work.
    // It is surfaced as a recoverable candidate for controller review/salvage.
    let root = tempfile::tempdir().expect("artifact root");
    let empty_patch_path = root.path().join("cook.patch");
    let transcript_path = root.path().join("transcript.md");
    fs::write(&empty_patch_path, "").expect("empty patch");
    fs::write(
        &transcript_path,
        "# transcript\nreal provider work happened\n",
    )
    .expect("transcript");

    let provenance = AgentTaskArtifactsPathProvenance {
        owner: "homeboy".to_string(),
        locality: "runner".to_string(),
        plan_id: "plan".to_string(),
        run_id: None,
        task_id: "opencode-empty-patch-with-evidence".to_string(),
        attempt: 1,
    };

    let mut empty_patch =
        fixture_artifact("patch", "patch", &empty_patch_path, Some("text/x-patch"));
    empty_patch.size_bytes = None;
    let mut transcript = fixture_artifact(
        "transcript",
        "transcript",
        &transcript_path,
        Some("text/markdown"),
    );
    transcript.size_bytes = None;

    let mut outcome = failed_outcome_with_run_result(Value::Null);
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;
    outcome.artifacts = vec![empty_patch, transcript];
    normalize_homeboy_local_artifact_sizes(&mut outcome, root.path(), &provenance);

    assert_eq!(
        outcome.status,
        AgentTaskOutcomeStatus::CandidateRecoverable,
        "empty patch beside substantive evidence must be reviewable, not NoOp"
    );
    assert_eq!(outcome.artifacts[0].size_bytes, Some(0));
    assert!(outcome.artifacts[1].size_bytes.unwrap_or_default() > 0);
    assert!(outcome
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("needs review"));
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
fn typed_provider_quota_normalizes_to_rate_limited_and_preserves_retry_hint() {
    let outcome: AgentTaskOutcome = serde_json::from_value(json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "quota-task",
        "status": "provider_error",
        "failure_classification": "provider_quota",
        "summary": "quota exceeded",
        "metadata": { "retry_after_ms": 1500 }
    }))
    .expect("OpenCode quota outcome is accepted");

    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::RateLimited)
    );
    assert_eq!(outcome.metadata["retry_after_ms"], json!(1500));
    assert_eq!(
        serde_json::to_value(outcome).expect("serialize normalized outcome")
            ["failure_classification"],
        json!("rate_limited")
    );
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
