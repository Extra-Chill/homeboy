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
fn default_provider_catalog_reads_codex_invocation_argv() {
    crate::test_support::with_isolated_home(|home| {
        let runtime_dir = home.path().join(".config/homeboy/agent-runtimes/codex");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::write(
            runtime_dir.join("codex.json"),
            json!({
                "schema": crate::agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                "id": "codex",
                "agent_task_executors": [{
                    "schema": AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
                    "id": "codex.agent-task-executor",
                    "backend": "codex",
                    "invocation": {
                        "schema": crate::command_invocation::COMMAND_INVOCATION_SCHEMA,
                        "argv": ["codex-agent-task-executor", "--json"]
                    }
                }]
            })
            .to_string(),
        )
        .expect("runtime manifest");

        let catalog = AgentTaskProviderCatalog::discover();
        let provider = catalog
            .providers()
            .iter()
            .find(|provider| provider.id == "codex.agent-task-executor")
            .expect("codex provider discovered");

        assert!(provider.command.is_empty());
        assert!(provider.command_argv.is_empty());
        assert_eq!(
            provider.invocation.schema.as_deref(),
            Some(crate::command_invocation::COMMAND_INVOCATION_SCHEMA)
        );
        assert_eq!(
            provider.invocation.argv,
            vec![
                "codex-agent-task-executor".to_string(),
                "--json".to_string()
            ]
        );

        let (program, args, cwd) = provider_command_parts(provider).expect("command parts");
        assert_eq!(program, "codex-agent-task-executor");
        assert_eq!(args, vec!["--json"]);
        assert_eq!(cwd, None);
    });
}

#[test]
fn provider_missing_chains_matching_runtime_discovery_diagnostics() {
    let (mut request, _provider) = request("missing-opencode", "unused".to_string());
    request.executor.backend = "opencode".to_string();
    let executor = ExtensionProviderAgentTaskExecutor::from_catalog(AgentTaskProviderCatalog {
        providers: Vec::new(),
        diagnostics: vec![AgentRuntimeDiscoveryDiagnostic {
            class: "agent_runtime_manifest.schema_mismatch".to_string(),
            message: "invalid type: map, expected string at line 12 column 18".to_string(),
            runtime_id: Some("opencode".to_string()),
            extension_id: None,
            path: Some("/tmp/opencode/opencode.json".to_string()),
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
        },
    );

    let summary = outcome.summary.as_deref().expect("summary");
    assert!(summary.contains("no extension agent-task provider found for backend 'opencode'"));
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
fn opencode_provider_boundary_uses_task_workspace_for_cwd_and_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("promotion-target");
    let candidate = temp.path().join("attempt-candidate");
    std::fs::create_dir_all(&original).expect("original workspace");
    std::fs::create_dir_all(&candidate).expect("candidate workspace");
    let script = script(
        r#"const fs = require('fs');
let input = '';
process.stdin.on('data', (chunk) => input += chunk);
process.stdin.on('end', () => {
  const request = JSON.parse(input);
  fs.writeFileSync('provider-observation.json', JSON.stringify({
    cwd: process.cwd(),
    workspace: request.workspace.root,
    config_workspace: request.executor.config.workspace.root,
    config_workspace_root: request.executor.config.workspace_root
  }));
  process.stderr.write('permission denied');
});"#,
    );
    let (mut request, mut provider) = request("opencode-boundary", format!("node {script}"));
    provider.id = "opencode.agent-task-executor".to_string();
    provider.backend = "opencode".to_string();
    request.executor.backend = "opencode".to_string();
    request.workspace.root = Some(candidate.display().to_string());
    request.executor.config = json!({
        "workspace": { "root": candidate.display().to_string() },
        "workspace_root": candidate.display().to_string(),
    });

    let outcome = run_provider_command_once(&request, &provider);
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class, "agent_task.provider_empty_stdout",
        "{:?}",
        outcome.diagnostics
    );
    assert!(
        candidate.join("provider-observation.json").is_file(),
        "candidate missing provider observation; original exists: {}; diagnostics: {:?}",
        original.join("provider-observation.json").is_file(),
        outcome.diagnostics
    );
    let observation: Value = serde_json::from_slice(
        &std::fs::read(candidate.join("provider-observation.json")).expect("provider observation"),
    )
    .expect("observation json");

    assert_eq!(
        observation["cwd"],
        std::fs::canonicalize(&candidate)
            .expect("canonical candidate")
            .display()
            .to_string()
    );
    assert_eq!(observation["workspace"], candidate.display().to_string());
    assert_eq!(
        observation["config_workspace"],
        candidate.display().to_string()
    );
    assert_eq!(
        observation["config_workspace_root"],
        candidate.display().to_string()
    );
    assert!(!original.join("provider-observation.json").exists());
}

#[test]
fn executor_git_boundary_blocks_commit_and_push_but_allows_diff_and_status() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test User"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .status()
            .expect("git setup")
            .success());
    }
    std::fs::write(workspace.join("tracked.txt"), "base\n").expect("base file");
    assert!(std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&workspace)
        .status()
        .expect("stage base")
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(&workspace)
        .status()
        .expect("commit base")
        .success());
    std::fs::write(workspace.join("tracked.txt"), "candidate\n").expect("candidate file");

    let script = script(
        r#"const { spawnSync } = require('child_process');
const run = (args) => spawnSync('git', args, { encoding: 'utf8' });
const status = run(['status', '--short']);
const diff = run(['diff', '--', 'tracked.txt']);
const commit = run(['commit', '-am', 'executor commit']);
const push = run(['push', 'origin', 'HEAD']);
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: 'git-boundary',
  status: 'succeeded',
  artifacts: [],
  typed_artifacts: [],
  evidence_refs: [],
  diagnostics: [],
  outputs: { status: status.stdout, diff: diff.stdout, commit: commit.status, push: push.status }
}));"#,
    );
    let (mut request, provider) = request("git-boundary", format!("node {script}"));
    request.workspace.root = Some(workspace.display().to_string());

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome.outputs["status"]
        .as_str()
        .expect("status output")
        .contains("tracked.txt"));
    assert!(outcome.outputs["diff"]
        .as_str()
        .expect("diff output")
        .contains("-base"));
    assert_eq!(outcome.outputs["commit"], 126);
    assert_eq!(outcome.outputs["push"], 126);
    assert_eq!(
        std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&workspace)
            .output()
            .expect("count commits")
            .stdout,
        b"1\n"
    );
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
fn runtime_contract_normalizes_provider_outputs_to_canonical_artifacts() {
    let artifact_root = std::env::temp_dir()
        .join("homeboy-agent-task-provider-tests")
        .join("task-runtime-normalization");
    std::fs::create_dir_all(&artifact_root).expect("create executor artifact root");
    let patch = artifact_root.join("runtime.patch");
    let report = artifact_root.join("report.json");
    std::fs::write(&patch, "patch bytes").expect("write patch");
    std::fs::write(&report, "{}").expect("write report");
    let provider_output = json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-runtime-normalization",
        "status": "succeeded",
        "outputs": {
            "runtime": {
                "status": "done",
                "summary": "runtime finished",
                "artifacts": {
                    "patch": patch,
                    "report": { "path": report }
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
        preflight_checks: Vec::new(),
    };

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(outcome.summary.as_deref(), Some("runtime finished"));
    assert_eq!(outcome.artifacts.len(), 2);
    assert_eq!(outcome.artifacts[0].kind, "patch");
    assert_eq!(outcome.artifacts[0].metadata["review_only"], true);
    assert_eq!(outcome.artifacts[1].kind, "report");
    assert_eq!(outcome.artifacts[1].metadata["review_only"], true);
    assert_eq!(outcome.artifacts[0].size_bytes, None);
    assert_eq!(outcome.artifacts[0].sha256, None);
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

    let outcome = run_provider_command(&request, &provider, None);

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
