use super::common::{request, script};
use super::*;

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
    provider_a.lab_runtime_components =
        vec!["agents-api".to_string(), "sample-component".to_string()];
    let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    request_b.executor.selector = Some("provider-b".to_string());
    provider_b.id = "provider-b".to_string();
    provider_b.lab_runtime_components =
        vec!["sample-component".to_string(), "php-ai-client".to_string()];
    let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

    let component_ids = executor.lab_runtime_component_ids_for_plan(&AgentTaskPlan::new(
        "plan-a",
        vec![request_a, request_b],
    ));

    // `lab_runtime_component_ids_for_plan` dedups across the selected providers
    // into a `BTreeSet`, so the result is deterministically sorted. provider_a
    // and provider_b both contribute `sample-component`, which collapses to one
    // entry. (#6705's rename of `data-machine` -> `sample-component` left the
    // expected literal in the old, no-longer-sorted order.)
    assert_eq!(
        component_ids,
        vec!["agents-api", "php-ai-client", "sample-component"]
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
            run_id: Some("gate-run".to_string()),
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
            selector_hint: None,
        }
    );
}

#[test]
fn provider_readiness_selector_mismatch_explains_runtime_provider_confusion() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.id = "example.sandbox-agent-task-executor".to_string();
    provider.backend = "sandbox".to_string();
    provider.cli.reserved_selector_hints = vec!["codex".to_string()];

    let error = validate_provider_runner_readiness_for_backend_with_providers(
        &[provider],
        "sandbox",
        Some("codex"),
    )
    .expect_err("selector mismatch should fail before runner readiness");

    assert_eq!(error.details["field"], "selector");
    let suggestions = error.details["tried"]
        .as_array()
        .expect("tried suggestions");
    assert!(suggestions.iter().any(|value| value
        .as_str()
        .is_some_and(|suggestion| suggestion.contains("runtime-specific provider configuration"))));
    assert!(suggestions.iter().any(|value| value
        .as_str()
        .is_some_and(|suggestion| suggestion.contains("example.sandbox-agent-task-executor"))));
}

#[test]
fn provider_readiness_rejects_invalid_immediate_failure_configuration_before_execution() {
    let (_, mut provider) = request("task-a", "node provider.js".to_string());
    provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
        id: "invalid".to_string(),
        error_contains_any: vec!["server error".to_string()],
        retryable: true,
        error_ref_pattern: Some("[".to_string()),
        log_lookup: None,
        fallback_action: None,
    }];

    let error =
        validate_provider_runner_readiness_for_backend_with_providers(&[provider], "test", None)
            .expect_err("invalid adapter configuration must fail pre-dispatch");

    assert_eq!(error.details["field"], "immediate_failure_patterns");
    assert!(error.message.contains("invalid error_ref_pattern"));
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
        "invocation": { "argv": ["custom-agent-task"] },
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
fn provider_manifest_parses_runner_and_dependency_contracts() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-executor-provider/v1",
        "id": "custom.provider",
        "backend": "custom",
        "invocation": { "argv": ["custom-agent-task"] },
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
        "lab_runtime_components": ["agents-api", "sample-component"]
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
        vec!["agents-api", "sample-component"]
    );
}

fn extension_policy(id: &str, backend: &str) -> extension::ExtensionManifest {
    serde_json::from_value(json!({
        "name": id,
        "version": "1.0.0",
        "agent_task": { "default_backend": backend }
    }))
    .expect("extension manifest")
}

#[test]
fn provider_declarations_do_not_select_default_backend() {
    let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
    provider_a.backend = "first".to_string();
    let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
    provider_b.backend = "preferred".to_string();
    provider_b.default_backend = true;

    let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

    assert_eq!(
        executor
            .default_backend_with_policy(Vec::new(), &defaults::HomeboyConfig::default())
            .unwrap(),
        None
    );
}

#[test]
fn default_backend_uses_global_config_policy() {
    let config = defaults::HomeboyConfig {
        agent_task: defaults::AgentTaskConfig {
            default_backend: Some("configured".to_string()),
            ..defaults::AgentTaskConfig::default()
        },
        ..defaults::HomeboyConfig::default()
    };

    assert_eq!(
        default_backend_from_policy_sources(None, Vec::new(), || {
            config.agent_task.default_backend.clone()
        })
        .unwrap()
        .as_deref(),
        Some("configured")
    );
}

#[test]
fn default_backend_uses_extension_policy() {
    assert_eq!(
        default_backend_from_policy_sources(
            None,
            vec![extension_policy("Runtime Extension", "extension-policy")],
            || panic!("global config should remain lazy"),
        )
        .unwrap()
        .as_deref(),
        Some("extension-policy")
    );
}

#[test]
fn default_backend_rejects_ambiguous_extension_policy() {
    let error = default_backend_from_policy_sources(
        None,
        vec![
            extension_policy("runtime-a", "backend-a"),
            extension_policy("runtime-b", "backend-b"),
        ],
        || None,
    )
    .expect_err("ambiguous policy should fail");

    assert!(error.message.contains("ambiguous"));
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
