use super::common::{request, script};
use super::*;

#[test]
fn provider_capability_contract_exports_core_owned_schema_ids() {
    let contract = provider_capability_contract();

    assert_eq!(
        contract.schema,
        AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
    );
    assert_eq!(
        contract.provider_schema,
        AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
    );
    assert_eq!(contract.schemas.request_schema, AGENT_TASK_REQUEST_SCHEMA);
    assert_eq!(contract.schemas.outcome_schema, AGENT_TASK_OUTCOME_SCHEMA);
    assert_eq!(contract.tool_request_schema, AGENT_TOOL_REQUEST_SCHEMA);
    assert_eq!(contract.tool_result_schema, AGENT_TOOL_RESULT_SCHEMA);
    assert_eq!(contract.tool_policy_schema, AGENT_TOOL_POLICY_SCHEMA);
}

#[test]
fn provider_manifest_defaults_core_owned_schema_ids() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "minimal.provider",
        "backend": "minimal",
        "command": "minimal-provider"
    }))
    .expect("provider manifest");

    assert_eq!(provider.schema, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA);
    assert_eq!(provider.request_schema, AGENT_TASK_REQUEST_SCHEMA);
    assert_eq!(provider.outcome_schema, AGENT_TASK_OUTCOME_SCHEMA);
}

#[test]
fn provider_manifest_accepts_typed_command_argv_aliases() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "argv.provider",
        "backend": "argv",
        "command": "legacy-provider --legacy",
        "argv": ["{{extension_path}}/bin/provider", "--runtime", "{{runtime_path}}"]
    }))
    .expect("provider manifest");

    assert_eq!(provider.command, "legacy-provider --legacy");
    assert_eq!(
        provider.command_argv,
        vec![
            "{{extension_path}}/bin/provider".to_string(),
            "--runtime".to_string(),
            "{{runtime_path}}".to_string(),
        ]
    );
}

#[test]
fn provider_manifest_accepts_command_invocation_contract() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "invocation.provider",
        "backend": "invocation",
        "command": "legacy-provider --legacy",
        "invocation": {
            "schema": "homeboy/command-invocation/v1",
            "argv": ["{{runtime_path}}/bin/provider", "--json"],
            "cwd": "{{runtime_path}}",
            "env": [{ "name": "TOKEN", "source": "secret_env", "redacted": true }],
            "display": "provider --json",
            "redaction": { "env": ["TOKEN"] }
        }
    }))
    .expect("provider manifest");

    assert_eq!(provider.invocation.argv[0], "{{runtime_path}}/bin/provider");
    assert_eq!(provider.invocation.cwd.as_deref(), Some("{{runtime_path}}"));
    assert_eq!(provider.invocation.env[0].name, "TOKEN");
    assert_eq!(
        provider.invocation.display.as_deref(),
        Some("provider --json")
    );
    assert_eq!(provider.invocation.redaction.env, vec!["TOKEN"]);
}

#[test]
fn provider_command_parts_warns_for_legacy_string_command() {
    let (_request, provider) = request("task-legacy-command", "legacy-provider --flag".to_string());

    let (program, args, cwd) = provider_command_parts(&provider).expect("command parts");

    assert_eq!(program, "legacy-provider");
    assert_eq!(args, vec!["--flag"]);
    assert_eq!(cwd, None);
}

#[test]
fn provider_manifest_preserves_unknown_metadata_on_export() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "metadata.provider",
        "backend": "metadata",
        "command": "metadata-provider",
        "provider_metadata": {
            "runtime": "provider-owned"
        },
        "runner_readiness": [{
            "id": "ready",
            "label": "Ready",
            "provider_hint": "preserve-me",
            "env_path": {
                "env": ["PROVIDER_HOME"],
                "path_kind": "provider-cache"
            },
            "executable": {
                "env": ["PROVIDER_BIN"],
                "candidates": ["provider-bin"],
                "version_command": ["--version"],
                "install_hint": "Install provider-bin.",
                "provider_executable_hint": "preserve-me"
            }
        }],
        "timeout_artifact_discovery": {
            "paths": ["artifacts"],
            "provider_discovery": true,
            "artifact_patterns": [{
                "kind": "log",
                "filename_contains": ["provider"],
                "provider_role": "diagnostic"
            }]
        },
        "role_aliases": {
            "outputs": { "patch": ["diff"] },
            "provider_alias_policy": "strict"
        },
        "result_contract": {
            "typed_artifact_envelope": {
                "schema": "provider/artifact-result-envelope/v1",
                "output": "provider_run_result",
                "provider_label": "Provider Runtime",
                "diagnostic_class_prefix": "provider_runtime",
                "private_shape_markers": ["private_result"],
                "require_typed_artifacts": true
            }
        },
        "runtime_contract": {
            "capabilities": ["sandbox", "artifacts"],
            "lifecycle_states": {
                "execution_states": { "queued": "queued", "done": "succeeded" },
                "outcome_statuses": { "ok": "succeeded", "error": "failed" },
                "failure_classifications": { "error": "provider" }
            },
            "normalization": {
                "status_path": "outputs.runtime.status",
                "summary_path": "outputs.runtime.summary",
                "output_artifacts": [{
                    "name": "patch",
                    "type": "patch",
                    "artifact_schema": "text/x-patch",
                    "path": "outputs.runtime.artifacts.patch",
                    "kind": "patch"
                }]
            }
        },
        "workspace_materialization": {
            "cwd": WorkspaceCwdMode::GitCheckout.to_string(),
            "provider_workspace_mode": "linked"
        }
    }))
    .expect("provider manifest");

    assert_eq!(
        provider.extra["provider_metadata"]["runtime"],
        "provider-owned"
    );
    assert_eq!(
        provider.runner_readiness[0].extra["provider_hint"],
        "preserve-me"
    );
    assert_eq!(
        provider.runner_readiness[0]
            .env_path
            .as_ref()
            .expect("env path")
            .extra["path_kind"],
        "provider-cache"
    );
    let executable = provider.runner_readiness[0]
        .executable
        .as_ref()
        .expect("executable readiness");
    assert_eq!(executable.env, vec!["PROVIDER_BIN".to_string()]);
    assert_eq!(executable.candidates, vec!["provider-bin".to_string()]);
    assert_eq!(executable.version_command, vec!["--version".to_string()]);
    assert_eq!(
        executable.install_hint.as_deref(),
        Some("Install provider-bin.")
    );
    assert_eq!(executable.extra["provider_executable_hint"], "preserve-me");
    assert_eq!(
        provider.timeout_artifact_discovery.extra["provider_discovery"],
        true
    );
    assert_eq!(
        provider.timeout_artifact_discovery.artifact_patterns[0].extra["provider_role"],
        "diagnostic"
    );
    assert_eq!(
        provider.role_aliases.extra["provider_alias_policy"],
        "strict"
    );
    let result_contract = provider
        .result_contract
        .typed_artifact_envelope
        .as_ref()
        .expect("typed artifact envelope contract");
    assert_eq!(
        result_contract.schema,
        "provider/artifact-result-envelope/v1"
    );
    assert_eq!(result_contract.output, "provider_run_result");
    assert_eq!(
        result_contract.diagnostic_class_prefix.as_deref(),
        Some("provider_runtime")
    );
    assert_eq!(
        result_contract.private_shape_markers,
        vec!["private_result"]
    );
    assert_eq!(result_contract.require_typed_artifacts, Some(true));
    assert_eq!(
        provider.runtime_contract.capabilities,
        vec!["sandbox".to_string(), "artifacts".to_string()]
    );
    assert_eq!(
        provider
            .runtime_contract
            .lifecycle_states
            .outcome_statuses
            .get("ok"),
        Some(&AgentTaskOutcomeStatus::Succeeded)
    );
    assert_eq!(
        provider.runtime_contract.normalization.output_artifacts[0].name,
        "patch"
    );
    assert_eq!(
        provider
            .workspace_materialization
            .as_ref()
            .expect("workspace materialization")
            .extra["provider_workspace_mode"],
        "linked"
    );

    let exported = serde_json::to_value(&provider).expect("provider export");
    assert_eq!(exported["provider_metadata"]["runtime"], "provider-owned");
    assert_eq!(
        exported["runner_readiness"][0]["provider_hint"],
        "preserve-me"
    );
    assert_eq!(
        exported["runner_readiness"][0]["env_path"]["path_kind"],
        "provider-cache"
    );
    assert_eq!(
        exported["runner_readiness"][0]["executable"]["version_command"][0],
        "--version"
    );
    assert_eq!(
        exported["timeout_artifact_discovery"]["provider_discovery"],
        true
    );
    assert_eq!(
        exported["timeout_artifact_discovery"]["artifact_patterns"][0]["provider_role"],
        "diagnostic"
    );
    assert_eq!(exported["role_aliases"]["provider_alias_policy"], "strict");
    assert_eq!(
        exported["result_contract"]["typed_artifact_envelope"]["schema"],
        "provider/artifact-result-envelope/v1"
    );
    assert_eq!(
        exported["result_contract"]["typed_artifact_envelope"]["private_shape_markers"][0],
        "private_result"
    );
    assert_eq!(exported["runtime_contract"]["capabilities"][0], "sandbox");
    assert_eq!(
        exported["runtime_contract"]["normalization"]["output_artifacts"][0]["name"],
        "patch"
    );
    assert_eq!(
        exported["workspace_materialization"]["provider_workspace_mode"],
        "linked"
    );
}

#[test]
fn runtime_contract_normalizes_provider_outputs_to_canonical_artifacts() {
    let provider_output = json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-runtime-normalization",
        "status": "succeeded",
        "outputs": {
            "runtime": {
                "status": "done",
                "summary": "runtime finished",
                "artifacts": {
                    "patch": "/tmp/runtime.patch",
                    "report": { "path": "/tmp/report.json" }
                }
            }
        }
    });
    let script = script(&format!(
        "process.stdout.write(JSON.stringify({}));",
        provider_output
    ));
    let (request, mut provider) = request("task-runtime-normalization", format!("node {script}"));
    provider.runtime_contract = AgentTaskRuntimeContract {
        capabilities: vec!["sandbox".to_string()],
        lifecycle_states: AgentTaskRuntimeLifecycleStates {
            execution_states: BTreeMap::new(),
            outcome_statuses: BTreeMap::from([(
                "done".to_string(),
                AgentTaskOutcomeStatus::Succeeded,
            )]),
            failure_classifications: BTreeMap::new(),
        },
        normalization: AgentTaskRuntimeNormalization {
            status_path: Some("outputs.runtime.status".to_string()),
            summary_path: Some("outputs.runtime.summary".to_string()),
            output_artifacts: vec![
                AgentTaskRuntimeOutputArtifactMapping {
                    name: "patch".to_string(),
                    artifact_type: Some("patch".to_string()),
                    artifact_schema: Some("text/x-patch".to_string()),
                    path: "outputs.runtime.artifacts.patch".to_string(),
                    kind: Some("patch".to_string()),
                    mime: Some("text/x-patch".to_string()),
                    id: None,
                },
                AgentTaskRuntimeOutputArtifactMapping {
                    name: "report".to_string(),
                    artifact_type: Some("agent_report".to_string()),
                    artifact_schema: Some("application/json".to_string()),
                    path: "outputs.runtime.artifacts.report".to_string(),
                    kind: Some("report".to_string()),
                    mime: Some("application/json".to_string()),
                    id: None,
                },
            ],
        },
        apply_back: AgentTaskRuntimeApplyBack::default(),
        staging: AgentTaskRuntimeStagingContract::default(),
    };

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(outcome.summary.as_deref(), Some("runtime finished"));
    assert_eq!(outcome.artifacts.len(), 2);
    assert_eq!(outcome.artifacts[0].kind, "patch");
    assert_eq!(
        outcome.artifacts[0].path.as_deref(),
        Some("/tmp/runtime.patch")
    );
    assert_eq!(outcome.artifacts[1].kind, "report");
    assert_eq!(
        outcome.artifacts[1].path.as_deref(),
        Some("/tmp/report.json")
    );
    assert_eq!(outcome.typed_artifacts.len(), 2);
    assert_eq!(outcome.typed_artifacts[0].name, "patch");
    assert_eq!(
        outcome.typed_artifacts[1].artifact_schema.as_deref(),
        Some("application/json")
    );
}

#[test]
fn runtime_contract_maps_failed_runtime_status() {
    let provider_output = json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-runtime-failed",
        "status": "succeeded",
        "outputs": { "sample_runtime": { "state": "failed" } }
    });
    let script = script(&format!(
        "process.stdout.write(JSON.stringify({}));",
        provider_output
    ));
    let (request, mut provider) = request("task-runtime-failed", format!("node {script}"));
    provider.backend = "sample-runtime".to_string();
    provider.runtime_contract.lifecycle_states.outcome_statuses =
        BTreeMap::from([("failed".to_string(), AgentTaskOutcomeStatus::Failed)]);
    provider
        .runtime_contract
        .lifecycle_states
        .failure_classifications = BTreeMap::from([(
        "failed".to_string(),
        AgentTaskFailureClassification::Provider,
    )]);
    provider.runtime_contract.normalization.status_path =
        Some("outputs.sample_runtime.state".to_string());

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
}

#[test]
fn readiness_validation_fails_before_execution_when_provider_executable_is_missing() {
    let (_request, mut provider) = request("task-readiness", "minimal-provider".to_string());
    provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "test.executable".to_string(),
        label: "Test executable".to_string(),
        secret_env: Vec::new(),
        env_path: None,
        executable: Some(AgentTaskProviderExecutableReadiness {
            env: vec!["HOMEBOY_TEST_PROVIDER_COMMAND".to_string()],
            candidates: vec![format!(
                "homeboy-definitely-missing-provider-{}",
                std::process::id()
            )],
            version_command: Vec::new(),
            install_hint: Some("Install the test provider".to_string()),
            extra: BTreeMap::new(),
        }),
        remediation: None,
        extra: BTreeMap::new(),
    }];

    let err =
        validate_provider_runner_readiness_for_backend_with_providers(&[provider], "test", None)
            .expect_err("missing provider executable should block preflight");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("backend 'test' is registered"));
    assert!(err
        .message
        .contains("provider runner executable 'Test executable'"));
    assert!(err.message.contains("HOMEBOY_TEST_PROVIDER_COMMAND"));
    assert!(err.message.contains("Install the test provider"));
}

#[test]
fn provider_command_env_exposes_generic_agent_tool_contracts() {
    let (mut request, provider) = request("task-1", "minimal-provider".to_string());
    request.policy.tools.tools.insert(
        "lookup".to_string(),
        AgentToolPolicyRule {
            execution_location: AgentToolExecutionLocation::ControlPlane,
            timeout_ms: Some(500),
            reason: Some("test policy".to_string()),
        },
    );

    let env = provider_command_env(&request, &provider).expect("provider env");
    let env: BTreeMap<String, String> = env.into_iter().collect();

    assert_eq!(
        env.get("HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA")
            .map(String::as_str),
        Some(AGENT_TOOL_REQUEST_SCHEMA)
    );
    assert_eq!(
        env.get("HOMEBOY_AGENT_TOOL_RESULT_SCHEMA")
            .map(String::as_str),
        Some(AGENT_TOOL_RESULT_SCHEMA)
    );
    assert_eq!(
        env.get("HOMEBOY_AGENT_TOOL_POLICY_SCHEMA")
            .map(String::as_str),
        Some(AGENT_TOOL_POLICY_SCHEMA)
    );
    let dispatch_command = env
        .get("HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND")
        .expect("tool dispatch command env");
    assert!(
        dispatch_command.ends_with(" agent-task tool dispatch"),
        "dispatch command should invoke hidden tool dispatch command: {dispatch_command}"
    );
    assert!(
        dispatch_command.starts_with('/') || dispatch_command.starts_with('\''),
        "dispatch command should start with an absolute executable path, shell quoted when needed: {dispatch_command}"
    );

    let policy: crate::core::agent_task::AgentToolPolicy = serde_json::from_str(
        env.get("HOMEBOY_AGENT_TOOL_POLICY_JSON")
            .expect("tool policy env"),
    )
    .expect("tool policy json");
    assert_eq!(
        policy.execution_location_for("lookup"),
        AgentToolExecutionLocation::ControlPlane
    );
    assert_eq!(
        policy.execution_location_for("unknown"),
        AgentToolExecutionLocation::Disabled
    );
}
