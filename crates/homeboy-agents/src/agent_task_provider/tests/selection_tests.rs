use super::common::request;
use super::*;

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
fn provider_nonzero_exit_preserves_bounded_redacted_process_diagnostics() {
    let secret_value = "super-secret-provider-token";
    std::env::set_var("HOMEBOY_TEST_PROVIDER_SECRET", secret_value);
    let fake = super::super::test_pi::FakePi::new(&format!(
        "{}\nprintf '%s\\n' 'Pi stderr: quota exhausted; token={secret_value}' >&2\nexit 1",
        super::super::test_pi::FakePi::answer("Pi CLI exited with status 1."),
    ));
    let mut request = fake.request("pi-failed");
    let provider = fake.provider();
    request.executor.secret_env = vec!["HOMEBOY_TEST_PROVIDER_SECRET".to_string()];

    let outcome = run_provider_command_once(&request, &provider);

    std::env::remove_var("HOMEBOY_TEST_PROVIDER_SECRET");
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "agent_task.provider_process_failed")
        .expect("process failure diagnostic");
    assert!(diagnostic.message.contains("pi.agent-task-executor"));
    assert!(diagnostic.message.contains("pi"));
    assert!(diagnostic.message.contains("status 1"));
    assert_eq!(diagnostic.data["provider"], "pi.agent-task-executor");
    assert_eq!(diagnostic.data["provider_backend"], "pi");
    assert_eq!(diagnostic.data["exit_code"], 1);
    let stderr = diagnostic.data["stderr"].as_str().expect("stderr tail");
    assert!(stderr.contains("quota exhausted"));
    assert!(stderr.contains("[redacted]"));
    assert!(!stderr.contains(secret_value));
    assert!(diagnostic.data["stdout"]
        .as_str()
        .expect("stdout tail")
        .contains("Pi CLI exited with status 1."));
    assert!(diagnostic.data["remediation_hints"]
        .as_array()
        .expect("remediation hints")
        .iter()
        .any(|hint| hint
            .as_str()
            .is_some_and(|value| value.contains("stdout/stderr"))));

    let aggregate = crate::agent_task_aggregate::AgentTaskAggregateReport::from(vec![outcome]);
    assert!(aggregate.tasks[0]
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "agent_task.provider_process_failed"));
}

#[test]
fn provider_selection_matches_exact_backend_first() {
    let (_, mut exact_provider) = request("task-a", "node exact-provider.js".to_string());
    exact_provider.id = "exact-provider".to_string();
    exact_provider.backend = "requested-backend".to_string();
    let (_, mut extension_provider) = request("task-b", "node extension-provider.js".to_string());
    extension_provider.id = "extension-provider".to_string();
    extension_provider.backend = "renamed-backend".to_string();

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
fn provider_command_receives_canonical_artifact_declarations() {
    let _home = super::common::artifact_home();
    let fake = super::super::test_pi::FakePi::success();
    let mut request = fake.request("task-artifact-normalization");
    request.expected_artifacts = vec!["patch".into()];
    request.normalize_artifact_declarations();
    let outcome = run_provider_command(&request, &fake.provider(), None);
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    let input = outcome
        .evidence_refs
        .iter()
        .find(|e| e.kind == "executor-input")
        .unwrap();
    let persisted: Value =
        serde_json::from_slice(&fs::read(input.uri.strip_prefix("file://").unwrap()).unwrap())
            .unwrap();
    assert_eq!(persisted["artifact_declarations"][0]["name"], "patch");
    assert_eq!(persisted["artifact_declarations"][0]["required"], true);
}

#[test]
fn provider_invocation_argv_and_cwd_preserve_paths_with_spaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime_dir = temp.path().join("runtime path with spaces");
    fs::create_dir_all(&runtime_dir).expect("runtime dir");
    let fake = super::super::test_pi::FakePi::new(&format!(
        "pwd > observed-cwd\n{}",
        super::super::test_pi::FakePi::answer("paths preserved")
    ));
    let command = runtime_dir.join("fake pi");
    fs::copy(&fake.command, &command).unwrap();
    let mut request = fake.request("task-invocation-cwd");
    request.executor.config["command"] = json!(command);
    request.workspace.root = Some(runtime_dir.display().to_string());
    let provider = fake.provider();

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(runtime_dir.join("observed-cwd"))
            .unwrap()
            .trim(),
        runtime_dir.canonicalize().unwrap().to_str().unwrap()
    );
}

fn extension_policy(id: &str, backend: &str) -> homeboy_extension_contract::ExtensionManifest {
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
