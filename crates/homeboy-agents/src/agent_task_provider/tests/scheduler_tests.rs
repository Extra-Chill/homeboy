use super::common::{request, script};
use super::*;
use crate::agent_task::AgentTaskArtifactDeclaration;
use crate::agent_task_scheduler::{
    AgentTaskAggregateStatus, AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
};
use std::sync::{Arc, Mutex};

static DEFAULT_TIMEOUT_ENV_LOCK: Mutex<()> = Mutex::new(());

fn git(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn linked_cook_source(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let repository = temp.path().join("repository");
    let source = temp.path().join("source");
    std::fs::create_dir(&repository).expect("create repository");
    git(&repository, &["init", "--quiet", "-b", "main"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(repository.join("base.txt"), "base\n").expect("write base");
    git(&repository, &["add", "base.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "base"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--detach",
            source.to_str().expect("source path"),
            "HEAD",
        ],
    );
    source
}

fn remapped_cook_source(temp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let controller = linked_cook_source(temp);
    let runner = temp.path().join("runner-snapshot");
    git(
        &controller,
        &[
            "clone",
            "--quiet",
            controller.to_str().expect("controller path"),
            runner.to_str().expect("runner path"),
        ],
    );
    (controller, runner)
}

#[test]
fn remapped_lab_cook_spawns_and_harvests_from_runner_snapshot_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (controller, source) = remapped_cook_source(&temp);
    let marker = temp.path().join("provider-started");
    let command = format!(
        "node {}",
        script(&format!(
            "let cp=require('child_process'),fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); fs.writeFileSync('provider-change.txt','changed\\n'); cp.execFileSync('git',['add','provider-change.txt']); cp.execFileSync('git',['-c','user.name=Homeboy','-c','user.email=homeboy@example.test','commit','-m','provider change']); fs.writeFileSync({:?}, JSON.stringify({{cwd:process.cwd(),workspace:req.workspace.root,attestation:req.metadata.cook_attempt_workspace_identity}})); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'provider spawned'}}));",
            marker.display().to_string()
        ))
    );
    let (mut request, provider) = request("cook-provider", command);
    request.workspace.root = Some(source.display().to_string());
    request.metadata = serde_json::json!({
        "cook_workspace_identity": crate::agent_task_workspace_identity::attest_workspace(&controller)
            .expect("attest source"),
    });
    let mut plan = AgentTaskPlan::new("remapped-lab-cook-provider", vec![request]);
    crate::agent_task_service::bind_runner_snapshot_workspace_attestations(&mut plan)
        .expect("bind runner snapshot");

    let aggregate = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ))
    .run(plan);

    assert_eq!(aggregate.totals.succeeded, 1, "{aggregate:?}");
    let provider_observation: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).expect("provider marker"))
            .expect("provider observation");
    let provider_cwd = provider_observation["cwd"].as_str().expect("provider cwd");
    assert_ne!(
        provider_cwd,
        std::fs::canonicalize(&source)
            .expect("canonical source")
            .display()
            .to_string(),
        "provider must run in the isolated attempt workspace"
    );
    assert_eq!(provider_observation["workspace"], provider_cwd);
    assert_eq!(
        provider_observation["attestation"]["canonical_path"], provider_cwd,
        "the spawned provider must receive the attestation for its isolated cwd"
    );
    let patch = aggregate.outcomes[0]
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "patch")
        .expect("committed change harvest patch");
    assert!(
        std::fs::read_to_string(patch.path.as_deref().expect("patch path"))
            .expect("read patch")
            .contains("provider-change.txt")
    );
}

#[test]
fn remapped_lab_cook_rejects_runner_snapshot_drift_before_provider_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (controller, runner) = remapped_cook_source(&temp);
    let marker = temp.path().join("provider-started");
    let (mut request, provider) = request(
        "cook-provider",
        format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); fs.writeFileSync({:?}, 'started'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:'cook-provider',status:'succeeded'}}));",
                marker.display().to_string()
            ))
        ),
    );
    request.workspace.root = Some(runner.display().to_string());
    request.metadata = serde_json::json!({
        "cook_workspace_identity": crate::agent_task_workspace_identity::attest_workspace(&controller)
            .expect("attest controller source"),
    });
    let mut plan = AgentTaskPlan::new("runner-drift-cook", vec![request]);
    crate::agent_task_service::bind_runner_snapshot_workspace_attestations(&mut plan)
        .expect("bind runner snapshot");
    std::fs::rename(runner.join(".git"), runner.join("replaced-git-directory"))
        .expect("replace runner git directory");
    std::fs::create_dir(runner.join(".git")).expect("new runner git directory");

    let aggregate = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ))
    .run(plan);

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.committed_harvest_git_failed"
    );
    assert!(
        !marker.exists(),
        "provider must not start after runner drift"
    );
}

#[test]
fn remapped_lab_cook_retry_retains_controller_predecessor_and_rebinds_directory_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (controller, runner) = remapped_cook_source(&temp);
    let (mut request, _) = request("cook-provider", "true".to_string());
    request.workspace.root = Some(runner.display().to_string());
    let controller_identity = crate::agent_task_workspace_identity::attest_workspace(&controller)
        .expect("attest controller source");
    request.metadata = serde_json::json!({ "cook_workspace_identity": controller_identity });
    let mut plan = AgentTaskPlan::new("remapped-lab-cook-retry", vec![request]);

    crate::agent_task_service::bind_runner_snapshot_workspace_attestations(&mut plan)
        .expect("bind runner snapshot");
    crate::agent_task_service::bind_runner_snapshot_workspace_attestations(&mut plan)
        .expect("rebind resumed runner snapshot");

    let metadata = &plan.tasks[0].metadata;
    assert_eq!(
        metadata["cook_workspace_identity"]["git_representation"],
        "directory"
    );
    assert_eq!(
        metadata["cook_workspace_identity_predecessor"]["git_representation"], "pointer_file",
        "resume must retain the controller attestation as provenance"
    );
    assert!(
        crate::agent_task_workspace_identity::workspace_matches_attestation(
            &runner,
            &metadata["cook_workspace_identity"]
        )
    );
}

#[test]
fn isolated_cook_rejects_source_identity_drift_before_snapshotting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = linked_cook_source(&temp);
    let marker = temp.path().join("provider-started");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs'); fs.writeFileSync({:?}, 'started'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:'cook-provider',status:'succeeded'}}));",
            marker.display().to_string()
        ))
    );
    let (mut request, provider) = request("cook-provider", command);
    request.workspace.root = Some(source.display().to_string());
    request.metadata = serde_json::json!({
        "cook_workspace_identity": crate::agent_task_workspace_identity::attest_workspace(&source)
            .expect("attest source"),
    });
    std::fs::write(source.join(".git"), "gitdir: ../replaced-gitdir\n")
        .expect("replace source gitdir reference");

    let aggregate = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ))
    .run(AgentTaskPlan::new("source-drift-cook", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.committed_harvest_git_failed"
    );
    assert!(
        !marker.exists(),
        "provider must not start after source drift"
    );
}

#[test]
fn scheduler_dispatches_extension_provider_command() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'ok',outputs:{issue_number:3447}}));")
    );
    let (request, provider) = request("task-a", command);
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

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
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let roots = context.path_roots();
    let runner_root = roots.artifacts().to_path_buf();
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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider])
            .with_path_roots(roots.clone()),
    ))
    .with_scratch_root(roots.data().to_path_buf());

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

#[test]
fn executor_artifact_paths_are_distinct_per_run() {
    {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:req.artifacts_path}));")
        );
        let (request, provider) = request("same-task", command);
        let first = AgentTaskScheduler::new(Arc::new(
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider.clone()]),
        ))
        .with_run_id("run-one")
        .run(AgentTaskPlan::new("plan", vec![request.clone()]));
        let second = AgentTaskScheduler::new(Arc::new(
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
        ))
        .with_run_id("run-two")
        .run(AgentTaskPlan::new("plan", vec![request]));

        assert_ne!(first.outcomes[0].summary, second.outcomes[0].summary);
    }
}

#[test]
fn scheduler_reports_missing_extension_provider() {
    let (request, _provider) = request("task-missing-provider", "unused".to_string());
    let scheduler =
        AgentTaskScheduler::new(Arc::new(ExtensionProviderAgentTaskExecutor::default()));

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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-capability", vec![request]));

    assert_eq!(aggregate.totals.failed, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::CapabilityMissing)
    );
    // #11509 layered a selection-level check ahead of the per-provider one: when
    // no provider for the backend advertises the capability at all, that is the
    // more specific diagnosis. The failure classification is unchanged.
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "agent_task.provider_capability_unavailable"
    );
    // The selection-level diagnostic also names the layer it failed at.
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["layer"],
        json!("provider")
    );
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].data["required_capabilities"],
        json!(["workspace_write"])
    );
}

#[test]
fn scheduler_normalizes_malformed_provider_output() {
    let command = format!("node {}", script("process.stdout.write('{not json');"));
    let (request, provider) = request("task-malformed-provider", command);
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

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
fn declared_evidence_policy_denial_is_terminal_without_provider_retry() {
    let count = tempfile::NamedTempFile::new().expect("count file");
    let command = format!(
        "node {} {}",
        script(&format!("const fs=require('fs');const count=process.argv[2];fs.writeFileSync(count,String((Number(fs.readFileSync(count,'utf8')||0))+1));process.stdout.write(JSON.stringify({{schema:'{AGENT_TASK_OUTCOME_SCHEMA}',task_id:'task-evidence-policy-denied',status:'failed',diagnostics:[{{class:'provider.permission',message:'denied',data:{{kind:'permission_denied',path:'/workspace/.homeboy/evidence/input.json'}}}}]}}));")),
        count.path().display(),
    );
    let (mut request, provider) = request("task-evidence-policy-denied", command);
    request.executor.config =
        json!({"evidence_inputs":[{"path":"/workspace/.homeboy/evidence/input.json"}]});

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::PolicyDenied)
    );
    assert_eq!(
        outcome.metadata["control_plane_failure"]["phase"],
        "provider_evidence_preflight"
    );
    assert_eq!(
        std::fs::read_to_string(count.path()).expect("read count"),
        "1"
    );
}

#[test]
fn policy_denial_prose_without_a_declared_structured_path_is_not_reclassified() {
    let command = format!(
        "node {}",
        script("process.stdout.write(JSON.stringify({status:'failed',summary:'permission policy denied external_directory /workspace/.homeboy/evidence/input.json'}));")
    );
    let (mut request, provider) = request("task-evidence-policy-prose", command);
    request.executor.config =
        json!({"evidence_inputs":[{"path":"/workspace/.homeboy/evidence/input.json"}]});
    let outcome = run_provider_command(&request, &provider, None);
    assert_ne!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::PolicyDenied)
    );
}

/// A provider killed by an external SIGTERM must not be reported as
/// "the provider produced no output". The two failures have different causes
/// and different remediations, and collapsing them sends operators at the
/// provider when the real cause was the kill.
#[cfg(unix)]
#[test]
fn provider_external_sigterm_is_not_reported_as_empty_stdout() {
    let command = format!(
        "node {}",
        script("process.kill(process.pid, 'SIGTERM'); setInterval(() => {}, 1000);")
    );
    let (request, provider) = request("task-external-sigterm", command);

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.diagnostics[0].class, "agent_task.provider_signal_terminated",
        "{:?}",
        outcome.diagnostics
    );
    let data = &outcome.diagnostics[0].data;
    assert_eq!(data["signal"], json!(15));
    assert_eq!(data["signal_name"], json!("SIGTERM"));
    assert_eq!(data["exit_code"], serde_json::Value::Null);
    assert_eq!(data["termination_initiator"], json!("external_sigterm"));
    assert_eq!(data["homeboy_initiated_termination"], json!(false));
    assert_eq!(data["likely_oom_kill"], json!(false));
    assert!(data["elapsed_ms"].is_u64(), "{data}");
    assert_eq!(data["execution_deadline_unix_ms"], serde_json::Value::Null);
    assert!(outcome
        .summary
        .as_deref()
        .expect("summary")
        .contains("SIGTERM"));
}

/// SIGKILL with no captured output is the signature of the Linux OOM killer,
/// which is a real failure mode on both this host and CI runners. The
/// diagnostic has to say so instead of blaming the provider for silence it
/// could not have avoided.
#[cfg(unix)]
#[test]
fn provider_sigkill_records_probable_oom_context() {
    let command = format!(
        "node {}",
        // Distinct body length keeps this script file separate from the
        // SIGTERM script, which is keyed by body length.
        script("process.kill(process.pid, 'SIGKILL'); setInterval(() => {}, 10_000);")
    );
    let (request, provider) = request("task-external-sigkill", command);

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(
        outcome.diagnostics[0].class, "agent_task.provider_signal_terminated",
        "{:?}",
        outcome.diagnostics
    );
    let data = &outcome.diagnostics[0].data;
    assert_eq!(data["signal"], json!(9));
    assert_eq!(data["signal_name"], json!("SIGKILL"));
    assert_eq!(data["termination_initiator"], json!("external_sigkill"));
    assert_eq!(data["likely_oom_kill"], json!(true));
    let hints = data["signal_remediation_hints"]
        .as_array()
        .expect("signal remediation hints");
    assert!(
        hints
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|hint| hint.contains("OOM")),
        "{hints:?}"
    );
}

#[test]
fn provider_empty_stdout_records_failed_run_with_executor_evidence() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let roots = context.path_roots();
    let lifecycle_store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(roots.clone());
    let command = format!(
            "node {}",
            script("process.stderr.write('provider emitted diagnostics but no outcome'); process.exit(42);")
        );
    let (request, provider) = request("task-empty-stdout-recorded", command);
    let plan = AgentTaskPlan::new("plan-empty-stdout-recorded", vec![request]);
    let run_id = "run-empty-provider-output";
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider])
            .with_path_roots(roots.clone()),
    ))
    .with_scratch_root(roots.data().to_path_buf());

    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submit plan");
    let aggregate = scheduler.run(plan.clone());
    let record = lifecycle_store
        .record_run_aggregate(run_id, &plan, &aggregate)
        .expect("record aggregate");

    assert_eq!(
        record.state,
        crate::agent_task_lifecycle::AgentTaskRunState::Failed
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
}

#[test]
fn provider_timeout_returns_structured_outcome() {
    let command = format!("node {}", script("setInterval(() => {}, 1000);"));
    let (mut request, provider) = request("task-timeout", command);
    request.limits.timeout_ms = Some(50);
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-timeout", vec![request]));

    assert_eq!(aggregate.totals.timed_out, 1);
    assert_eq!(
        aggregate.outcomes[0].failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
}

#[test]
fn provider_timeout_persists_bounded_redacted_last_activity() {
    const SECRET_ENV: &str = "HOMEBOY_TIMEOUT_TEST_SECRET";
    const SECRET: &str = "timeout-secret-value";
    std::env::set_var(SECRET_ENV, SECRET);
    struct ClearSecret;
    impl Drop for ClearSecret {
        fn drop(&mut self) {
            std::env::remove_var(SECRET_ENV);
        }
    }
    let _clear_secret = ClearSecret;

    let command = format!(
        "node {}",
        script("process.stderr.write('x'.repeat(20000) + process.env.HOMEBOY_TIMEOUT_TEST_SECRET); setInterval(() => {}, 1000);")
    );
    let (mut request, provider) = request("task-timeout-evidence", command);
    request.limits.timeout_ms = Some(50);
    request.executor.secret_env = vec![SECRET_ENV.to_string()];

    let outcome = run_provider_command(&request, &provider, Some("run-timeout-evidence"));

    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "agent_task.provider_timeout")
        .expect("timeout diagnostic");
    assert!(
        diagnostic.data["output_event_count"]
            .as_u64()
            .unwrap_or_default()
            >= 1
    );
    assert_eq!(diagnostic.data["stdout_bytes"], json!(0));
    assert!(diagnostic.data["stderr_bytes"].as_u64().unwrap_or_default() > 16 * 1024);
    assert_eq!(diagnostic.data["last_activity"]["kind"], json!("stderr"));
    assert_eq!(diagnostic.data["stderr_tail_truncated"], json!(true));
    assert!(
        diagnostic.data["stderr_tail"]
            .as_str()
            .unwrap_or_default()
            .len()
            <= 16 * 1024
    );
    assert!(!diagnostic.data.to_string().contains(SECRET));
    assert!(diagnostic.data.to_string().contains("[redacted]"));
    assert_eq!(
        diagnostic.data["log_lookup"],
        json!("homeboy agent-task logs run-timeout-evidence --task task-timeout-evidence")
    );
    assert_eq!(
        diagnostic.data["provider_boundary_evidence"],
        json!("executor-result")
    );
}

#[test]
fn expired_execution_deadline_returns_typed_outcome_without_spawning_provider() {
    let (mut request, provider) = request("task-deadline", "missing-provider-command".to_string());
    request.limits.execution_deadline_unix_ms = Some(0);

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Timeout)
    );
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.execution_deadline_exceeded"
    );
    assert_eq!(
        outcome.diagnostics[0].data["completed_phase"],
        "provider_execution"
    );
    assert_eq!(outcome.diagnostics[0].data["remaining_budget_ms"], 0);
}

/// Restores the process-global test timeout override on drop, including on
/// panic. Without this a failing assertion left the 50ms override set for every
/// subsequent test in the process (#7739).
struct DefaultTimeoutOverride;

impl DefaultTimeoutOverride {
    fn set(value: &str) -> Self {
        std::env::set_var("HOMEBOY_AGENT_TASK_TEST_DEFAULT_PROVIDER_TIMEOUT_MS", value);
        Self
    }
}

impl Drop for DefaultTimeoutOverride {
    fn drop(&mut self) {
        std::env::remove_var("HOMEBOY_AGENT_TASK_TEST_DEFAULT_PROVIDER_TIMEOUT_MS");
    }
}

#[test]
fn provider_default_timeout_returns_structured_outcome_without_explicit_timeout() {
    let _lock = DEFAULT_TIMEOUT_ENV_LOCK
        .lock()
        .expect("default timeout env lock");
    let _override = DefaultTimeoutOverride::set("50");
    let command = format!("node {}", script("setInterval(() => {}, 1000);"));
    let (mut request, provider) = request("task-default-timeout", command);
    // This test is specifically about the *default*, so drop the explicit
    // timeout the shared fixture now pins.
    request.limits.timeout_ms = None;

    let outcome = run_provider_command(&request, &provider, None);

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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![primary, fallback]),
    ));
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
fn silent_provider_hits_liveness_deadline_before_wall_timeout() {
    let command = format!("node {}", script("setInterval(() => {}, 1_000);"));
    let (mut request, provider) = request("task-liveness-silent", command);
    request.limits.timeout_ms = Some(500);
    request.limits.liveness_timeout_ms = Some(50);

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Stalled)
    );
    assert_eq!(
        outcome.diagnostics[0].class,
        "agent_task.provider_liveness_timeout"
    );
    assert_eq!(outcome.diagnostics[0].data["deadline"], json!("liveness"));
    assert_eq!(
        outcome.diagnostics[0].data["liveness_timeout_ms"],
        json!(50)
    );
    assert_eq!(outcome.diagnostics[0].data["timeout_ms"], json!(500));
}

#[test]
fn parent_visible_progress_keeps_provider_alive_until_wall_deadline() {
    let command = format!(
        "node {}",
        script("setInterval(() => process.stderr.write('.'), 10);")
    );
    let (mut request, provider) = request("task-liveness-progress", command);
    request.limits.timeout_ms = Some(500);
    request.limits.liveness_timeout_ms = Some(300);

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
    assert_eq!(outcome.diagnostics[0].class, "agent_task.provider_timeout");
    assert_eq!(outcome.diagnostics[0].data["deadline"], json!("wall_clock"));
    assert_eq!(
        outcome.diagnostics[0].data["liveness_timeout_ms"],
        json!(300)
    );
    assert_eq!(outcome.diagnostics[0].data["timeout_ms"], json!(500));
}

#[test]
fn stalled_provider_retains_declared_attempt_workspace_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = linked_cook_source(&temp);
    let command = format!(
        "node {}",
        script("let fs=require('fs'); fs.writeFileSync('partial.patch','retained partial patch\\n'); process.stderr.write('artifact written\\n'); setInterval(() => {}, 1000);")
    );
    let (mut request, provider) = request("task-stalled-artifact", command);
    request.workspace.root = Some(workspace.display().to_string());
    request.metadata = serde_json::json!({
        "cook_attempt_workspace_identity": crate::agent_task_workspace_identity::attest_workspace(&workspace)
            .expect("attest workspace"),
    });
    request.artifact_declarations = vec![AgentTaskArtifactDeclaration {
        name: "partial_patch".to_string(),
        artifact_type: Some("patch".to_string()),
        artifact_schema: None,
        path: Some("partial.patch".to_string()),
        required: true,
        description: None,
        metadata: Value::Null,
    }];
    request.limits.liveness_timeout_ms = Some(5_000);

    let outcome = run_provider_command_once(&request, &provider);

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::ProviderError);
    let artifact = outcome.artifacts.first().expect("retained artifact");
    let path = std::path::Path::new(artifact.path.as_deref().expect("finalized path"));
    assert_eq!(
        std::fs::read(path).expect("retained bytes"),
        b"retained partial patch\n"
    );
    assert_eq!(artifact.size_bytes, Some(23));
    assert_eq!(
        artifact.sha256,
        Some(homeboy_core::artifact_metadata::sha256_file(path).expect("retained hash"))
    );
    assert_eq!(artifact.metadata["executor_artifact_finalized"], true);
}

#[test]
fn provider_can_return_timeout_payload_during_wrapper_grace() {
    let command = format!(
        "node {}",
        script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'provider serialized timeout',failure_classification:'timeout',artifacts:[{schema:'homeboy/agent-task-artifact/v1',id:'timeout-evidence',kind:'provider-task-runner-preflight',path:'/tmp/timeout-evidence.json'}]})), 5001);")
    );
    let (mut request, provider) = request("task-timeout-payload", command);
    request.limits.timeout_ms = Some(5000);

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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-config", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    assert_eq!(
        aggregate.outcomes[0].summary.as_deref(),
        Some("test.provider")
    );
}

#[test]
fn provider_attempts_receive_distinct_allocated_runtime_tmpdirs() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let roots = context.path_roots();
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

    let aggregate = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider])
            .with_path_roots(roots.clone()),
    ))
    .with_scratch_root(roots.data().to_path_buf())
    .run(plan);

    assert_eq!(aggregate.totals.succeeded, 1);
    let attempts: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(&state).expect("attempt records"))
            .expect("attempt JSON");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["keep"], "preserved");
    assert_eq!(attempts[1]["keep"], "preserved");
    assert_ne!(attempts[0]["tmp"], attempts[1]["tmp"]);
    for attempt in &attempts {
        let tmpdir = PathBuf::from(attempt["tmp"].as_str().expect("TMPDIR"));
        assert!(tmpdir.is_dir());
    }
    let first_tmpdir = PathBuf::from(attempts[0]["tmp"].as_str().expect("TMPDIR"));
    let scratch_root =
        fs::canonicalize(roots.data().join("controller-scratch/attempts")).expect("scratch root");
    let scratch_run_id = first_tmpdir
        .strip_prefix(scratch_root)
        .expect("scratch root")
        .components()
        .next()
        .expect("scratch run id")
        .as_os_str();
    let index: Value = serde_json::from_str(
        &fs::read_to_string(
            roots
                .data()
                .join("controller-scratch/test-indexes")
                .join(scratch_run_id)
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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

    let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env", vec![request]));

    assert_eq!(aggregate.totals.succeeded, 1);
    std::env::remove_var(secret_name);
}

#[test]
fn provider_command_receives_only_the_declared_launch_environment() {
    let secret_name = format!("HOMEBOY_TEST_DECLARED_LAUNCH_SECRET_{}", std::process::id());
    let ambient_name = format!("HOMEBOY_TEST_UNDECLARED_LAUNCH_ENV_{}", std::process::id());
    std::env::set_var(&secret_name, "declared-secret");
    std::env::set_var(&ambient_name, "must-not-leak");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs');let req=JSON.parse(fs.readFileSync(0,'utf8'));let launch=JSON.parse(process.env.{});let secretOk=process.env.{secret_name}==='declared-secret';let ambientAbsent=process.env.{ambient_name}===undefined;let redacted=!JSON.stringify(launch).includes('declared-secret');let identity=launch.task_id===req.task_id&&launch.schema==='homeboy/agent-task-provider-launch-context/v1';process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:secretOk&&ambientAbsent&&redacted&&identity?'succeeded':'failed',summary:'launch context checked'}}));",
            crate::agent_task_provider::AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON_ENV
        ))
    );
    let (mut request, provider) = request("task-declared-launch-env", command);
    request.executor.secret_env = vec![secret_name.clone()];
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

    let aggregate = scheduler.run(AgentTaskPlan::new(
        "plan-declared-launch-env",
        vec![request],
    ));

    assert_eq!(aggregate.totals.succeeded, 1, "{aggregate:?}");
    std::env::remove_var(secret_name);
    std::env::remove_var(ambient_name);
}

#[test]
fn provider_command_receives_canonical_secret_env_plan_without_values() {
    let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_PLAN_SECRET_{}", std::process::id());
    std::env::set_var(&secret_name, "hydrated-secret");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let plan=JSON.parse(process.env.{}); let mapped=(plan.env_name_mapping['test.provider']||[]).includes('{secret_name}'); let configured=(plan.status||[]).some((item)=>item.name==='{secret_name}'&&item.configured===true&&item.source==='env'); let leaked=JSON.stringify(plan).includes('hydrated-secret'); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:mapped&&configured&&!leaked?'succeeded':'failed',summary:JSON.stringify(plan)}}));",
            homeboy_core::secret_env_plan::AGENT_TASK_SECRET_ENV_PLAN_JSON_ENV
        ))
    );
    let (mut request, mut provider) = request("task-secret-env-plan", command);
    provider.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
        id: "test.provider.auth".to_string(),
        label: "Test provider auth".to_string(),
        invocation: None,
        secret_env: vec![secret_name.clone()],
        env_path: None,
        executable: None,
        remediation: None,
        extra: BTreeMap::new(),
    }];
    request.executor.secret_env = vec![secret_name.clone()];
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

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
    let scheduler = AgentTaskScheduler::new(Arc::new(
        ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
    ));

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
    let scheduler =
        AgentTaskScheduler::new(Arc::new(ExtensionProviderAgentTaskExecutor::default()));

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
    let scheduler =
        AgentTaskScheduler::new(Arc::new(ExtensionProviderAgentTaskExecutor::default()));

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

#[test]
fn repeated_immediate_adapter_classified_failure_opens_circuit_with_actionable_evidence() {
    let state_path = unique_state_path("immediate-circuit");
    let _ = fs::remove_file(&state_path);
    let state = state_path.to_string_lossy().replace('\\', "\\\\");
    let command = format!(
        "node {}",
        script(&format!(
            "let fs=require('fs');let req=JSON.parse(fs.readFileSync(0,'utf8'));let p='{state}';let n=0;try{{n=parseInt(fs.readFileSync(p,'utf8'))||0}}catch(e){{}}fs.writeFileSync(p,String(n+1));process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'Unexpected server error. Check server logs for details. err_test123',failure_classification:'provider'}}));"
        ))
    );
    let (request, mut provider) = request("task-immediate-circuit", command);
    provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
        id: "server_error".to_string(),
        error_contains_any: vec!["unexpected server error".to_string()],
        retryable: true,
        error_ref_pattern: Some(r"err_[A-Za-z0-9]+".to_string()),
        log_lookup: Some("providerctl logs --error-ref <provider-error-ref>".to_string()),
        fallback_action: Some("Select another configured provider.".to_string()),
    }];

    let outcome = run_provider_command(&request, &provider, None);

    assert_eq!(fs::read_to_string(&state_path).expect("attempt count"), "2");
    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::Provider)
    );
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.class == "agent_task.provider_immediate_failure_retry_suppressed"
        })
        .expect("circuit diagnostic");
    assert_eq!(
        diagnostic.data["provider_error_refs"],
        json!(["err_test123"])
    );
    assert_eq!(diagnostic.data["retryable"], json!(false));
    assert_eq!(
        diagnostic.data["log_lookup"],
        "providerctl logs --error-ref <provider-error-ref>"
    );
    assert_eq!(
        diagnostic.data["fallback_action"],
        "Select another configured provider."
    );
    let _ = fs::remove_file(&state_path);
}

#[test]
fn immediate_failure_suppression_is_scoped_to_one_task_provider_retry_sequence() {
    let first_state = unique_state_path("immediate-first");
    let second_state = unique_state_path("immediate-second");
    let _ = fs::remove_file(&first_state);
    let _ = fs::remove_file(&second_state);
    for (task_id, state) in [("first", &first_state), ("second", &second_state)] {
        let state = state.to_string_lossy().replace('\\', "\\\\");
        let (request, mut provider) = request(task_id, format!("node {}", script(&format!(
            "let fs=require('fs');let req=JSON.parse(fs.readFileSync(0,'utf8'));let p='{state}';let n=0;try{{n=parseInt(fs.readFileSync(p,'utf8'))||0}}catch(e){{}}fs.writeFileSync(p,String(n+1));process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'provider_error',summary:'server unavailable',failure_classification:'provider'}}));"
        ))));
        provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
            id: "server".to_string(),
            error_contains_any: vec!["server unavailable".to_string()],
            retryable: true,
            error_ref_pattern: None,
            log_lookup: None,
            fallback_action: None,
        }];
        let outcome = run_provider_command(&request, &provider, None);
        assert!(outcome
            .diagnostics
            .iter()
            .any(|d| d.class == "agent_task.provider_immediate_failure_retry_suppressed"));
    }
    assert_eq!(fs::read_to_string(&first_state).expect("first count"), "2");
    assert_eq!(
        fs::read_to_string(&second_state).expect("second count"),
        "2"
    );
    let _ = fs::remove_file(&first_state);
    let _ = fs::remove_file(&second_state);
}

#[test]
fn invalid_immediate_failure_regex_is_rejected_before_dispatch() {
    let (_, mut provider) = request("invalid-pattern", "node provider.js".to_string());
    provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
        id: "bad".to_string(),
        error_contains_any: vec!["server".to_string()],
        retryable: true,
        error_ref_pattern: Some("[".to_string()),
        log_lookup: None,
        fallback_action: None,
    }];
    let error = validate_provider_immediate_failure_patterns(&provider).expect_err("invalid regex");
    assert!(error.contains("invalid error_ref_pattern"));
}

#[test]
fn empty_matching_immediate_failure_regex_is_rejected_before_dispatch() {
    let (_, mut provider) = request("empty-pattern", "node provider.js".to_string());
    provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
        id: "empty".to_string(),
        error_contains_any: vec!["server".to_string()],
        retryable: true,
        error_ref_pattern: Some(".*".to_string()),
        log_lookup: None,
        fallback_action: None,
    }];
    let error =
        validate_provider_immediate_failure_patterns(&provider).expect_err("empty match regex");
    assert!(error.contains("must not match an empty string"));
}

#[test]
fn immediate_failure_patterns_preserve_terminal_failure_classifications() {
    let (_, mut provider) = request("terminal-pattern", "node provider.js".to_string());
    provider.immediate_failure_patterns = vec![AgentTaskProviderImmediateFailurePattern {
        id: "server".to_string(),
        error_contains_any: vec!["server error".to_string()],
        retryable: true,
        error_ref_pattern: None,
        log_lookup: None,
        fallback_action: None,
    }];
    for classification in [
        AgentTaskFailureClassification::PolicyDenied,
        AgentTaskFailureClassification::RateLimited,
        AgentTaskFailureClassification::InvalidInput,
        AgentTaskFailureClassification::CapabilityMissing,
        AgentTaskFailureClassification::ExecutionFailed,
        AgentTaskFailureClassification::Timeout,
        AgentTaskFailureClassification::Stalled,
        AgentTaskFailureClassification::Unknown,
    ] {
        let outcome = AgentTaskOutcome {
            status: AgentTaskOutcomeStatus::ProviderError,
            summary: Some("server error".to_string()),
            failure_classification: Some(classification),
            ..Default::default()
        };
        assert!(immediate_provider_failure(&provider, &outcome, Duration::ZERO).is_none());
    }
}

#[test]
fn opencode_manifest_shaped_declaration_classifies_the_observed_server_failure() {
    let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
        "id": "opencode.agent-task-executor",
        "backend": "opencode",
        "immediate_failure_patterns": [{
            "id": "unexpected_server_error",
            "error_contains_any": ["Unexpected server error. Check server logs for details."],
            "retryable": true,
            "error_ref_pattern": "err_[A-Za-z0-9]+",
            "log_lookup": "opencode logs --error <provider-error-ref>",
            "fallback_action": "Select another configured provider."
        }]
    }))
    .expect("manifest-shaped provider");
    let outcome = AgentTaskOutcome {
        status: AgentTaskOutcomeStatus::ProviderError,
        summary: Some(
            "Unexpected server error. Check server logs for details. err_3a6d31e2".to_string(),
        ),
        ..Default::default()
    };
    let failure = immediate_provider_failure(&provider, &outcome, Duration::from_secs(1))
        .expect("classified failure");
    assert_eq!(failure.error_refs, vec!["err_3a6d31e2"]);
    assert_eq!(
        failure.log_lookup,
        "opencode logs --error <provider-error-ref>"
    );
}

fn selected_component_contract(
    slug: &str,
    path: &std::path::Path,
) -> crate::agent_task::AgentTaskComponentContract {
    let mut extra = serde_json::Map::new();
    extra.insert("loadMode".to_string(), json!("runtime-loadable"));
    extra.insert("activate".to_string(), json!(true));
    crate::agent_task::AgentTaskComponentContract {
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
    component: crate::agent_task::AgentTaskComponentContract,
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
