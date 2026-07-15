use super::common::{request, script};
use super::*;
use crate::core::agent_task_scheduler::{
    AgentTaskAggregateStatus, AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
};
use std::sync::Mutex;

static DEFAULT_TIMEOUT_ENV_LOCK: Mutex<()> = Mutex::new(());

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
fn executor_materializes_runner_local_artifacts_for_no_op_and_editing_requests() {
    let runner_root = crate::core::artifacts::root().expect("runner artifact root");
    {
        let controller_root = tempfile::tempdir().expect("controller root");
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let path=require('path'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let valid=path.isAbsolute(req.artifacts_path)&&fs.statSync(req.artifacts_path).isDirectory()&&req.artifacts_path_provenance.owner==='homeboy'&&req.artifacts_path_provenance.locality==='runner'&&!req.artifacts_path.startsWith(req.executor.config.controller_root); fs.writeFileSync(path.join(req.artifacts_path, req.task_id+'.txt'),'captured'); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:valid?(req.executor.config.no_op?'no_op':'succeeded'):'failed',summary:req.artifacts_path,artifacts:[]}));")
        );
        let (mut no_op, provider) = request("task-no-op", command);
        no_op.executor.config = json!({
            "controller_root": controller_root.path(),
            "no_op": true
        });
        let mut editing = no_op.clone();
        editing.task_id = "task-editing".to_string();
        editing.executor.config["no_op"] = json!(false);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]))
            .with_run_id("runner-local-artifact-run");

        let aggregate = scheduler.run(AgentTaskPlan::new(
            "runner-local-artifact-plan",
            vec![no_op, editing],
        ));

        assert_eq!(aggregate.outcomes[0].status, AgentTaskOutcomeStatus::NoOp);
        assert_eq!(
            aggregate.outcomes[1].status,
            AgentTaskOutcomeStatus::Succeeded
        );
        for outcome in &aggregate.outcomes {
            let path = PathBuf::from(outcome.summary.as_deref().expect("artifacts path"));
            assert!(path.starts_with(&runner_root));
            assert!(!path.starts_with(controller_root.path()));
            assert!(path.join(format!("{}.txt", outcome.task_id)).is_file());
        }
        assert_ne!(aggregate.outcomes[0].summary, aggregate.outcomes[1].summary);
    }
}

#[test]
fn executor_artifact_paths_are_distinct_per_run() {
    {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:req.artifacts_path}));")
        );
        let (request, provider) = request("same-task", command);
        let first =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider.clone(),
            ]))
            .with_run_id("run-one")
            .run(AgentTaskPlan::new("plan", vec![request.clone()]));
        let second =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]))
            .with_run_id("run-two")
            .run(AgentTaskPlan::new("plan", vec![request]));

        assert_ne!(first.outcomes[0].summary, second.outcomes[0].summary);
    }
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
    provider.cli.reserved_selector_hints = vec!["codex".to_string()];
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
        .contains("matched selector"));
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["hint"],
        "'codex' is declared by an executor provider as runtime-specific provider configuration, not a dispatch selector. --dispatch-selector selects the Homeboy executor provider id for backend 'synthetic-runtime'; pass runtime/provider configuration through --dispatch-provider-config instead."
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
fn provider_empty_stdout_captures_bounded_stderr_and_exit_context() {
    let command = format!(
        "node {}",
        script("process.stderr.write('x'.repeat(20000) + 'runtime contract constants are incomplete'); process.exit(42);")
    );
    let (request, provider) = request("task-empty-stdout", command);

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.provider_empty_stdout"
    );
    assert_eq!(outcome.diagnostics[0].data["exit_code"], json!(42));
    assert_eq!(outcome.diagnostics[0].data["stderr_truncated"], json!(true));
    assert!(
        outcome.diagnostics[0].data["stderr_bytes"]
            .as_u64()
            .expect("stderr byte count")
            > 16_384
    );
    let stderr = outcome.diagnostics[0].data["stderr"]
        .as_str()
        .expect("stderr capture");
    assert!(stderr.contains("runtime contract constants are incomplete"));
    assert!(stderr.len() <= 16 * 1024);
    assert!(outcome
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "executor-result"));
}

#[test]
fn provider_empty_stdout_records_failed_run_with_executor_evidence() {
    crate::test_support::with_isolated_home(|_| {
        let command = format!(
            "node {}",
            script("process.stderr.write('provider emitted diagnostics but no outcome'); process.exit(42);")
        );
        let (request, provider) = request("task-empty-stdout-recorded", command);
        let plan = AgentTaskPlan::new("plan-empty-stdout-recorded", vec![request]);
        let run_id = "run-empty-provider-output";
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]))
            .with_run_id(run_id);

        crate::core::agent_tasks::lifecycle::submit_plan(&plan, Some(run_id)).expect("submit plan");
        crate::core::agent_tasks::lifecycle::mark_running(run_id).expect("mark running");
        let aggregate = scheduler.run(plan.clone());
        let record =
            crate::core::agent_tasks::lifecycle::record_run_aggregate(run_id, &plan, &aggregate)
                .expect("record aggregate");

        assert_eq!(
            record.state,
            crate::core::agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::ProviderError
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_empty_stdout"
        );
        assert!(aggregate.outcomes[0]
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "executor-result"));
        assert!(record.latest_executor_evidence.is_some());
        assert!(record
            .artifact_refs
            .iter()
            .any(|reference| reference.kind == "executor-result"));
    });
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
fn provider_default_timeout_returns_structured_outcome_without_explicit_timeout() {
    let _lock = DEFAULT_TIMEOUT_ENV_LOCK
        .lock()
        .expect("default timeout env lock");
    std::env::set_var("HOMEBOY_AGENT_TASK_TEST_DEFAULT_PROVIDER_TIMEOUT_MS", "50");
    let command = format!("node {}", script("setInterval(() => {}, 1000);"));
    let (request, provider) = request("task-default-timeout", command);

    let outcome = run_provider_command(&request, &provider, None);
    std::env::remove_var("HOMEBOY_AGENT_TASK_TEST_DEFAULT_PROVIDER_TIMEOUT_MS");

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
    assert_eq!(outcome.diagnostics[0].class, "agent_task.provider_timeout");
    assert_eq!(outcome.diagnostics[0].data["timeout_ms"], json!(50));
}

#[test]
fn stalled_provider_is_killed_and_rotates_to_configured_fallback() {
    let pid_path = unique_state_path("stalled-child");
    let _ = fs::remove_file(&pid_path);
    let pid = pid_path.to_string_lossy().replace('\\', "\\\\");
    let primary_command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs'); fs.writeFileSync('{pid}', String(process.pid)); setInterval(() => {{}}, 1000);"
        ))
    );
    let fallback_command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'fallback completed'}));")
    );
    let (request, primary) = request("task-stalled-rotation", primary_command);
    let mut fallback = primary.clone();
    fallback.id = "fallback.provider".to_string();
    fallback.backend = "fallback".to_string();
    fallback.command_argv = fallback_command
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let scheduler =
        AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
            primary, fallback,
        ]));
    let mut plan = AgentTaskPlan::new("plan-stalled-rotation", vec![request]);
    plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
        entries: vec![AgentTaskProviderRotationEntry {
            backend: Some("fallback".to_string()),
            ..AgentTaskProviderRotationEntry::default()
        }],
        liveness_timeout_ms: Some(50),
        ..AgentTaskProviderRotationPolicy::default()
    });

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    let attempts = aggregate.outcomes[0]
        .metadata
        .pointer("/provider_rotation/attempts")
        .and_then(Value::as_array)
        .expect("rotation evidence");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["failure_classification"], json!("stalled"));
    assert_eq!(attempts[1]["backend"], json!("fallback"));

    let child_pid = fs::read_to_string(&pid_path).expect("stalled child wrote pid");
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", child_pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("check child process")
            .success(),
        "liveness timeout must reap the provider child"
    );
    let _ = fs::remove_file(&pid_path);
}

#[test]
fn provider_can_return_timeout_payload_during_wrapper_grace() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'provider serialized timeout',failure_classification:'timeout',artifacts:[{schema:'homeboy/agent-task-artifact/v1',id:'timeout-evidence',kind:'provider-task-runner-preflight',path:'/tmp/timeout-evidence.json'}]})), 3050);")
    );
    let (mut request, provider) = request("task-timeout-payload", command);
    request.limits.timeout_ms = Some(3000);

    let outcome = run_provider_command(&request, &provider, None);

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
fn provider_attempts_receive_distinct_allocated_runtime_tmpdirs() {
    crate::test_support::with_isolated_home(|_| {
        let state = unique_state_path("scratch-attempts");
        let state_path = state.to_string_lossy().replace('\\', "\\\\");
        let command = format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let tmp=req.executor.config.runtime_env.TMPDIR; let entries=[]; try {{ entries=JSON.parse(fs.readFileSync('{state_path}','utf8')); }} catch (_) {{}} entries.push({{tmp,keep:req.executor.config.runtime_env.KEEP}}); fs.writeFileSync('{state_path}',JSON.stringify(entries)); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:entries.length===1?'failed':'succeeded',failure_classification:entries.length===1?'execution_failed':null,summary:tmp}}));"
            ))
        );
        let (mut request, provider) = request("task-scratch-retry", command);
        request.executor.config = json!({ "runtime_env": { "KEEP": "preserved" } });
        let mut plan = AgentTaskPlan::new("plan-scratch-retry", vec![request]);
        plan.options.retry.max_attempts = 2;
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::ExecutionFailed];

        let aggregate =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]))
            .with_run_id("run-scratch-retry")
            .run(plan);

        assert_eq!(aggregate.totals.succeeded, 1);
        let attempts: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(&state).expect("attempt records"))
                .expect("attempt JSON");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["keep"], "preserved");
        assert_eq!(attempts[1]["keep"], "preserved");
        assert_ne!(attempts[0]["tmp"], attempts[1]["tmp"]);
        for attempt in attempts {
            let tmpdir = PathBuf::from(attempt["tmp"].as_str().expect("TMPDIR"));
            assert!(tmpdir.is_dir());
        }
        let index: Value = serde_json::from_str(
            &fs::read_to_string(
                crate::core::paths::homeboy_data()
                    .expect("homeboy data")
                    .join("controller-scratch/test-indexes")
                    .join("run-scratch-retry")
                    .join("resources.json"),
            )
            .expect("scratch index"),
        )
        .expect("scratch index JSON");
        let resources = index["resources"].as_array().expect("scratch resources");
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0]["lifecycle_state"], "released");
        assert_eq!(resources[0]["terminal_reason"], "retry");
        assert_eq!(resources[1]["lifecycle_state"], "released");
        assert_eq!(resources[1]["terminal_reason"], "succeeded");
    });
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
    assert!(!is_transient_provider_error("429 Too Many Requests"));

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

    let outcome = run_provider_command(&request, &provider, None);

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

    let outcome = run_provider_command(&request, &provider, None);

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

    let outcome = run_provider_command(&request, &provider, None);

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

fn selected_component_contract(
    slug: &str,
    path: &std::path::Path,
) -> crate::core::agent_task::AgentTaskComponentContract {
    let mut extra = serde_json::Map::new();
    extra.insert("loadMode".to_string(), json!("runtime-loadable"));
    extra.insert("activate".to_string(), json!(true));
    crate::core::agent_task::AgentTaskComponentContract {
        slug: Some(slug.to_string()),
        path: Some(path.display().to_string()),
        extra,
    }
}

fn runtime_preflight_provider() -> AgentTaskExecutorProvider {
    let (_, mut provider) = request("runtime-preflight", "noop".to_string());
    provider.runtime_contract.preflight_checks = vec![serde_json::from_value(json!({
        "id": "runtime.package_shadow",
        "enforcement": "error",
        "target": {
            "component": {
                "metadata_equals": { "loadMode": "runtime-loadable" },
                "metadata_any_equals": { "activate": true }
            }
        },
        "path_probes": {
            "exists": [{
                "path": "vendor/acme/runtime-lib",
                "subject": "acme/runtime-lib",
                "owner": "runtime-1"
            }]
        }
    }))
    .expect("runtime preflight check")];
    provider
}

fn runtime_preflight_plan(
    component: crate::core::agent_task::AgentTaskComponentContract,
) -> AgentTaskPlan {
    let (mut req, _) = request("runtime-preflight", "noop".to_string());
    req.component_contracts = vec![component];
    AgentTaskPlan::new("plan-runtime-preflight".to_string(), vec![req])
}

#[test]
fn plan_runtime_preflight_passes_when_declared_probe_is_absent() {
    let component = tempfile::tempdir().expect("component dir");
    let plan = runtime_preflight_plan(selected_component_contract(
        "provider-component",
        component.path(),
    ));

    enforce_runtime_preflight_checks_for_plan_with_providers(
        &plan,
        &[runtime_preflight_provider()],
    )
    .expect("clean component passes declared preflight before dispatch");
}

#[test]
fn plan_runtime_preflight_refuses_declared_path_conflict_before_dispatch() {
    let component = tempfile::tempdir().expect("component dir");
    fs::create_dir_all(component.path().join("vendor/acme/runtime-lib"))
        .expect("create conflict dir");
    let plan = runtime_preflight_plan(selected_component_contract(
        "provider-component",
        component.path(),
    ));

    let err = enforce_runtime_preflight_checks_for_plan_with_providers(
        &plan,
        &[runtime_preflight_provider()],
    )
    .expect_err("declared path conflict is refused before dispatch");

    assert_eq!(err.details["field"], "runtime_preflight_checks");
    assert!(err.message.contains("acme/runtime-lib"));
    assert!(err.message.contains("runtime-1"));
    assert!(err.message.contains("provider-component"));
}

#[test]
fn plan_runtime_preflight_skips_provider_without_declared_checks() {
    let component = tempfile::tempdir().expect("component dir");
    fs::create_dir_all(component.path().join("vendor/acme/runtime-lib"))
        .expect("create conflict dir");
    let plan = runtime_preflight_plan(selected_component_contract(
        "provider-component",
        component.path(),
    ));
    let (_, provider) = request("runtime-preflight", "noop".to_string());

    enforce_runtime_preflight_checks_for_plan_with_providers(&plan, &[provider])
        .expect("provider without declared checks is a no-op");
}
