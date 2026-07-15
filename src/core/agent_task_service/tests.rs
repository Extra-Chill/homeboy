#![cfg(test)]

use super::*;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspace,
    AgentTaskWorkspaceMode, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_lifecycle::{status as lifecycle_status, AgentTaskRunState};
use crate::core::agent_task_schedule::AgentTaskPlan;
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskState,
};
use crate::core::run_lifecycle_record::RunExecutionState;
use crate::core::{agent_task_lifecycle, worktree};
use crate::test_support::with_isolated_home;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn status_reconciles_deferred_candidate_once_and_artifacts_see_the_projection() {
    with_isolated_home(|_| {
        let fixture = deferred_cleanup_fixture("candidate_recovered");
        let record = lifecycle_status(&fixture.run_id).expect("reconciled status");
        assert_eq!(record.state, AgentTaskRunState::PartialRecoverable);
        assert_eq!(record.totals.expect("totals").recoverable_candidates, 1);
        assert_eq!(
            artifacts(&fixture.run_id)
                .expect("artifacts")
                .artifacts
                .len(),
            2
        );

        let aggregate = agent_task_lifecycle::read_aggregate(&fixture.run_id).expect("aggregate");
        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::PartialRecoverable
        );
        assert_eq!(aggregate.outcomes[0].artifacts.len(), 2);
        assert!(
            !agent_task_lifecycle::reconcile_deferred_candidate(&fixture.run_id)
                .expect("idempotent")
        );
        assert_eq!(
            agent_task_lifecycle::read_aggregate(&fixture.run_id)
                .expect("aggregate")
                .outcomes[0]
                .artifacts
                .len(),
            2
        );
    });
}

#[test]
fn deferred_cleanup_pending_or_no_candidate_keeps_timeout_truthful() {
    with_isolated_home(|_| {
        for state in ["pending", "completed_no_candidate"] {
            let fixture = deferred_cleanup_fixture(state);
            let record = lifecycle_status(&fixture.run_id).expect("status");
            assert_eq!(record.state, AgentTaskRunState::Failed, "{state}");
            assert_eq!(
                agent_task_lifecycle::read_aggregate(&fixture.run_id)
                    .expect("aggregate")
                    .outcomes[0]
                    .status,
                AgentTaskOutcomeStatus::Timeout,
                "{state}"
            );
        }
    });
}

#[test]
fn deferred_cleanup_failure_surfaces_its_diagnostic_without_reclassifying_timeout() {
    with_isolated_home(|_| {
        let fixture = deferred_cleanup_fixture("failed");
        let aggregate = agent_task_lifecycle::read_aggregate(&fixture.run_id).expect("aggregate");
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        let record = lifecycle_status(&fixture.run_id).expect("status");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert!(agent_task_lifecycle::read_aggregate(&fixture.run_id)
            .expect("aggregate")
            .outcomes[0]
            .diagnostics
            .iter()
            .any(|entry| entry.class == "agent_task.deferred_cleanup_failed"));
    });
}

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
fn submitted_terminal_runs_reuse_durable_evidence_without_reexecution() {
    with_isolated_home(|_| {
        for (run_id, expected_exit_code) in [("terminal-succeeded", 0), ("terminal-failed", 1)] {
            if run_id == "terminal-succeeded" {
                run_loaded_plan(test_plan(), Some(run_id), SucceedingExecutor)
                    .expect("succeeded run completed");
            } else {
                run_loaded_plan(test_plan(), Some(run_id), TimeoutExecutor)
                    .expect("failed run completed");
            }

            let observed_request = Arc::new(Mutex::new(None));
            let result = run_submitted(
                run_id.to_string(),
                CapturingExecutor {
                    observed_request: Arc::clone(&observed_request),
                },
            )
            .expect("terminal run returns its durable aggregate");

            assert_eq!(result.exit_code, expected_exit_code);
            assert!(observed_request.lock().expect("executor lock").is_none());
        }
    });
}

#[test]
fn cancelled_terminal_run_is_not_reexecuted_without_durable_aggregate() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("terminal-cancelled"))
            .expect("run submitted");
        agent_task_lifecycle::cancel_run("terminal-cancelled", Some("test cancellation"))
            .expect("run cancelled");

        let observed_request = Arc::new(Mutex::new(None));
        let error = run_submitted(
            "terminal-cancelled".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect_err("cancelled run has no aggregate to reuse");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("terminal with state Cancelled"));
        assert!(observed_request.lock().expect("executor lock").is_none());
    });
}

#[test]
fn submitted_incomplete_run_still_executes_for_recovery() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("incomplete-queued"))
            .expect("run submitted");

        let observed_request = Arc::new(Mutex::new(None));
        let result = run_submitted(
            "incomplete-queued".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("queued run recovers through normal execution");

        assert_eq!(result.exit_code, 0);
        assert!(observed_request.lock().expect("executor lock").is_some());
        assert_eq!(
            lifecycle_status("incomplete-queued").expect("status").state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn service_persists_timed_out_run_record_and_evidence_refs() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(test_plan(), Some("service-timeout"), TimeoutExecutor)
            .expect("timeout run completed");
        let record = lifecycle_status("service-timeout").expect("status persisted");
        let artifacts = artifacts("service-timeout").expect("artifacts persisted");

        assert_eq!(result.exit_code, 1);
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::TimedOut);
        assert_eq!(record.totals.as_ref().expect("totals").timed_out, 1);
        assert!(record.aggregate_path.is_some());
        assert!(record
            .artifact_refs
            .iter()
            .any(|artifact| artifact.kind == "executor-result"));
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-result"));
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
        let attempt_root = Path::new(
            observed
                .workspace
                .root
                .as_deref()
                .expect("attempt workspace"),
        );
        assert_ne!(attempt_root, Path::new(&record.worktree_path));
        assert!(
            !attempt_root.exists(),
            "attempt workspace is retired after dispatch"
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

struct TimeoutExecutor;

impl AgentTaskExecutorAdapter for TimeoutExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some("provider exceeded timeout_ms=50".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "executor-result".to_string(),
                uri: "file:///tmp/executor-result.json".to_string(),
                label: Some("executor result".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_task.provider_timeout".to_string(),
                message: "provider exceeded timeout_ms=50".to_string(),
                data: serde_json::json!({ "timeout_ms": 50 }),
            }],
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
    crate::test_support::run_git_fixture_command(path, &["init", "-q"]);
    crate::test_support::run_git_fixture_command(
        path,
        &["config", "user.email", "homeboy@example.com"],
    );
    crate::test_support::run_git_fixture_command(path, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("readme");
    crate::test_support::run_git_fixture_command(path, &["add", "."]);
    crate::test_support::run_git_fixture_command(path, &["commit", "-q", "-m", "initial"]);
}

struct DeferredCleanupFixture {
    run_id: String,
    _directory: tempfile::TempDir,
}

fn deferred_cleanup_fixture(status: &str) -> DeferredCleanupFixture {
    use sha2::{Digest, Sha256};

    let directory = tempfile::tempdir().expect("fixture directory");
    let run_id = format!("deferred-{status}");
    let action_path = directory.path().join("deferred-cleanup.json");
    let patch_path = directory.path().join("candidate.patch");
    let patch = "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
    std::fs::write(&patch_path, patch).expect("patch");
    let sha256 = format!("{:x}", Sha256::digest(patch.as_bytes()));
    let candidate = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "candidate".to_string(),
        kind: "patch".to_string(),
        name: Some("candidate.patch".to_string()),
        label: None,
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some(patch_path.display().to_string()),
        url: Some(format!(
            "homeboy://agent-task/run/{run_id}/artifacts#task=service-task&artifact=candidate"
        )),
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(sha256),
        metadata: serde_json::json!({ "role": "patch" }),
    };
    let mut action = serde_json::json!({
        "schema": "homeboy/agent-task-deferred-cleanup/v1",
        "status": status,
        "run_id": run_id,
        "task_id": "service-task",
        "attempt": 1,
    });
    if status == "candidate_recovered" {
        action["candidate_artifacts"] = serde_json::json!([candidate]);
    }
    if status == "failed" {
        action["diagnostic"] = serde_json::json!("worker cleanup could not remove workspace");
    }
    std::fs::write(
        &action_path,
        serde_json::to_vec(&action).expect("action JSON"),
    )
    .expect("action");
    let cleanup_action = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "cleanup".to_string(),
        kind: "cleanup_action".to_string(),
        name: None,
        label: None,
        role: Some("cleanup_action".to_string()),
        semantic_key: None,
        path: Some(action_path.display().to_string()),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: None,
        sha256: None,
        metadata: serde_json::json!({ "run_id": run_id, "task_id": "service-task", "attempt": 1 }),
    };
    let aggregate = AgentTaskAggregate {
        schema: crate::core::agent_task_scheduler::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: "service-plan".to_string(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            timed_out: 1,
            ..Default::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "service-task".to_string(),
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some("deadline expired".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
            artifacts: vec![cleanup_action],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }],
        events: Vec::new(),
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    };
    agent_task_lifecycle::record_completed_run(&test_plan(), &aggregate, Some(&run_id))
        .expect("persist timeout");
    DeferredCleanupFixture {
        run_id,
        _directory: directory,
    }
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
