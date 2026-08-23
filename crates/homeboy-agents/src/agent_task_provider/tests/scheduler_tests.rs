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
