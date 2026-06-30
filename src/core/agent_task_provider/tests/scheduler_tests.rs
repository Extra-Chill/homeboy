use super::common::{request, script};
use super::*;

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
    request.executor.selector = Some("codex".to_string());
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
    assert!(aggregate.outcomes[0].diagnostics[0]
        .message
        .contains("nested AI runtime provider"));
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["hint"],
        "'codex' looks like a nested AI runtime provider, not a dispatch selector. --dispatch-selector selects the Homeboy executor provider id for backend 'synthetic-runtime'; pass the AI provider in --dispatch-provider-config instead."
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
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let plan=JSON.parse(process.env.{}); let mapped=(plan.env_name_mapping['test.provider']||[]).includes('{secret_name}'); let configured=(plan.status||[]).some((item)=>item.name==='{secret_name}'&&item.configured===true&&item.source==='env'); let leaked=JSON.stringify(plan).includes('hydrated-secret'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:mapped&&configured&&!leaked?'succeeded':'failed',summary:JSON.stringify(plan)}}));",
            crate::core::secret_env_plan::AGENT_TASK_SECRET_ENV_PLAN_JSON_ENV
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

fn plugin_component_contract(
    slug: &str,
    path: &std::path::Path,
) -> crate::core::agent_task::AgentTaskComponentContract {
    crate::core::agent_task::AgentTaskComponentContract {
        slug: Some(slug.to_string()),
        path: Some(path.display().to_string()),
        load_as: Some("plugin".to_string()),
        activate: Some(true),
        extra: Default::default(),
    }
}

fn staging_provider() -> AgentTaskExecutorProvider {
    let (_, mut provider) = request("staging", "noop".to_string());
    provider.runtime_contract.staging = AgentTaskRuntimeStagingContract {
        reconciled_packages: vec![AgentTaskRuntimeReconciledPackage {
            name: "acme/runtime-lib".to_string(),
            owner: Some("wordpress-7.0".to_string()),
            ..AgentTaskRuntimeReconciledPackage::default()
        }],
        ..AgentTaskRuntimeStagingContract::default()
    };
    provider
}

fn staging_plan(component: crate::core::agent_task::AgentTaskComponentContract) -> AgentTaskPlan {
    let (mut req, _) = request("staging", "noop".to_string());
    req.component_contracts = vec![component];
    AgentTaskPlan::new("plan-staging".to_string(), vec![req])
}

#[test]
fn plan_reconciliation_passes_when_no_staged_plugin_shadows_runtime() {
    let plugin = tempfile::tempdir().expect("plugin dir");
    let plan = staging_plan(plugin_component_contract("provider-plugin", plugin.path()));

    reconcile_staged_runtime_for_plan_with_providers(&plan, &[staging_provider()])
        .expect("clean staged plugin reconciles before dispatch");
}

#[test]
fn plan_reconciliation_refuses_shadowed_runtime_package_before_dispatch() {
    let plugin = tempfile::tempdir().expect("plugin dir");
    fs::create_dir_all(plugin.path().join("vendor/acme/runtime-lib"))
        .expect("create vendored runtime dir");
    let plan = staging_plan(plugin_component_contract("provider-plugin", plugin.path()));

    let err = reconcile_staged_runtime_for_plan_with_providers(&plan, &[staging_provider()])
        .expect_err("shadowed runtime package is refused before dispatch");

    assert_eq!(err.details["field"], "staged_plugin");
    assert!(err.message.contains("acme/runtime-lib"));
    assert!(err.message.contains("wordpress-7.0"));
    assert!(err.message.contains("provider-plugin"));
}

#[test]
fn plan_reconciliation_skips_provider_without_staging_contract() {
    let plugin = tempfile::tempdir().expect("plugin dir");
    fs::create_dir_all(plugin.path().join("vendor/acme/runtime-lib"))
        .expect("create vendored runtime dir");
    let plan = staging_plan(plugin_component_contract("provider-plugin", plugin.path()));
    // Default provider has an empty staging contract: nothing to reconcile.
    let (_, provider) = request("staging", "noop".to_string());

    reconcile_staged_runtime_for_plan_with_providers(&plan, &[provider])
        .expect("provider without a staging contract is a no-op");
}
