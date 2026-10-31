use super::super::test_pi::FakePi;
use super::common::{artifact_home, request, script};
use super::*;
use crate::agent_task::AgentTaskOutputDeclaration;

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
        "invocation": { "argv": ["minimal-provider"] }
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
        "argv": ["{{extension_path}}/bin/provider", "--runtime", "{{runtime_path}}"]
    }))
    .expect("provider manifest");

    assert!(provider.command.is_empty());
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
fn provider_manifest_defaults_executable_readiness_env_when_omitted() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "path-runtime.agent-task-executor",
        "backend": "path-runtime",
        "invocation": { "argv": ["node", "{{runtime_path}}/scripts/agent/homeboy-path-runtime-agent-task-executor.cjs"] },
        "runner_readiness": [{
            "id": "path-runtime.executable",
            "label": "PATH runtime executable",
            "executable": {
                "candidates": ["path-runtime"],
                "version_command": ["--version"],
                "install_hint": "Install path-runtime so it is available on PATH."
            }
        }]
    }))
    .expect("provider manifest");

    let executable = provider.runner_readiness[0]
        .executable
        .as_ref()
        .expect("executable readiness");
    assert!(executable.env.is_empty());
    assert_eq!(executable.candidates, vec!["path-runtime".to_string()]);
    assert_eq!(executable.version_command, vec!["--version".to_string()]);
    assert_eq!(
        executable.install_hint.as_deref(),
        Some("Install path-runtime so it is available on PATH.")
    );

    let exported = serde_json::to_value(&provider).expect("provider export");
    assert!(
        exported["runner_readiness"][0]["executable"]
            .get("env")
            .is_none(),
        "empty env should remain omitted on export: {exported:?}"
    );
}

#[test]
fn provider_missing_chains_matching_runtime_discovery_diagnostics() {
    let _home = artifact_home();
    let (mut request, _provider) = request("missing-pi", "unused".to_string());
    request.executor.backend = "pi".to_string();
    let executor = ExtensionProviderAgentTaskExecutor::from_catalog(AgentTaskProviderCatalog {
        providers: Vec::new(),
        diagnostics: vec![AgentRuntimeDiscoveryDiagnostic {
            class: "agent_runtime_manifest.schema_mismatch".to_string(),
            message: "invalid type: map, expected string at line 12 column 18".to_string(),
            runtime_id: Some("pi".to_string()),
            extension_id: None,
            path: Some("/tmp/pi/pi.json".to_string()),
        }],
        version: None,
    });

    let outcome = executor.execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "test-plan".to_string(),
            run_id: None,
            attempt: 1,
            cancellation: AgentTaskCancellationToken::default(),
            lifecycle_store: None,
            provider_capacity_key: None,
        },
    );

    let summary = outcome.summary.as_deref().expect("summary");
    assert!(summary.contains("no extension agent-task provider found for backend 'pi'"));
    assert!(summary.contains("agent_runtime_manifest.schema_mismatch"));
    assert!(summary.contains("invalid type: map, expected string at line 12 column 18"));
    assert_eq!(outcome.diagnostics[0].class, "agent_task.provider_missing");
    assert_eq!(
        outcome.diagnostics[0].data["runtime_discovery_diagnostics"][0]["class"],
        "agent_runtime_manifest.schema_mismatch"
    );
}

#[test]
fn provider_command_parts_uses_argv() {
    let (_request, provider) = request("task-legacy-command", "legacy-provider --flag".to_string());

    let (program, args, cwd) = provider_command_parts(&provider).expect("command parts");

    assert_eq!(program, "legacy-provider");
    assert_eq!(args, vec!["--flag"]);
    assert_eq!(cwd, None);
}

#[test]
fn provider_readiness_invocation_defaults_and_validates_its_total_timeout() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "ready.provider",
        "backend": "ready",
        "readiness_invocation": { "argv": ["ready-provider"] }
    }))
    .expect("provider manifest");

    assert_eq!(
        provider
            .readiness_invocation
            .as_ref()
            .expect("readiness invocation")
            .timeout_ms,
        homeboy_extension_contract::agent_task_executor_declaration::DEFAULT_PROVIDER_READINESS_INVOCATION_TIMEOUT_MS
    );
    assert_eq!(
        serde_json::to_value(&provider).expect("provider export")["readiness_invocation"]
            ["timeout_ms"],
        json!(20_000)
    );

    let error = serde_json::from_value::<AgentTaskExecutorProvider>(json!({
        "id": "invalid.provider",
        "backend": "invalid",
        "readiness_invocation": { "argv": ["invalid-provider"], "timeout_ms": 120001 }
    }))
    .expect_err("timeout above the core maximum must fail");
    assert!(error
        .to_string()
        .contains("readiness_invocation.timeout_ms"));
}

#[test]
fn workspace_permission_root_capability_serializes_the_exact_task_workspace() {
    let _home = artifact_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("promotion-target");
    let candidate = temp.path().join("attempt-candidate");
    std::fs::create_dir_all(&original).expect("original workspace");
    std::fs::create_dir_all(&candidate).expect("candidate workspace");
    let fake = FakePi::new(&format!(
        "pwd > provider-observation.txt\n{}",
        FakePi::answer("workspace checked")
    ));
    let mut request = fake.request("workspace-boundary");
    let mut provider = fake.provider();
    provider
        .capabilities
        .push(AGENT_TASK_PROVIDER_CAPABILITY_WORKSPACE_PERMISSION_ROOT_V1.to_string());
    request.workspace.root = Some(candidate.display().to_string());
    request.executor.config["workspace"] = json!({ "root": candidate });
    request.executor.config["workspace_root"] = json!(candidate);

    let outcome = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]).execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874"
                .to_string(),
            run_id: Some(
                "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874".to_string(),
            ),
            attempt: 1,
            cancellation: Default::default(),
            lifecycle_store: None,
            provider_capacity_key: None,
        },
    );
    assert_eq!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded,
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(candidate.join("provider-observation.txt"))
            .unwrap()
            .trim(),
        candidate.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(!original.join("provider-observation.txt").exists());
    // Pi no longer consumes a JSON request on stdin. Inspect the persisted
    // Homeboy request instead to verify the scheduler-selected boundary.
    let request_path = outcome
        .evidence_refs
        .iter()
        .find(|e| e.kind == "executor-input")
        .unwrap();
    let persisted: Value = serde_json::from_slice(
        &std::fs::read(request_path.uri.strip_prefix("file://").unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["workspace"]["root"], json!(candidate));
    assert_eq!(
        persisted["executor"]["config"]["workspace_permission_root"],
        json!(candidate)
    );
    assert_eq!(
        persisted["executor"]["config"]["workspace"]["root"],
        json!(candidate)
    );
    assert_eq!(
        persisted["executor"]["config"]["workspace_root"],
        json!(candidate)
    );
}

#[test]
fn runtime_tool_readiness_obeys_the_command_policy() {
    let _home = artifact_home();
    let script = script("process.exit(0);");
    let (mut request, provider) = request("runtime-tool-policy", format!("node {script}"));
    request.runtime_tools = vec![serde_json::from_value(json!({
        "id": "fixture.mcp",
        "command": ["node", script],
        "readiness": { "version_command": ["--version"] }
    }))
    .expect("runtime tool")];
    request.policy.tools.tools.insert(
        "fixture.mcp".to_string(),
        crate::agent_task::AgentToolPolicyRule {
            execution_location: crate::agent_task::AgentToolExecutionLocation::Runner,
            timeout_ms: None,
            reason: None,
        },
    );
    request.policy.tools.commands = serde_json::from_value(json!({
        "mode": "deny_list",
        "deny": [{ "pattern": "*node * --version" }]
    }))
    .expect("command policy");

    let outcome = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]).execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "runtime-tool-policy-plan".to_string(),
            run_id: None,
            attempt: 1,
            cancellation: Default::default(),
            lifecycle_store: None,
            provider_capacity_key: None,
        },
    );

    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.runtime_tool_command_denied"
    );
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
}

#[test]
fn runtime_tool_capability_probe_obeys_the_command_policy() {
    let _home = artifact_home();
    let script = script("process.exit(0);");
    let (mut request, provider) =
        request("runtime-tool-capability-policy", format!("node {script}"));
    request.runtime_tools = vec![serde_json::from_value(json!({
        "id": "fixture.mcp",
        "command": ["node", script],
        "required_capabilities": ["fixture"],
        "readiness": { "capability_probe": { "argv": ["--capability-probe"] } }
    }))
    .expect("runtime tool")];
    request.policy.tools.tools.insert(
        "fixture.mcp".to_string(),
        crate::agent_task::AgentToolPolicyRule {
            execution_location: crate::agent_task::AgentToolExecutionLocation::Runner,
            timeout_ms: None,
            reason: None,
        },
    );
    request.policy.tools.commands = serde_json::from_value(json!({
        "mode": "deny_list",
        "deny": [{ "pattern": "*node * --capability-probe" }]
    }))
    .expect("command policy");

    let outcome = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]).execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "runtime-tool-capability-policy-plan".to_string(),
            run_id: None,
            attempt: 1,
            cancellation: Default::default(),
            lifecycle_store: None,
            provider_capacity_key: None,
        },
    );

    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.runtime_tool_command_denied"
    );
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
}

#[test]
fn provider_validates_declared_outputs() {
    let fake = FakePi::new(&FakePi::answer(
        r#"{"outputs":{"inspection":{"verdict":"pass"}}}"#,
    ));
    let mut request = fake.request("declared-output");
    let provider = fake.provider();
    request.output_declarations = vec![AgentTaskOutputDeclaration {
        name: "inspection".to_string(),
        required: true,
        schema: "example/inspection/v1".to_string(),
        structural_schema: json!({
            "type": "object",
            "required": ["verdict"],
            "properties": { "verdict": { "type": "string" } }
        }),
        max_bytes: Some(64),
        evidence_relationship: None,
    }];

    let valid = run_provider_command_once(&request, &provider);
    assert_eq!(valid.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(valid.outputs["inspection"]["verdict"], "pass");

    request.output_declarations[0].evidence_relationship =
        Some(crate::agent_task::AgentTaskOutputEvidenceRelationship {
            relationship: "supported_by".into(),
            evidence: AgentTaskEvidenceRef {
                kind: "transcript".into(),
                uri: "file:///provider-transcript.log".into(),
                label: None,
            },
        });
    let missing_evidence = run_provider_command_once(&request, &provider);
    assert_eq!(
        missing_evidence.status,
        AgentTaskOutcomeStatus::CandidateRecoverable
    );
    assert!(missing_evidence
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.class == "agent_task.output_evidence_missing" }));
    request.output_declarations[0].evidence_relationship = None;

    let missing_pi = FakePi::new(&FakePi::answer(r#"{"outputs":{"other":"present"}}"#));
    request.executor.config = missing_pi.config();
    let missing = run_provider_command_once(&request, &provider);
    assert_eq!(missing.status, AgentTaskOutcomeStatus::Failed);
    assert_eq!(
        missing.failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
    assert!(missing
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "pi.required_outputs_missing"));

    let malformed_pi = FakePi::new(&FakePi::answer(
        r#"{"outputs":{"inspection":{"verdict":42}}}"#,
    ));
    request.executor.config = malformed_pi.config();
    let malformed = run_provider_command_once(&request, &provider);
    assert_eq!(
        malformed.status,
        AgentTaskOutcomeStatus::CandidateRecoverable
    );
    assert!(malformed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "agent_task.output_malformed"));

    let oversized_pi = FakePi::new(&FakePi::answer(
        &json!({"outputs":{"inspection":{"verdict":"pass", "detail":"x".repeat(100)}}}).to_string(),
    ));
    request.executor.config = oversized_pi.config();
    let oversized = run_provider_command_once(&request, &provider);
    assert_eq!(
        oversized.status,
        AgentTaskOutcomeStatus::CandidateRecoverable
    );
    assert!(oversized
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.class == "agent_task.output_oversized"));

    request.inputs = json!("caller-owned scalar input");
    let invalid_inputs = run_provider_command_once(&request, &provider);
    assert_eq!(invalid_inputs.status, AgentTaskOutcomeStatus::Failed);
    assert!(invalid_inputs.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "agent_task.output_declaration_invalid"
            && diagnostic
                .message
                .contains("output declarations require object task inputs")
    }));
}

#[cfg(unix)]
fn workspace_attestation(path: &std::path::Path) -> Value {
    use std::os::unix::fs::MetadataExt;

    let canonical = std::fs::canonicalize(path).expect("canonical workspace");
    let metadata = std::fs::symlink_metadata(&canonical).expect("workspace metadata");
    let git_file = canonical.join(".git");
    let git_content = std::fs::read_to_string(&git_file).expect("linked git file");
    let gitdir_target = git_content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok())
        .expect("canonical gitdir target");
    serde_json::json!({
        "canonical_path": canonical,
        "device": metadata.dev(),
        "inode": metadata.ino(),
        "git_file_is_file": true,
        "git_file_content": git_content,
        "gitdir_target": gitdir_target,
    })
}

#[cfg(unix)]
fn linked_workspace(temp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let workspace = temp.path().join("workspace");
    let gitdir = temp.path().join("gitdir");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&gitdir).expect("gitdir");
    std::fs::write(workspace.join(".git"), "gitdir: ../gitdir\n").expect("linked git file");
    (workspace, gitdir)
}

#[cfg(unix)]
#[test]
fn provider_attests_worktree_root_and_executes_nested_component() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (workspace, _) = linked_workspace(&temp);
    let component = workspace.join("packages/component");
    std::fs::create_dir_all(&component).expect("component directory");
    let attestation = workspace_attestation(&workspace);
    let fake = FakePi::new(&format!(
        "pwd > provider-observation.txt\n{}",
        FakePi::answer("nested component executed")
    ));
    let mut request = fake.request("nested-component");
    let provider = fake.provider();
    request.workspace.root = Some(workspace.display().to_string());
    request.executor.config["component_cwd"] = json!("packages/component");
    request.metadata = json!({ "cook_attempt_workspace_identity": attestation });

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(
        std::fs::read_to_string(component.join("provider-observation.txt"))
            .unwrap()
            .trim(),
        component.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(!workspace.join("provider-observation.txt").exists());
}

#[cfg(unix)]
#[test]
fn provider_refuses_legacy_source_attestation_git_file_replaced_before_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (workspace, _) = linked_workspace(&temp);
    let attestation = workspace_attestation(&workspace);
    let marker = workspace.join("provider-started");
    let script = script(&format!(
        "require('fs').writeFileSync({:?}, 'started')",
        marker.display().to_string()
    ));
    let (mut request, provider) = request("attested-workspace", format!("node {script}"));
    request.workspace.root = Some(workspace.display().to_string());
    request.metadata = serde_json::json!({ "cook_workspace_identity": attestation });

    std::fs::remove_file(workspace.join(".git")).expect("remove linked git file");
    std::fs::create_dir(workspace.join(".git")).expect("replace with git directory");

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.workspace_git_representation_changed"
    );
    assert!(!marker.exists(), "provider command must not start");
}

#[cfg(unix)]
#[test]
fn provider_refuses_linked_git_target_swap_before_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (workspace, _) = linked_workspace(&temp);
    let attestation = workspace_attestation(&workspace);
    let marker = workspace.join("provider-started");
    let script = script(&format!(
        "require('fs').writeFileSync({:?}, 'started')",
        marker.display().to_string()
    ));
    let (mut request, provider) = request("attested-workspace", format!("node {script}"));
    request.workspace.root = Some(workspace.display().to_string());
    request.metadata = serde_json::json!({ "cook_attempt_workspace_identity": attestation });

    std::fs::create_dir_all(temp.path().join("replacement-gitdir")).expect("replacement gitdir");
    std::fs::write(workspace.join(".git"), "gitdir: ../replacement-gitdir\n")
        .expect("replace linked git target");

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.workspace_identity_changed"
    );
    assert!(!marker.exists(), "provider command must not start");
}

#[test]
fn provider_manifest_rejects_deprecated_string_command() {
    let error = serde_json::from_value::<AgentTaskExecutorProvider>(json!({
        "id": "legacy.provider",
        "backend": "legacy",
        "command": "legacy-provider --flag"
    }))
    .expect_err("string-form command must be rejected");

    assert!(error.to_string().contains(
        "agent-task provider string-form 'command' is no longer supported; use invocation.argv or argv instead"
    ));
}

#[test]
fn provider_manifest_preserves_unknown_metadata_on_export() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "metadata.provider",
        "backend": "metadata",
        "invocation": { "argv": ["metadata-provider"] },
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

    let policy: crate::agent_task::AgentToolPolicy = serde_json::from_str(
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

/// The command policy must reach the provider process through the same env
/// handshake the tool policy already uses, so a runtime that calls back into
/// `homeboy agent-task tool dispatch` inherits the refusals (#11481).
#[test]
fn provider_command_env_carries_the_command_policy() {
    let (mut request, provider) = request("task-1", "minimal-provider".to_string());
    request.policy.tools.commands = crate::agent_task::AgentCommandPolicy {
        deny: vec![crate::agent_task::AgentCommandRule::with_reason(
            "cargo test",
            "this host routes builds to CI",
        )],
        ..crate::agent_task::AgentCommandPolicy::default()
    };

    let env = provider_command_env(&request, &provider).expect("provider env");
    let env: BTreeMap<String, String> = env.into_iter().collect();

    let policy: crate::agent_task::AgentToolPolicy = serde_json::from_str(
        env.get("HOMEBOY_AGENT_TOOL_POLICY_JSON")
            .expect("tool policy env"),
    )
    .expect("tool policy json");

    let denial = policy
        .evaluate_command("timeout 1200 cargo test -p homeboy-agents")
        .denial()
        .cloned()
        .expect("denied");
    assert_eq!(denial.reason, "this host routes builds to CI");
}

#[test]
fn provider_command_env_strips_git_push_credentials() {
    // The provider environment must deny git push credentials so an executor
    // cannot push a candidate to a real remote before verification. (#8486)
    let (request, provider) = request("task-1", "minimal-provider".to_string());
    let env = provider_command_env(&request, &provider).expect("provider env");
    let env: BTreeMap<String, String> = env.into_iter().collect();

    assert_eq!(
        env.get("GIT_ASKPASS").map(String::as_str),
        Some("/bin/false")
    );
    assert_eq!(
        env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        env.get("GIT_CONFIG_NOSYSTEM").map(String::as_str),
        Some("1")
    );
    // An empty `credential.helper` via git's environment config resets any
    // inherited helper list, so a cached-credential helper cannot supply a token.
    assert_eq!(env.get("GIT_CONFIG_COUNT").map(String::as_str), Some("1"));
    assert_eq!(
        env.get("GIT_CONFIG_KEY_0").map(String::as_str),
        Some("credential.helper")
    );
    assert_eq!(env.get("GIT_CONFIG_VALUE_0").map(String::as_str), Some(""));
}

#[cfg(unix)]
#[test]
fn pi_backend_spawns_pi_directly_without_node_or_stdin_request() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let pi = temp.path().join("pi");
    let observed = temp.path().join("observed");
    std::fs::write(&pi, format!(r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'pi 1.0'; exit; fi
if [ "$1" = --list-models ]; then echo 'roadie test'; exit; fi
printf '%s\n' "$@" > '{}'
if read input; then exit 77; fi
printf '%s\n' '{{"type":"message_end","message":{{"role":"assistant","stopReason":"stop","content":[{{"type":"text","text":"Native Pi completed."}}]}}}}'
"#, observed.display())).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _home = artifact_home();
    let (mut request, _) = request("native-pi", String::new());
    let provider = discovery::discover_agent_task_executor_providers().remove(0);
    assert!(provider.extension_id.is_none());
    assert!(provider.runtime_path.is_none());
    request.executor.backend = "pi".into();
    request.executor.config = json!({"command":pi,"model":"roadie/test"});
    request.workspace.root = Some(temp.path().display().to_string());
    std::fs::write(temp.path().join("report.txt"), "retained report").unwrap();
    request.artifact_declarations = vec![serde_json::from_value(
        json!({"name":"report", "path":"report.txt", "required":true}),
    )
    .unwrap()];
    let result = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]).execute(
        request,
        AgentTaskExecutionContext {
            plan_id: "native-pi".into(),
            run_id: None,
            attempt: 1,
            cancellation: Default::default(),
            lifecycle_store: None,
            provider_capacity_key: None,
        },
    );
    assert_eq!(
        result.status,
        AgentTaskOutcomeStatus::Succeeded,
        "{result:?}"
    );
    assert_eq!(result.summary.as_deref(), Some("Native Pi completed."));
    let artifact = result
        .artifacts
        .iter()
        .find(|a| a.name.as_deref() == Some("report"))
        .expect("retained declared file");
    assert_eq!(
        std::fs::read_to_string(artifact.path.as_ref().unwrap()).unwrap(),
        "retained report"
    );
    let observed = std::fs::read_to_string(observed).unwrap();
    assert!(observed.starts_with("--mode\njson\n--session\n"));
    assert!(observed.contains("native-pi-pi-session.jsonl\n--model\nroadie/test\n"));
}
