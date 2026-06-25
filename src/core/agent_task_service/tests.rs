#![cfg(test)]

use super::*;
use crate::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy,
    AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_lifecycle::{status as lifecycle_status, AgentTaskRunState};
use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
use crate::core::run_lifecycle_record::RunExecutionState;
use crate::test_support::with_isolated_home;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn service_run_loaded_plan_persists_durable_lifecycle() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(test_plan(), Some("service-run"), SucceedingExecutor)
            .expect("service run completed");
        let record = lifecycle_status("service-run").expect("status persisted");

        assert_eq!(result.exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
        assert!(record.aggregate_path.is_some());
    });
}

#[test]
fn service_normalizes_resolved_component_worktree_plan() {
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("homeboy".to_string());
    plan.tasks[0].workspace.materialization = serde_json::json!({
        "resolved_root": "/tmp/homeboy@service"
    });

    normalize_plan_workspaces(&mut plan).expect("workspace normalized");

    assert!(plan.tasks[0].workspace.kind.is_none());
    assert_eq!(plan.tasks[0].workspace.slug.as_deref(), Some("homeboy"));
    assert_eq!(
        plan.tasks[0].workspace.root.as_deref(),
        Some("/tmp/homeboy@service")
    );
    assert_eq!(
        plan.tasks[0].workspace.mode,
        AgentTaskWorkspaceMode::Existing
    );
    assert!(plan.tasks[0].workspace.materialization.is_null());
}

#[test]
fn service_materializes_component_worktree_before_provider_dispatch() {
    with_isolated_home(|home| {
        let repo = home.path().join("fixture");
        create_git_repo(&repo);
        write_component_registration(home.path(), "fixture", &repo);
        let observed_request = Arc::new(Mutex::new(None));
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("fixture".to_string());
        plan.tasks[0].workspace.branch = Some("fix/service-task".to_string());
        plan.tasks[0].workspace.base_ref = Some("HEAD".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: "https://example.com/tasks/123".to_string(),
            revision: None,
        }];

        let result = run_loaded_plan(
            plan,
            Some("service-materialized-worktree"),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");
        let record = worktree::resolve("fixture@fix-service-task").expect("worktree record");

        assert_eq!(result.exit_code, 0);
        assert_eq!(
            record.run_id.as_deref(),
            Some("service-materialized-worktree")
        );
        assert_eq!(
            record.task_url.as_deref(),
            Some("https://example.com/tasks/123")
        );
        assert_eq!(
            record.cleanup_policy,
            worktree::CleanupPolicy::PreserveOnFailure
        );
        assert_eq!(observed.workspace.mode, AgentTaskWorkspaceMode::Existing);
        assert_eq!(
            observed.workspace.root.as_deref(),
            Some(record.worktree_path.as_str())
        );
        assert_eq!(observed.workspace.slug.as_deref(), Some("fixture"));
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert_eq!(observed.workspace.cleanup.as_deref(), Some("preserve"));
        assert_eq!(
            observed.workspace.materialization["id"].as_str(),
            Some("fixture@fix-service-task")
        );
        assert!(Path::new(&record.worktree_path).is_dir());
    });
}

#[test]
fn run_next_records_failed_lifecycle_when_prepare_after_claim_fails() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0]
            .executor
            .secret_env
            .push("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__".to_string());
        std::env::remove_var("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__");
        agent_task_lifecycle::submit_plan(&plan, Some("run-next-preexecution-fails"))
            .expect("submitted");

        let error = run_next(SucceedingExecutor).expect_err("prepare failure returned");
        let record = lifecycle_status("run-next-preexecution-fails").expect("status persisted");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(record.lifecycle.execution.state, RunExecutionState::Failed);
        assert!(record.lifecycle.execution.finished_at.is_some());
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "prepare_plan_for_execution"
        );
        assert!(record.aggregate_path.is_some());
    });
}

#[test]
fn discovery_lists_durable_runs_with_operator_commands() {
    with_isolated_home(|_| {
        let plan = discovery_plan();
        agent_task_lifecycle::submit_plan(&plan, Some("run-discovery-list")).expect("submitted");

        let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
        let run = report.runs.first().expect("run");

        assert_eq!(report.schema, "homeboy/agent-task-discovery/v1");
        assert_eq!(report.filter, "all");
        assert_eq!(report.count, 1);
        assert_eq!(report.total, 1);
        assert!(!report.truncated);
        assert!(report.limit.is_none());
        assert!(report
            .lab_discovery
            .runner_scoped_command
            .contains("--runner"));
        assert!(report.lab_discovery.fallback_command.contains("runs list"));
        assert_eq!(run.run_id, "run-discovery-list");
        assert_eq!(run.state, AgentTaskRunState::Queued);
        assert_eq!(run.repo.as_deref(), Some("homeboy"));
        assert_eq!(run.workspace.as_deref(), Some("/tmp/homeboy"));
        assert_eq!(
            run.task_url.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/issues/4386")
        );
        assert_eq!(run.counts.queued, 1);
        assert!(run
            .commands
            .status
            .ends_with("agent-task status run-discovery-list"));
        assert!(run
            .commands
            .logs
            .ends_with("agent-task logs run-discovery-list"));
        assert!(run
            .commands
            .artifacts
            .ends_with("agent-task artifacts run-discovery-list"));
        assert!(run
            .commands
            .review
            .ends_with("agent-task review run-discovery-list"));
        assert!(run
            .commands
            .retry
            .ends_with("agent-task retry run-discovery-list --run"));
        assert!(run
            .commands
            .run_plan
            .contains("homeboy --runner <runner-id> agent-task run-plan --plan @"));
        assert!(run
            .commands
            .run_plan
            .contains("/agent-task-runs/run-discovery-list/plan.json"));
    });
}

#[test]
fn discovery_active_filters_to_queued_and_running_runs() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-queued"))
            .expect("queued submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-running"))
            .expect("running submitted");
        agent_task_lifecycle::mark_running("run-active-running").expect("marked running");
        run_loaded_plan(
            discovery_plan(),
            Some("run-active-complete"),
            SucceedingExecutor,
        )
        .expect("completed");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
        let run_ids: Vec<_> = report.runs.iter().map(|run| run.run_id.as_str()).collect();

        assert_eq!(report.filter, "active");
        assert_eq!(report.count, 2);
        assert!(run_ids.contains(&"run-active-queued"));
        assert!(run_ids.contains(&"run-active-running"));
        assert!(!run_ids.contains(&"run-active-complete"));
    });
}

#[test]
fn discovery_active_marks_runner_backed_running_run_as_stale_retryable() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-stale"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-runner-stale", |record| {
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-123",
            });
        })
        .expect("running runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
        let run = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-runner-stale")
            .expect("runner-backed run listed");

        assert_eq!(run.runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(run.runner_job_id.as_deref(), Some("job-123"));
        assert_eq!(run.stale, Some(true));
        assert_eq!(
            run.stale_reason.as_deref(),
            Some("runner_job_unverified_after_daemon_restart")
        );
        assert_eq!(run.retryable, Some(true));
    });
}

#[test]
fn discovery_active_classifies_liveness_and_source() {
    with_isolated_home(|_| {
        // Queued run: always classified active.
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-queued"))
            .expect("queued submitted");

        // Stale runner-backed run: lifecycle flags it stale -> Stale.
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-stale"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-live-stale", |record| {
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-xyz",
            });
        })
        .expect("stale runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");

        let queued = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-queued")
            .expect("queued listed");
        assert_eq!(queued.liveness, Some(AgentTaskLiveness::Active));
        assert!(queued.source == "local" || queued.source.starts_with("runner:"));

        let stale = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-stale")
            .expect("stale listed");
        assert_eq!(stale.liveness, Some(AgentTaskLiveness::Stale));
        assert_eq!(stale.source, "runner:homeboy-lab");

        let summary = report.liveness_summary.expect("active summary present");
        assert!(summary.active >= 1);
        assert_eq!(summary.stale, 1);
        assert_eq!(
            summary.reconcile_command,
            "homeboy agent-task active --reconcile"
        );
    });
}

#[test]
fn reconcile_dry_run_reports_but_does_not_cancel_stale_runs() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-dry"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-reconcile-dry", |record| {
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-dry",
            });
        })
        .expect("stale record stored");

        let report = reconcile_stale_active_runs(true).expect("dry run reconciled");
        assert!(report.dry_run);
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.considered, 1);
        assert_eq!(report.runs[0].action, "would-reconcile");

        // Record must remain running after a dry run.
        let record = lifecycle_status("run-reconcile-dry").expect("status");
        assert_eq!(record.state, AgentTaskRunState::Running);
    });
}

#[test]
fn reconcile_cancels_stale_running_record_without_manual_edit() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-live"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-reconcile-live", |record| {
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-live",
            });
        })
        .expect("stale record stored");

        let report = reconcile_stale_active_runs(false).expect("reconciled");
        assert!(!report.dry_run);
        assert_eq!(report.reconciled, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.runs[0].action, "reconciled");

        let record = lifecycle_status("run-reconcile-live").expect("status");
        assert_eq!(record.state, AgentTaskRunState::Cancelled);

        // A genuinely-active run reconcile pass leaves nothing to do.
        let empty = reconcile_stale_active_runs(false).expect("nothing to reconcile");
        assert_eq!(empty.considered, 0);
        assert_eq!(empty.reconciled, 0);
    });
}

#[test]
fn discovery_latest_returns_only_newest_run() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-a"))
            .expect("first submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-z"))
            .expect("second submitted");

        let report = discover_runs(AgentTaskDiscoveryFilter::Latest).expect("latest listed");

        assert_eq!(report.filter, "latest");
        assert_eq!(report.count, 1);
        assert_eq!(report.runs[0].run_id, "run-latest-z");
    });
}

#[test]
fn discovery_limit_caps_list_and_reports_total() {
    with_isolated_home(|_| {
        for run_id in ["run-cap-a", "run-cap-b", "run-cap-c"] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        }

        let report = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions { limit: Some(2) },
        )
        .expect("listed with limit");

        assert_eq!(report.count, 2);
        assert_eq!(report.total, 3);
        assert_eq!(report.limit, Some(2));
        assert!(report.truncated);
        assert_eq!(report.runs.len(), 2);
    });
}

#[test]
fn discovery_latest_ignores_limit() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-a"))
            .expect("submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-z"))
            .expect("submitted");

        let report = discover_runs_with_options(
            AgentTaskDiscoveryFilter::Latest,
            AgentTaskDiscoveryOptions { limit: Some(5) },
        )
        .expect("latest listed");

        // `latest` is always a single run; a limit is a no-op and not echoed.
        assert_eq!(report.count, 1);
        assert!(report.limit.is_none());
        assert!(!report.truncated);
    });
}

#[test]
fn discovery_runner_backed_run_emits_runner_scoped_commands() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-commands"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-runner-commands", |record| {
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
            });
        })
        .expect("runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
        let run = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-runner-commands")
            .expect("runner-backed run listed");

        // Commands must be valid for the run's location: runner-scoped.
        assert_eq!(
            run.commands.status,
            "homeboy --runner homeboy-lab agent-task status run-runner-commands"
        );
        assert_eq!(
            run.commands.logs,
            "homeboy --runner homeboy-lab agent-task logs run-runner-commands"
        );
        assert_eq!(
            run.commands.review,
            "homeboy --runner homeboy-lab agent-task review run-runner-commands"
        );
        assert_eq!(
            run.commands.reconcile,
            "homeboy --runner homeboy-lab agent-task cancel run-runner-commands --reason stale-running"
        );
    });
}

struct SucceedingExecutor;

impl AgentTaskExecutorAdapter for SucceedingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct CapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

impl AgentTaskExecutorAdapter for CapturingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        *self
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

fn create_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("repo dir");
    run_git(path, &["init", "-q"]);
    run_git(path, &["config", "user.email", "homeboy@example.com"]);
    run_git(path, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("readme");
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "initial"]);
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
    let dir = home.join(".config/homeboy/components");
    std::fs::create_dir_all(&dir).expect("components dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::json!({
            "local_path": local_path,
            "remote_path": format!("wp-content/plugins/{id}")
        })
        .to_string(),
    )
    .expect("component registration");
}

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "service-plan",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "service-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("service".to_string()),
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
        }],
    )
}

fn discovery_plan() -> AgentTaskPlan {
    let mut plan = test_plan();
    plan.group_key = Some("homeboy".to_string());
    plan.tasks[0].group_key = Some("homeboy".to_string());
    plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
        kind: "task".to_string(),
        uri: "https://github.com/Extra-Chill/homeboy/issues/4386".to_string(),
        revision: None,
    }];
    plan.tasks[0].workspace.root = Some("/tmp/homeboy".to_string());
    plan.tasks[0].workspace.slug = Some("homeboy".to_string());
    plan
}
