use super::*;
use crate::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
    AgentTaskWorkspaceMode, AgentToolExecutionLocation, AgentToolPolicyRule,
};
use crate::core::agent_task_scheduler::{
    AgentTaskCancellationToken, AgentTaskExecutionContext, AgentTaskPlan, AgentTaskScheduler,
};
use std::fs;

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
    assert_eq!(contract.request_schema, AGENT_TASK_REQUEST_SCHEMA);
    assert_eq!(contract.outcome_schema, AGENT_TASK_OUTCOME_SCHEMA);
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
            "cwd": "git_checkout",
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

fn script(body: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "homeboy-agent-task-provider-{}-{}.js",
        std::process::id(),
        body.len()
    ));
    fs::write(&path, body).expect("script written");
    path.to_string_lossy().to_string()
}

fn request(task_id: &str, command: String) -> (AgentTaskRequest, AgentTaskExecutorProvider) {
    let provider = AgentTaskExecutorProvider {
        schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
        id: "test.provider".to_string(),
        label: None,
        backend: "test".to_string(),
        default_backend: false,
        command,
        command_argv: Vec::new(),
        invocation: CommandInvocation::default(),
        request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        capabilities: vec!["structured_outcome".to_string()],
        secret_requirements: Vec::new(),
        secret_env_requirements: Vec::new(),
        workspace_materialization: None,
        provider_defaults: BTreeMap::new(),
        runner_readiness: Vec::new(),
        runner_sources: Vec::new(),
        dependency_failure_patterns: Vec::new(),
        lab_runtime_components: Vec::new(),
        timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery::default(),
        role_aliases: AgentTaskProviderRoleAliases::default(),
        runtime_contract: AgentTaskRuntimeContract::default(),
        extension_id: None,
        extension_path: None,
        runtime_id: None,
        runtime_path: None,
        extra: BTreeMap::new(),
    };
    let request = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        group_key: None,
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "test".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "run".to_string(),
        inputs: Value::Null,
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: Value::Null,
    };
    (request, provider)
}

#[test]
fn provider_preflight_reports_missing_command_before_spawn() {
    let (request, provider) = request(
        "missing-command",
        "homeboy-definitely-missing-provider-command --json".to_string(),
    );

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.provider_command_unavailable"
    );
    assert_eq!(
        outcome.diagnostics[0].data["program"],
        "homeboy-definitely-missing-provider-command"
    );
    assert!(outcome.diagnostics[0].data["failures"][0]["remediation"]
        .as_str()
        .expect("remediation")
        .contains("PATH"));
}

#[test]
fn provider_preflight_reports_missing_secret_readiness() {
    let (request, mut provider) = request(
        "missing-secret",
        std::env::current_exe()
            .expect("current exe")
            .display()
            .to_string(),
    );
    provider.secret_requirements = vec![AgentTaskProviderSecretRequirement {
        name: None,
        env: vec!["HOMEBOY_TEST_PROVIDER_SECRET_THAT_SHOULD_NOT_EXIST".to_string()],
        required: Some(true),
        purpose: Some("test".to_string()),
        extra: BTreeMap::new(),
    }];

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.secret_env_missing"
    );
    assert_eq!(
        outcome.diagnostics[0].data["secret_env_status"][0]["name"],
        "HOMEBOY_TEST_PROVIDER_SECRET_THAT_SHOULD_NOT_EXIST"
    );
    assert_eq!(
        outcome.diagnostics[0].data["secret_env_status"][0]["configured"],
        false
    );
}

#[test]
fn required_extension_ids_follow_selected_agent_task_providers() {
    let (request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    provider_a.id = "provider-a".to_string();
    provider_a.extension_id = Some("extension-a".to_string());
    let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    request_b.executor.selector = Some("provider-b".to_string());
    provider_b.id = "provider-b".to_string();
    provider_b.extension_id = Some("extension-b".to_string());
    let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

    let extension_ids = executor
        .required_extension_ids_for_plan(&AgentTaskPlan::new("plan-a", vec![request_a, request_b]));

    assert_eq!(extension_ids, vec!["extension-a", "extension-b"]);
}

#[test]
fn lab_runtime_component_ids_follow_selected_agent_task_providers() {
    let (request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    provider_a.id = "provider-a".to_string();
    provider_a.lab_runtime_components = vec!["agents-api".to_string(), "data-machine".to_string()];
    let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    request_b.executor.selector = Some("provider-b".to_string());
    provider_b.id = "provider-b".to_string();
    provider_b.lab_runtime_components =
        vec!["data-machine".to_string(), "php-ai-client".to_string()];
    let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

    let component_ids = executor.lab_runtime_component_ids_for_plan(&AgentTaskPlan::new(
        "plan-a",
        vec![request_a, request_b],
    ));

    assert_eq!(
        component_ids,
        vec!["agents-api", "data-machine", "php-ai-client"]
    );
}

#[test]
fn repo_local_gate_execution_kind_runs_without_extension_provider() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(
        temp.path().join("gate.mjs"),
        "import fs from 'node:fs'; fs.writeFileSync(process.env.RESULT_PATH, JSON.stringify({ok:true}));",
    )
    .expect("write script");
    let (mut request, _) = request("repo-local-gate", "unused".to_string());
    request.executor.backend = "agent-task".to_string();
    request.executor.config = json!({
        "execution_kind": "repo_local_gate",
        "script": "gate.mjs",
        "artifact_outputs": {
            "result": { "schema": "example/Result/v1" }
        }
    });
    request.workspace = AgentTaskWorkspace {
        mode: AgentTaskWorkspaceMode::Existing,
        root: Some(temp.path().display().to_string()),
        ..AgentTaskWorkspace::default()
    };
    let executor = ExtensionProviderAgentTaskExecutor::with_providers(Vec::new());

    let outcome = executor.execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "gate-plan".to_string(),
            attempt: 1,
            cancellation: AgentTaskCancellationToken::default(),
        },
    );

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(outcome.outputs["result"]["ok"], true);
    assert_eq!(outcome.typed_artifacts.len(), 1);
    assert_eq!(outcome.typed_artifacts[0].name, "result");
}

#[test]
fn provider_selection_matches_exact_backend_first() {
    let (_, mut exact_provider) = request("task-a", "node exact-provider.js".to_string());
    exact_provider.id = "exact-provider".to_string();
    exact_provider.backend = "requested-backend".to_string();
    exact_provider.extension_id = Some("other-extension".to_string());
    let (_, mut extension_provider) = request("task-b", "node extension-provider.js".to_string());
    extension_provider.id = "extension-provider".to_string();
    extension_provider.backend = "renamed-backend".to_string();
    extension_provider.extension_id = Some("requested-backend".to_string());

    let providers = [extension_provider, exact_provider];
    let selected = select_provider_by_backend(&providers, "requested-backend", None)
        .expect("provider selected");

    assert_eq!(selected.id, "exact-provider");
}

#[test]
fn provider_selection_reports_exact_backend_selector_mismatch() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.id = "example.synthetic-agent-task-executor".to_string();
    provider.backend = "synthetic-runtime".to_string();

    let providers = [provider];
    let resolution = resolve_provider_for_backend(&providers, "synthetic-runtime", Some("fast"));

    assert_eq!(
        resolution,
        ProviderResolution::SelectorMismatch {
            available_ids: vec!["example.synthetic-agent-task-executor".to_string()],
        }
    );
}

#[test]
fn provider_selection_matches_unique_extension_alias() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.id = "extension-a.provider".to_string();
    provider.backend = "renamed-backend".to_string();
    provider.extension_id = Some("extension-a".to_string());

    let providers = [provider];
    let selected =
        select_provider_by_backend(&providers, "extension-a", None).expect("provider selected");

    assert_eq!(selected.backend, "renamed-backend");
}

#[test]
fn provider_selection_rejects_ambiguous_extension_alias() {
    let (_, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    provider_a.id = "provider-a".to_string();
    provider_a.backend = "renamed-backend".to_string();
    provider_a.extension_id = Some("extension-a".to_string());
    let (_, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    provider_b.id = "provider-b".to_string();
    provider_b.backend = "fixture".to_string();
    provider_b.extension_id = Some("extension-a".to_string());

    assert!(select_provider_by_backend(&[provider_a, provider_b], "extension-a", None).is_none());
}

#[test]
fn provider_selection_applies_selector_to_unique_extension_alias() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.id = "selected-provider".to_string();
    provider.backend = "renamed-backend".to_string();
    provider.extension_id = Some("extension-a".to_string());

    assert!(
        select_provider_by_backend(&[provider.clone()], "extension-a", Some("missing")).is_none()
    );
    assert_eq!(
        select_provider_by_backend(&[provider], "extension-a", Some("selected-provider"))
            .expect("provider selected")
            .id,
        "selected-provider"
    );
}

#[test]
fn provider_manifest_parses_role_aliases() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-executor-provider/v1",
        "id": "custom.provider",
        "backend": "custom",
        "default_backend": true,
        "command": "custom-agent-task",
        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
        "role_aliases": {
            "artifact_kinds": {
                "patch": ["custom-patch"]
            },
            "artifact_filenames": {
                "preflight_evidence": ["*-preflight.json"]
            },
            "outputs": {
                "provider_run_result": ["custom_run_result"]
            },
            "metadata": {
                "provider_run_result": ["customRunResult"]
            }
        }
    }))
    .expect("provider manifest");

    assert!(provider.default_backend);
    assert!(provider
        .role_aliases
        .artifact_kind_matches_role("patch", "custom-patch"));
    assert!(provider
        .role_aliases
        .artifact_filename_matches_role("preflight_evidence", "runner-preflight.json"));
    assert_eq!(
        provider
            .role_aliases
            .output_aliases_for_role("provider_run_result"),
        vec!["custom_run_result"]
    );
}

#[test]
fn provider_command_receives_canonical_artifact_declarations() {
    let command = format!(
        "node {}",
        script(
            r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: 'succeeded',
  artifacts: [],
  typed_artifacts: [],
  evidence_refs: [],
  diagnostics: [],
  outputs: { artifact_declarations: input.artifact_declarations },
  metadata: null
}));
"#
        )
    );
    let (mut request, provider) = request("task-artifact-normalization", command);
    request.expected_artifacts = vec!["patch".to_string()];

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(outcome.outputs["artifact_declarations"][0]["name"], "patch");
    assert_eq!(
        outcome.outputs["artifact_declarations"][0]["required"],
        true
    );
}

#[test]
fn provider_command_argv_preserves_extension_and_runtime_paths_with_spaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let extension_dir = temp.path().join("extension path with spaces");
    let runtime_dir = temp.path().join("runtime path with spaces");
    fs::create_dir_all(&extension_dir).expect("extension dir");
    fs::create_dir_all(&runtime_dir).expect("runtime dir");
    fs::write(
        runtime_dir.join("provider.js"),
        r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: process.argv[2] === '--extension' && process.argv[3].includes('extension path with spaces') ? 'succeeded' : 'failed',
  summary: process.argv.slice(2).join('|')
}));
"#,
    )
    .expect("provider script");
    let (request, mut provider) = request("task-argv-spaces", "legacy unused".to_string());
    provider.extension_path = Some(extension_dir.to_string_lossy().to_string());
    provider.runtime_path = Some(runtime_dir.to_string_lossy().to_string());
    provider.command_argv = vec![
        "node".to_string(),
        "{{runtime_path}}/provider.js".to_string(),
        "--extension".to_string(),
        "{{extension_path}}".to_string(),
    ];

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("extension path with spaces"));
}

#[test]
fn provider_invocation_argv_and_cwd_preserve_paths_with_spaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime_dir = temp.path().join("runtime path with spaces");
    fs::create_dir_all(&runtime_dir).expect("runtime dir");
    fs::write(
        runtime_dir.join("provider.js"),
        r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: process.cwd().includes('runtime path with spaces') ? 'succeeded' : 'failed',
  summary: process.cwd()
}));
"#,
    )
    .expect("provider script");
    let (request, mut provider) = request("task-invocation-cwd", "legacy unused".to_string());
    provider.runtime_path = Some(runtime_dir.to_string_lossy().to_string());
    provider.invocation.argv = vec!["node".to_string(), "provider.js".to_string()];
    provider.invocation.cwd = Some("{{runtime_path}}".to_string());

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("runtime path with spaces"));
}

#[test]
fn provider_manifest_parses_runner_and_dependency_contracts() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-executor-provider/v1",
        "id": "custom.provider",
        "backend": "custom",
        "command": "custom-agent-task",
        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
        "runner_readiness": [{
            "id": "custom.runtime_cache",
            "label": "Custom runtime cache",
            "secret_env": ["CUSTOM_RUNTIME_TOKEN"],
            "env_path": { "env": ["CUSTOM_RUNTIME_BIN"], "revision": true },
            "remediation": "Refresh the custom runtime cache."
        }],
        "dependency_failure_patterns": [{
            "id": "custom.prepared_dependency",
            "label": "Custom prepared dependency",
            "path_contains": "prepared-dependencies/",
            "error_contains_any": ["enoent", "no such file or directory"],
            "remediation": "Refresh prepared dependencies."
        }],
        "lab_runtime_components": ["agents-api", "data-machine"]
    }))
    .expect("provider manifest");

    assert_eq!(provider.runner_readiness[0].id, "custom.runtime_cache");
    assert_eq!(
        provider.runner_readiness[0].secret_env,
        vec!["CUSTOM_RUNTIME_TOKEN"]
    );
    assert_eq!(
        provider.runner_readiness[0].env_path.as_ref().unwrap().env,
        vec!["CUSTOM_RUNTIME_BIN"]
    );
    assert_eq!(
        provider.dependency_failure_patterns[0].path_contains,
        "prepared-dependencies/"
    );
    assert_eq!(
        provider.lab_runtime_components,
        vec!["agents-api", "data-machine"]
    );
}

#[test]
fn default_backend_ignores_provider_declaration() {
    let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    provider_a.backend = "first".to_string();
    let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    provider_b.backend = "preferred".to_string();
    provider_b.default_backend = true;

    let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

    crate::test_support::with_isolated_home(|_| {
        assert_eq!(executor.default_backend().unwrap(), None);
    });
}

#[test]
fn default_backend_uses_global_config_policy() {
    crate::test_support::with_isolated_home(|_| {
        defaults::save_config(&defaults::HomeboyConfig {
            agent_task: defaults::AgentTaskConfig {
                default_backend: Some("configured".to_string()),
                ..defaults::AgentTaskConfig::default()
            },
            ..defaults::HomeboyConfig::default()
        })
        .expect("config saved");

        assert_eq!(default_backend().unwrap().as_deref(), Some("configured"));
    });
}

#[test]
fn default_backend_uses_extension_policy() {
    crate::test_support::with_isolated_home(|home| {
        defaults::save_config(&defaults::HomeboyConfig {
            agent_task: defaults::AgentTaskConfig {
                default_backend: Some("global-policy".to_string()),
                ..defaults::AgentTaskConfig::default()
            },
            ..defaults::HomeboyConfig::default()
        })
        .expect("config saved");
        let extension_dir = home
            .path()
            .join(".config/homeboy/extensions/runtime-extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join("runtime-extension.json"),
            json!({
                "name": "Runtime Extension",
                "version": "1.0.0",
                "agent_task": { "default_backend": "extension-policy" }
            })
            .to_string(),
        )
        .expect("extension manifest");

        assert_eq!(
            default_backend().unwrap().as_deref(),
            Some("extension-policy")
        );
    });
}

#[test]
fn default_backend_rejects_ambiguous_extension_policy() {
    crate::test_support::with_isolated_home(|home| {
        for (id, backend) in [("runtime-a", "backend-a"), ("runtime-b", "backend-b")] {
            let extension_dir = home.path().join(format!(".config/homeboy/extensions/{id}"));
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join(format!("{id}.json")),
                json!({
                    "name": id,
                    "version": "1.0.0",
                    "agent_task": { "default_backend": backend }
                })
                .to_string(),
            )
            .expect("extension manifest");
        }

        let error = default_backend().expect_err("ambiguous policy should fail");
        assert!(error.message.contains("ambiguous"));
    });
}

#[test]
fn default_backend_reads_component_scoped_extension_policy() {
    let mut component = component::Component::new(
        "fixture".to_string(),
        "/tmp/fixture".to_string(),
        String::new(),
        None,
    );
    component.extensions = Some(std::collections::HashMap::from([(
        "runtime-extension".to_string(),
        component::ScopedExtensionConfig {
            settings: std::collections::HashMap::from([(
                "agent_task".to_string(),
                json!({ "default_backend": "component-policy" }),
            )]),
            ..component::ScopedExtensionConfig::default()
        },
    )]));

    assert_eq!(
        component_default_backend(&component).as_deref(),
        Some("component-policy")
    );
}

#[test]
fn default_backend_ignores_provider_manifest_default_backend() {
    crate::test_support::with_isolated_home(|home| {
        let runtime_dir = home
            .path()
            .join(".config/homeboy/agent-runtimes/standalone-runtime");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::write(
            runtime_dir.join("standalone-runtime.json"),
            json!({
                "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                "id": "standalone-runtime",
                "agent_task_executors": [{
                    "schema": "homeboy/agent-task-executor-provider/v1",
                    "id": "runtime.provider",
                    "backend": "runtime-default",
                    "default_backend": true,
                    "command": "runtime-provider",
                    "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                    "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                }]
            })
            .to_string(),
        )
        .expect("runtime manifest");

        assert_eq!(default_backend().unwrap(), None);
    });
}

#[test]
fn default_backend_is_absent_without_provider_declaration() {
    crate::test_support::with_isolated_home(|_| {
        let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.backend = "first".to_string();
        let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.backend = "second".to_string();

        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        assert_eq!(executor.default_backend().unwrap(), None);
    });
}

#[test]
fn provider_command_interpolates_runtime_path_separately_from_extension_path() {
    let (_, mut provider) = request(
        "task-a",
        "{{runtime_path}}/bin/provider --extension {{extension_path}}".to_string(),
    );
    provider.extension_path = Some("/extensions/project-type".to_string());
    provider.runtime_path = Some("/agent-runtimes/example".to_string());

    assert_eq!(
        render_provider_command_display(&provider),
        "/agent-runtimes/example/bin/provider --extension /extensions/project-type"
    );
}

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

#[test]
fn provider_workspace_materialization_declares_cwd_git_checkout_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some("git_checkout".to_string()),
        requires_git: None,
        write_scope: None,
        artifact_paths: Vec::new(),
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_default_secret_sources_resolve_required_env_without_duplicate_mapping() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(
            &auth_path,
            json!({
                "tokens": {
                    "access_token": "provider-owned-access-token",
                    "refresh_token": "provider-owned-refresh-token"
                }
            })
            .to_string(),
        )
        .expect("write auth");
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-oauth" });
        request.executor.secret_env = vec![
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
        ];
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env": request.executor.secret_env,
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.access_token"
                    },
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.refresh_token"
                    }
                }
            }),
        );

        let env = provider_command_env(&request, &provider).expect("provider env resolves");

        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "provider-owned-access-token".to_string()
        )));
        let rendered = serde_json::to_string(&provider_secret_sources(&provider, Some(&request)))
            .expect("sources json");
        assert!(!rendered.contains("provider-owned-access-token"));
    });
}

#[test]
fn provider_secret_sources_for_providers_include_default_json_sources() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.provider_defaults.insert(
        "example-oauth".to_string(),
        json!({
            "secret_env": ["EXAMPLE_PROVIDER_ACCESS_TOKEN"],
            "secret_env_sources": {
                "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                    "source": "json-file",
                    "path": "~/.example-provider/auth.json",
                    "field": "tokens.access_token"
                }
            }
        }),
    );

    let sources = provider_secret_sources_for_providers(&[provider]);

    let source = sources
        .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
        .expect("provider default source discovered");
    assert_eq!(source.source, "json-file");
    assert_eq!(
        source.path.as_deref(),
        Some("~/.example-provider/auth.json")
    );
    assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
}

#[test]
fn provider_default_secret_sources_accept_nested_json_sources() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(
            &auth_path,
            json!({
                "provider": {
                    "access": "provider-access-token",
                    "refresh": "provider-refresh-token",
                    "expires": 12345
                }
            })
            .to_string(),
        )
        .expect("write auth");
        let auth_path = auth_path.to_string_lossy().to_string();
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-oauth" });
        request.executor.secret_env = vec![
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
        ];
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env": [
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN",
                    "EXAMPLE_PROVIDER_EXPIRES_AT"
                ],
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.access"
                    },
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.refresh"
                    },
                    "EXAMPLE_PROVIDER_EXPIRES_AT": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.expires"
                    }
                }
            }),
        );

        let env = provider_command_env(&request, &provider).expect("provider env resolves");

        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "provider-refresh-token".to_string()
        )));
        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
            "12345".to_string()
        )));
    });
}

#[test]
fn provider_default_secret_sources_feed_secret_readiness_status() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(
            &auth_path,
            json!({
                "tokens": {
                    "access_token": "provider-owned-access-token"
                }
            })
            .to_string(),
        )
        .expect("write auth");
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.access_token"
                    }
                }
            }),
        );
        let fallback_sources = provider_secret_sources_for_providers(&[provider]);

        let status = crate::core::agent_task_secrets::secret_env_status_with_fallbacks(
            &["EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()],
            &fallback_sources,
        );

        assert_eq!(status.len(), 1);
        assert!(status[0].configured);
        assert_eq!(status[0].source, "json-file");
    });
}

#[test]
fn provider_workspace_materialization_declares_requires_git_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: None,
        requires_git: Some(true),
        write_scope: Some("artifacts".to_string()),
        artifact_paths: vec![".homeboy/provider".to_string()],
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_apply_back_contract_declares_git_checkout_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        apply_back: AgentTaskRuntimeApplyBack {
            requires_git_checkout: Some(true),
            strategy: Some("mutation_artifacts".to_string()),
            mutation_artifacts: vec![AgentTaskRuntimeMutationArtifact {
                name: "patch".to_string(),
                path: "outputs.runtime.artifacts.patch".to_string(),
                kind: Some("patch".to_string()),
                semantic_key: Some("workspace.patch".to_string()),
                apply_method: Some("git_apply".to_string()),
            }],
        },
        ..AgentTaskProviderWorkspaceMaterialization::default()
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_workspace_materialization_ignores_unselected_provider() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some("git_checkout".to_string()),
        requires_git: None,
        write_scope: None,
        artifact_paths: Vec::new(),
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(!provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "other",
        None
    ));
}

#[test]
fn provider_workspace_materialization_exports_typed_mount_specs() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some("workspace".to_string()),
        mounts: vec![WorkspaceMountSpec {
            handle: Some("homeboy@fix-workspace-materialization-spec".to_string()),
            repo: Some("homeboy".to_string()),
            host_path: Some("/host/workspaces/homeboy@fix".to_string()),
            target_path: Some("/workspace/homeboy".to_string()),
            mode: Some("read_write".to_string()),
            materialization: Some("bind_mount".to_string()),
            metadata: json!({ "source": "fixture" }),
            extra: BTreeMap::new(),
        }],
        ..AgentTaskProviderWorkspaceMaterialization::default()
    });

    let exported = serde_json::to_value(&provider).expect("provider json");

    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["handle"],
        "homeboy@fix-workspace-materialization-spec"
    );
    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["target_path"],
        "/workspace/homeboy"
    );
    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["materialization"],
        "bind_mount"
    );
}

#[test]
fn workspace_materialization_spec_validates_nested_mounts() {
    let materialization = AgentTaskProviderWorkspaceMaterialization {
        spec: Some(WorkspaceMaterializationSpec {
            materialization: Some("bind_mount".to_string()),
            mounts: vec![WorkspaceMountSpec {
                host_path: Some("/tmp/homeboy".to_string()),
                target_path: Some(" ".to_string()),
                ..WorkspaceMountSpec::default()
            }],
            ..WorkspaceMaterializationSpec::default()
        }),
        mounts: vec![WorkspaceMountSpec {
            host_path: Some("/tmp/homeboy".to_string()),
            ..WorkspaceMountSpec::default()
        }],
        ..AgentTaskProviderWorkspaceMaterialization::default()
    };

    let errors = materialization.validate().expect_err("validation errors");

    assert_eq!(
        errors,
        vec![
            "spec.mounts[0].target_path must not be blank".to_string(),
            "mounts[0].target_path is required when host_path is set".to_string(),
        ]
    );
}

#[test]
fn provider_secret_contracts_are_applied_generically() {
    let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
    request.executor.config = json!({ "provider": "example-provider" });
    provider.secret_requirements = vec![
        AgentTaskProviderSecretRequirement {
            name: Some("REQUIRED_TOKEN".to_string()),
            required: Some(true),
            ..AgentTaskProviderSecretRequirement::default()
        },
        AgentTaskProviderSecretRequirement {
            name: Some("OPTIONAL_TOKEN".to_string()),
            required: Some(false),
            ..AgentTaskProviderSecretRequirement::default()
        },
    ];
    provider.secret_env_requirements = vec![AgentTaskProviderSecretEnvRequirement {
        env: vec!["EXAMPLE_PROVIDER_TOKEN".to_string()],
        when: Some(json!({
            "any": [
                { "path": "executor.config.provider", "equals": "example-provider" },
                { "path": "provider", "equals": "example-provider" }
            ]
        })),
        ..AgentTaskProviderSecretEnvRequirement::default()
    }];
    provider.provider_defaults.insert(
        "example-provider".to_string(),
        json!({ "secret_env": ["EXAMPLE_PROVIDER_REFRESH_TOKEN"] }),
    );
    let mut plan = AgentTaskPlan::new("plan-a", vec![request]);

    apply_provider_runner_secret_env_contracts_with_providers(&mut plan, &[provider]);

    assert_eq!(
        plan.tasks[0].executor.secret_env,
        vec![
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_TOKEN".to_string(),
            "REQUIRED_TOKEN".to_string(),
        ]
    );
}

#[test]
fn scheduler_dispatches_extension_provider_command() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'ok',outputs:{issue_number:3447}}));")
    );
    let (request, provider) = request("task-a", command);
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-a", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(
        aggregate.outcomes[0].status,
        AgentTaskOutcomeStatus::Succeeded
    );
    assert_eq!(aggregate.outcomes[0].outputs["issue_number"], json!(3447));
}

#[test]
fn scheduler_reports_missing_extension_provider() {
    let (request, _provider) = request("task-missing-provider", "unused".to_string());
    let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-provider", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::CapabilityMissing)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.provider_missing"
    );
}

#[test]
fn scheduler_reports_provider_selector_mismatch() {
    let (mut request, mut provider) = request("task-selector-mismatch", "unused".to_string());
    request.executor.backend = "synthetic-runtime".to_string();
    request.executor.selector = Some("fast".to_string());
    provider.id = "example.synthetic-agent-task-executor".to_string();
    provider.backend = "synthetic-runtime".to_string();
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-selector-mismatch", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.provider_selector_mismatch"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["available_provider_ids"],
        json!(["example.synthetic-agent-task-executor"])
    );
}

#[test]
fn scheduler_reports_missing_provider_capability() {
    let (mut request, provider) = request("task-missing-capability", "unused".to_string());
    request.executor.required_capabilities = vec!["workspace_write".to_string()];
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-capability", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::CapabilityMissing)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.capability_missing"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["missing_capabilities"],
        json!(["workspace_write"])
    );
}

#[test]
fn scheduler_normalizes_malformed_provider_output() {
    let command = format!("node {}", script("process.stdout.write('{not json');"));
    let (request, provider) = request("task-malformed-provider", command);
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-malformed-provider", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.provider_malformed_json"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["stdout"],
        "{not json"
    );
}

#[test]
fn provider_preserves_structured_outcome_from_stderr_when_stdout_empty() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stderr.write('diagnostic prefix\\n' + JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'failed',summary:'captured provider evidence',failure_classification:'provider',diagnostics:[{class:'sample_runtime.empty_data_packet_returned',message:'empty data packet returned',data:{typed_artifacts:{}}}]}));")
    );
    let (request, provider) = request("task-stderr-outcome", command);

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        outcome.summary.as_deref(),
        Some("captured provider evidence")
    );
    assert_eq!(
        outcome.diagnostics[0].class,
        "sample_runtime.empty_data_packet_returned"
    );
    assert_eq!(outcome.diagnostics[0].data["typed_artifacts"], json!({}));
}

#[test]
fn provider_timeout_returns_structured_outcome() {
    let command = format!("node {}", script("setInterval(() => {}, 1000);"));
    let (mut request, provider) = request("task-timeout", command);
    request.limits.timeout_ms = Some(50);
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-timeout", vec![request]));

    assert_eq!(aggregate.totals.timed_out, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
}

#[test]
fn provider_can_return_timeout_payload_during_wrapper_grace() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'provider serialized timeout',failure_classification:'timeout',artifacts:[{schema:'homeboy/agent-task-artifact/v1',id:'timeout-evidence',kind:'provider-task-runner-preflight',path:'/tmp/timeout-evidence.json'}]})), 3050);")
    );
    let (mut request, provider) = request("task-timeout-payload", command);
    request.limits.timeout_ms = Some(3000);

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
    assert_eq!(
        outcome.summary.as_deref(),
        Some("provider serialized timeout")
    );
    assert_eq!(outcome.artifacts.len(), 1);
    assert_eq!(outcome.artifacts[0].id, "timeout-evidence");
}

#[test]
fn provider_command_receives_executor_config_env() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let config=JSON.parse(process.env.HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:config.marker==='configured'?'succeeded':'failed',summary:process.env.HOMEBOY_AGENT_TASK_PROVIDER_ID}));")
    );
    let (mut request, mut provider) = request("task-config", command);
    request.executor.config = json!({ "marker": "configured" });
    provider.extension_id = Some("wordpress".to_string());
    provider.extension_path = Some("/tmp/homeboy-extension".to_string());
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-config", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(
        aggregate.outcomes[0].summary.as_deref(),
        Some("test.provider")
    );
}

#[test]
fn provider_command_receives_declared_secret_env() {
    let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_SECRET_{}", std::process::id());
    std::env::set_var(&secret_name, "hydrated-secret");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:process.env.{secret_name}==='hydrated-secret'?'succeeded':'failed',summary:'checked'}}));"
        ))
    );
    let (mut request, provider) = request("task-secret-env", command);
    request.executor.secret_env = vec![secret_name.clone()];
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    std::env::remove_var(secret_name);
}

#[test]
fn provider_command_receives_canonical_secret_env_plan_without_values() {
    let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_PLAN_SECRET_{}", std::process::id());
    std::env::set_var(&secret_name, "hydrated-secret");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let plan=JSON.parse(process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON); let mapped=(plan.env_name_mapping['test.provider']||[]).includes('{secret_name}'); let configured=(plan.status||[]).some((item)=>item.name==='{secret_name}'&&item.configured===true&&item.source==='env'); let leaked=JSON.stringify(plan).includes('hydrated-secret'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:mapped&&configured&&!leaked?'succeeded':'failed',summary:JSON.stringify(plan)}}));"
        ))
    );
    let (mut request, mut provider) = request("task-secret-env-plan", command);
    provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "test.provider.auth".to_string(),
        label: "Test provider auth".to_string(),
        secret_env: vec![secret_name.clone()],
        env_path: None,
        executable: None,
        remediation: None,
        extra: BTreeMap::new(),
    }];
    request.executor.secret_env = vec![secret_name.clone()];
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env-plan", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    assert!(!aggregate.outcomes[0]
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("hydrated-secret"));
    std::env::remove_var(secret_name);
}

#[test]
fn missing_declared_secret_env_fails_before_provider_spawn() {
    let secret_name = format!(
        "HOMEBOY_TEST_MISSING_AGENT_TASK_SECRET_{}",
        std::process::id()
    );
    std::env::remove_var(&secret_name);
    let command = format!(
        "node {}",
        script("throw new Error('provider should not run');")
    );
    let (mut request, provider) = request("task-missing-secret-env", command);
    request.executor.secret_env = vec![secret_name.clone()];
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            provider,
        ]));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-secret-env", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.secret_env_missing"
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["missing_secret_env"],
        json!([secret_name])
    );
}

#[test]
fn fixture_backend_produces_deterministic_smoke_artifacts() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let (mut request, _provider) = request("task-fixture", "unused".to_string());
    request.executor.backend = "fixture".to_string();
    request.executor.config = json!({
        "artifact_root": artifact_root.path().display().to_string(),
        "changed_file": "docs/smoke.md"
    });
    let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-fixture", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "patch" && artifact.size_bytes.unwrap_or_default() > 0));
    assert!(outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "agent_result"));
    assert!(outcome
        .evidence_refs
        .iter()
        .any(|evidence| evidence.kind == "transcript"));
}

#[test]
fn fixture_backend_classifies_empty_runtime_bundle() {
    let artifact_root = tempfile::tempdir().expect("artifact root");
    let (mut request, _provider) = request("task-empty-runtime", "unused".to_string());
    request.executor.backend = "fixture".to_string();
    request.executor.config = json!({
        "artifact_root": artifact_root.path().display().to_string(),
        "mode": "empty_runtime_bundle"
    });
    let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-empty-runtime", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.fixture_empty_runtime_bundle"
    );
    assert!(aggregate.outcomes[0]
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "runtime_bundle"));
}

#[test]
fn is_transient_provider_error_classifies_transient_and_permanent_text() {
    // Transient network/provider blips.
    assert!(is_transient_provider_error(
        "Network error ... cURL error 28: Operation timed out after 15000ms"
    ));
    assert!(is_transient_provider_error("connection reset by peer"));
    assert!(is_transient_provider_error("503 Service Unavailable"));
    assert!(is_transient_provider_error("HTTP 502 Bad Gateway"));
    assert!(is_transient_provider_error("429 Too Many Requests"));

    // Permanent failures must not be treated as transient.
    assert!(!is_transient_provider_error(
        "401 Unauthorized: invalid token"
    ));
    assert!(!is_transient_provider_error(
        "400 Bad Request: validation failed"
    ));
    assert!(!is_transient_provider_error("404 Not Found"));
    assert!(!is_transient_provider_error(
        "malformed JSON in provider output"
    ));
    assert!(!is_transient_provider_error(
        "provider output path /tmp/homeboy-500abc/stdout.json was malformed"
    ));
}

/// Node script that increments a counter file and emits a transient cURL-28
/// provider error for the first `fail_until` attempts, then a success
/// outcome. Used to prove transient retries recover.
fn transient_then_success_script(state_path: &Path, fail_until: u32) -> String {
    let state = state_path.to_string_lossy().replace('\\', "\\\\");
    script(&format!(
        "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); \
         let p='{state}'; let n=0; try {{ n=parseInt(fs.readFileSync(p,'utf8'))||0; }} catch(e) {{}} \
         n+=1; fs.writeFileSync(p, String(n)); \
         if (n <= {fail_until}) {{ \
           process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'Network error ... cURL error 28: Operation timed out after 15000ms',failure_classification:'provider'}})); \
         }} else {{ \
           process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'recovered'}})); \
         }}",
    ))
}

/// Node script that increments a counter file and always emits a permanent
/// auth/validation provider error. Used to prove permanent errors fail fast.
fn permanent_error_script(state_path: &Path) -> String {
    let state = state_path.to_string_lossy().replace('\\', "\\\\");
    script(&format!(
        "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); \
         let p='{state}'; let n=0; try {{ n=parseInt(fs.readFileSync(p,'utf8'))||0; }} catch(e) {{}} \
         n+=1; fs.writeFileSync(p, String(n)); \
         process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'401 Unauthorized: invalid token',failure_classification:'provider'}}));",
    ))
}

fn unique_state_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "homeboy-transient-retry-{}-{}-{}.count",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ))
}

#[test]
fn provider_retries_transient_error_then_succeeds() {
    let state_path = unique_state_path("recover");
    let _ = fs::remove_file(&state_path);
    let command = format!("node {}", transient_then_success_script(&state_path, 2));
    let (request, provider) = request("task-transient-recover", command);

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded,
        "transient blip should be retried until it recovers"
    );
    let attempts: u32 = fs::read_to_string(&state_path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or_default();
    assert_eq!(attempts, 3, "two transient failures plus one success");
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|d| d.class == "agent_task.provider_transient_retry"),
        "recovery should be surfaced as a diagnostic"
    );
    let _ = fs::remove_file(&state_path);
}

#[test]
fn provider_does_not_retry_permanent_error() {
    let state_path = unique_state_path("permanent");
    let _ = fs::remove_file(&state_path);
    let command = format!("node {}", permanent_error_script(&state_path));
    let (request, provider) = request("task-permanent", command);

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider),
        "permanent auth/validation failures stay non-retryable"
    );
    let attempts: u32 = fs::read_to_string(&state_path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or_default();
    assert_eq!(attempts, 1, "permanent error must fail fast, no retry");
    assert!(
        !outcome
            .diagnostics
            .iter()
            .any(|d| d.class == "agent_task.provider_transient_retry"),
        "permanent failures should not record retry history"
    );
    let _ = fs::remove_file(&state_path);
}

#[test]
fn provider_exhausts_bounded_transient_retries() {
    let state_path = unique_state_path("exhaust");
    let _ = fs::remove_file(&state_path);
    // Always transient: never recovers within the bounded attempt budget.
    let command = format!("node {}", transient_then_success_script(&state_path, 999));
    let (request, provider) = request("task-transient-exhaust", command);

    let outcome = run_provider_command(&request, &provider);

    assert_eq!(
        outcome.status,
        AgentTaskOutcomeStatus::ProviderError,
        "persistent transient failure still fails after the bounded budget"
    );
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Transient),
        "exhausted transient failures stay classified as transient/retryable"
    );
    let attempts: u32 = fs::read_to_string(&state_path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or_default();
    assert_eq!(
        attempts, PROVIDER_TRANSIENT_MAX_ATTEMPTS,
        "retry budget is bounded to PROVIDER_TRANSIENT_MAX_ATTEMPTS"
    );
    assert!(
        outcome.diagnostics.iter().any(|d| {
            d.class == "agent_task.provider_transient_retry"
                && d.data["retries_exhausted"] == json!(true)
        }),
        "exhaustion should be surfaced as a diagnostic"
    );
    let _ = fs::remove_file(&state_path);
}
