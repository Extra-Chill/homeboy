//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::super::*;
use super::fixtures::*;
use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskArtifactDeclaration,
    AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

mod adaptive_concurrency_tests {
    use super::*;

    #[test]
    fn adaptive_concurrency_scales_up_when_runner_slots_are_available() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 1;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            max_concurrency: Some(3),
            runner_capacity: Some(3),
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert!(max_seen.load(Ordering::SeqCst) > 1);
        assert!(max_seen.load(Ordering::SeqCst) <= 3);
        assert_eq!(adaptive.configured_max_concurrency, 1);
        assert_eq!(adaptive.max_concurrency, 3);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Increased
                && decision.effective_concurrency == 3
                && decision.reason.contains("runner slots are available")
        }));
    }

    #[test]
    fn adaptive_concurrency_scales_down_under_runner_pressure() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 4;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            max_concurrency: Some(4),
            runner_capacity: Some(3),
            active_leases: 2,
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(adaptive.effective_concurrency, 1);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Decreased
                && decision.reason.contains("available runner slots 1")
        }));
    }

    #[test]
    fn adaptive_concurrency_pauses_and_blocks_when_runner_capacity_is_unavailable() {
        let executor = RecordingExecutor {
            statuses: HashMap::new(),
            delay: Duration::from_millis(0),
            running: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            cancel_calls: Arc::new(Mutex::new(Vec::new())),
        };
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            runner_capacity: Some(1),
            active_leases: 1,
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.blocked, 2);
        assert_eq!(max_seen.load(Ordering::SeqCst), 0);
        assert_eq!(adaptive.effective_concurrency, 0);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Paused
                && decision.reason.contains("consume runner_capacity=1")
        }));
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "adaptive_concurrency"));
    }

    #[test]
    fn adaptive_concurrency_status_records_held_decision() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(1);
        plan.options.max_concurrency = 2;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy::default());

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(adaptive.effective_concurrency, 2);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Held
                && decision.reason.contains("configured ceiling")
        }));
    }
}

mod concurrency_tests {
    use super::*;

    pub(super) fn init_git_workspace(path: &std::path::Path) {
        fs::create_dir(path).expect("workspace directory");
        for args in [
            ["init", "-b", "main"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "Homeboy Test"].as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git command")
                .success());
        }
        fs::write(path.join("base.txt"), "base\n").expect("base file");
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .expect("stage base")
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(path)
            .status()
            .expect("commit base")
            .success());
    }

    #[test]
    fn schedules_tasks_with_bounded_concurrency_and_success_aggregate() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.totals.succeeded, 4);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.state == AgentTaskState::Running));
    }

    #[test]
    fn blocks_tasks_over_queue_depth_and_reports_backpressure() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(3);
        plan.options.max_queue_depth = Some(2);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.blocked, 1);
        assert_eq!(aggregate.queue.blocked, 1);
        assert_eq!(aggregate.queue.max_queue_depth, Some(2));
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "queue_depth"));
        assert!(aggregate
            .events
            .iter()
            .any(|event| { event.task_id == "task-3" && event.state == AgentTaskState::Blocked }));
    }

    #[test]
    fn applies_per_executor_concurrency_below_global_limit() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.options
            .per_executor_concurrency
            .insert("test".to_string(), 1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(
            aggregate.queue.per_executor_concurrency.get("test"),
            Some(&1)
        );
    }

    #[test]
    fn applies_per_model_concurrency_below_global_limit() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        for task in &mut plan.tasks {
            task.executor.model = Some("model-a".to_string());
        }
        plan.options.max_concurrency = 3;
        plan.options
            .per_model_concurrency
            .insert("test:model-a".to_string(), 1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(
            aggregate.queue.per_model_concurrency.get("test:model-a"),
            Some(&1)
        );
    }

    #[test]
    fn serializes_tasks_that_share_a_mutable_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        init_git_workspace(&workspace);
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        for task in &mut plan.tasks {
            task.workspace.root = Some(workspace.display().to_string());
        }

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn root_and_subdirectory_share_the_same_git_workspace_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        init_git_workspace(&workspace);
        let subdirectory = workspace.join("src");
        fs::create_dir(&subdirectory).expect("subdirectory");
        let mut root_request = request("root");
        root_request.workspace.root = Some(workspace.display().to_string());
        let mut subdirectory_request = request("subdirectory");
        subdirectory_request.workspace.root = Some(subdirectory.display().to_string());

        assert_eq!(
            AgentTaskScheduleSupport::workspace_key(&root_request),
            AgentTaskScheduleSupport::workspace_key(&subdirectory_request)
        );
    }

    #[test]
    fn serializes_tasks_with_the_same_declared_exclusive_resource() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        for task in &mut plan.tasks {
            task.limits.exclusive_resource_keys = vec!["cache:shared".to_string()];
        }

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-2"
                && event.state == AgentTaskState::Blocked
                && event.message.as_deref().is_some_and(|message| {
                    message.contains("waiting for exclusive resource 'cache:shared'")
                        && message.contains("held by 'task-1'")
                        && message.contains("ms elapsed")
                })
        }));
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-2"
                && event.state == AgentTaskState::Running
                && event.message.as_deref().is_some_and(|message| {
                    message.contains("acquired exclusive resource 'cache:shared' after waiting")
                        && message.contains("previous holder 'task-1'")
                })
        }));
    }

    #[test]
    fn unrelated_declared_resources_remain_concurrent() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        plan.tasks[0].limits.exclusive_resource_keys = vec!["cache:one".to_string()];
        plan.tasks[1].limits.exclusive_resource_keys = vec!["cache:two".to_string()];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(max_seen.load(Ordering::SeqCst), 2);
        assert!(!aggregate.events.iter().any(|event| {
            event.state == AgentTaskState::Blocked
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("exclusive resource"))
        }));
    }

    #[derive(Clone)]
    struct ResourceTimeoutExecutor;

    impl AgentTaskExecutorAdapter for ResourceTimeoutExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if request.task_id == "task-1" {
                // Keep task-2 queued on the resource for longer than its
                // execution deadline. Its own immediate execution must still
                // succeed after admission.
                std::thread::sleep(Duration::from_millis(150));
            }
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    #[test]
    fn execution_timeout_excludes_declared_resource_wait() {
        let scheduler = AgentTaskScheduler::new(ResourceTimeoutExecutor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        for task in &mut plan.tasks {
            task.limits.exclusive_resource_keys = vec!["cache:timeout-test".to_string()];
        }
        plan.tasks[0].limits.timeout_ms = Some(500);
        plan.tasks[1].limits.timeout_ms = Some(50);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(aggregate
            .outcomes
            .iter()
            .all(|outcome| { outcome.status == AgentTaskOutcomeStatus::Succeeded }));
        let resource_wait = aggregate
            .events
            .iter()
            .find(|event| event.task_id == "task-2" && event.state == AgentTaskState::Running)
            .and_then(|event| event.message.as_deref())
            .expect("task-2 resource acquisition event");
        assert!(resource_wait.contains("cache:timeout-test"));
        let waited_ms = resource_wait
            .split("after waiting ")
            .nth(1)
            .and_then(|value| value.split(" ms").next())
            .and_then(|value| value.parse::<u128>().ok())
            .expect("resource wait duration in lifecycle event");
        assert!(
            waited_ms >= 50,
            "resource wait ({waited_ms} ms) must exceed task-2's 50 ms execution timeout"
        );
    }

    #[test]
    fn serializes_overlapping_non_git_workspace_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let child = workspace.join("child");
        fs::create_dir(&workspace).expect("workspace");
        fs::create_dir(&child).expect("child");
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[1].workspace.root = Some(workspace.join(".").display().to_string());
        plan.tasks[2].workspace.root = Some(child.display().to_string());

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
    }

    #[derive(Clone)]
    struct LateMutatingExecutor {
        events: Arc<Mutex<Vec<String>>>,
        finished: Arc<AtomicBool>,
        attempt_roots: Arc<Mutex<Vec<std::path::PathBuf>>>,
    }

    impl AgentTaskExecutorAdapter for LateMutatingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.events
                .lock()
                .expect("events")
                .push(format!("{}-started", request.task_id));
            if request.task_id == "task-1" {
                let workspace = request
                    .workspace
                    .root
                    .as_deref()
                    .map(std::path::PathBuf::from)
                    .expect("attempt workspace");
                self.attempt_roots
                    .lock()
                    .expect("attempt roots")
                    .push(workspace.clone());
                std::thread::sleep(Duration::from_millis(3_000));
                fs::write(workspace.join("late-change.txt"), "late\n")
                    .expect("late workspace mutation");
                assert!(Command::new("git")
                    .args(["add", "late-change.txt"])
                    .current_dir(&workspace)
                    .status()
                    .expect("stage late mutation")
                    .success());
                assert!(Command::new("git")
                    .args(["commit", "-m", "late mutation"])
                    .current_dir(&workspace)
                    .status()
                    .expect("commit late mutation")
                    .success());
            }
            self.events
                .lock()
                .expect("events")
                .push(format!("{}-finished", request.task_id));
            if request.task_id == "task-1" {
                self.finished.store(true, Ordering::SeqCst);
            }
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    #[test]
    fn timeout_quarantines_only_its_workspace_without_starving_unrelated_work() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let unrelated = temp.path().join("unrelated");
        init_git_workspace(&workspace);
        init_git_workspace(&unrelated);
        let events = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let attempt_roots = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(LateMutatingExecutor {
            events: Arc::clone(&events),
            finished: Arc::clone(&finished),
            attempt_roots: Arc::clone(&attempt_roots),
        });
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[1].workspace.root = Some(workspace.display().to_string());
        plan.tasks[2].workspace.root = Some(unrelated.display().to_string());

        let started = Instant::now();
        let aggregate = scheduler.run(plan);
        assert!(started.elapsed() >= Duration::from_secs(2));
        while !finished.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let events = events.lock().expect("events");

        assert!(events.iter().any(|event| event == "task-3-started"));
        assert!(events.iter().any(|event| event == "task-2-started"));
        assert!(aggregate
            .outcomes
            .iter()
            .any(|outcome| outcome.task_id == "task-1"
                && outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable
                && outcome.artifacts.iter().any(|artifact| {
                    artifact.kind == "patch"
                        && artifact
                            .sha256
                            .as_deref()
                            .is_some_and(|sha| sha.len() == 64)
                        && artifact.metadata["provider_backend"].is_string()
                })));
        assert!(aggregate.outcomes.iter().any(|outcome| {
            outcome.task_id == "task-2" && outcome.status == AgentTaskOutcomeStatus::Succeeded
        }));
        assert!(
            !workspace.join("late-change.txt").exists(),
            "the late provider mutation must stay out of the caller workspace"
        );
    }

    #[test]
    fn rejects_preexisting_caller_dirt_before_attempt_checkout_or_provider_dispatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        init_git_workspace(&workspace);
        fs::write(workspace.join("user-edit.txt"), "keep me\n").expect("user edit");
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(0));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(max_seen.load(Ordering::SeqCst), 0);
        assert!(aggregate.outcomes[0].diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.committed_harvest_dirty_workspace"
                && diagnostic.data["status"]
                    .as_str()
                    .is_some_and(|status| status.contains("user-edit.txt"))
        }));
        assert_eq!(
            fs::read_to_string(workspace.join("user-edit.txt")).unwrap(),
            "keep me\n"
        );
    }

    #[derive(Clone)]
    struct PermissionDeniedThenCommitExecutor {
        attempts: Arc<AtomicUsize>,
        workspaces: Arc<Mutex<Vec<std::path::PathBuf>>>,
    }

    impl AgentTaskExecutorAdapter for PermissionDeniedThenCommitExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let workspace = std::path::PathBuf::from(
                request
                    .workspace
                    .root
                    .as_deref()
                    .expect("isolated workspace"),
            );
            self.workspaces
                .lock()
                .expect("workspaces")
                .push(workspace.clone());
            assert_eq!(
                request.executor.config["workspace"]["root"],
                workspace.display().to_string()
            );
            assert_eq!(
                request.executor.config["workspace_root"],
                workspace.display().to_string()
            );
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            std::fs::write(
                workspace.join("agent-change.txt"),
                format!("attempt {attempt}\n"),
            )
            .expect("write agent change");
            if attempt == 1 {
                return AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: request.task_id,
                    status: AgentTaskOutcomeStatus::Failed,
                    summary: Some("permission denied".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::Transient),
                    artifacts: Vec::new(),
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: vec![AgentTaskDiagnostic {
                        class: "provider.permission_denied".to_string(),
                        message: "executor was denied the requested operation".to_string(),
                        data: json!({ "operation": "write_file" }),
                    }],
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                };
            }
            assert!(Command::new("git")
                .args(["add", "agent-change.txt"])
                .current_dir(&workspace)
                .status()
                .expect("stage change")
                .success());
            assert!(Command::new("git")
                .args(["commit", "-m", "agent change"])
                .current_dir(&workspace)
                .status()
                .expect("commit change")
                .success());
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    #[test]
    fn retry_uses_clean_isolated_workspace_after_permission_denial() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        init_git_workspace(&source);
        assert!(Command::new("git")
            .args([
                "clone",
                source.to_str().expect("source path"),
                target.to_str().expect("target path")
            ])
            .status()
            .expect("clone target")
            .success());
        let executor = PermissionDeniedThenCommitExecutor {
            attempts: Arc::new(AtomicUsize::new(0)),
            workspaces: Arc::new(Mutex::new(Vec::new())),
        };
        let attempts = Arc::clone(&executor.attempts);
        let workspaces = Arc::clone(&executor.workspaces);
        let scheduler = AgentTaskScheduler::new(executor).with_run_id("retry-isolation");
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(source.display().to_string());
        plan.tasks[0].executor.config = json!({
            "workspace": { "root": source.display().to_string() },
            "workspace_root": source.display().to_string(),
        });
        plan.options.retry.max_attempts = 2;
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Transient];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let workspaces = workspaces.lock().expect("workspaces");
        assert_eq!(workspaces.len(), 2);
        assert_ne!(workspaces[0], workspaces[1]);
        assert!(workspaces.iter().all(|workspace| workspace != &source));
        assert!(workspaces.iter().all(|workspace| !workspace.exists()));
        assert!(Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&source)
            .output()
            .expect("source status")
            .stdout
            .is_empty());
        assert!(!target.join("agent-change.txt").exists());
        assert!(aggregate.outcomes[0]
            .artifacts
            .iter()
            .any(|artifact| artifact.id == "task-1-attempt-2-committed-changes"));
        let retry = aggregate.outcomes[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
            .expect("failed retry evidence");
        assert_eq!(
            retry.data["diagnostics"][0]["class"],
            "provider.permission_denied"
        );
        assert_eq!(
            retry.data["diagnostics"][0]["data"]["operation"],
            "write_file"
        );
        assert_eq!(
            retry.data["artifacts"][0]["id"],
            "task-1-attempt-1-uncommitted-changes"
        );
        assert_eq!(
            retry.data["artifacts"][0]["metadata"]["change_source"],
            "uncommitted_attempt_workspace"
        );
    }

    #[test]
    fn clean_workspace_without_executor_commits_remains_a_no_patch_success() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        init_git_workspace(&workspace);
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(aggregate.outcomes[0].artifacts.is_empty());
    }
}

mod resource_budget_tests {
    use super::*;

    #[test]
    fn resource_budget_limits_concurrent_task_cost() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 4;
        plan.options.resource_budget.max_active_units = Some(2);
        plan.options.resource_budget.default_task_units = 1;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 4);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
        assert_eq!(aggregate.queue.resource_budget.max_active_units, Some(2));
        assert_eq!(aggregate.queue.resource_budget.default_task_units, 1);
    }

    #[test]
    fn resource_budget_blocks_task_that_cannot_fit() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(1);
        plan.options.resource_budget.max_active_units = Some(2);
        plan.options.resource_budget.default_task_units = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.blocked, 1);
        assert_eq!(aggregate.queue.blocked, 1);
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "resource_budget"));
    }

    #[test]
    fn retry_budget_and_failure_classifications_gate_retries() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 3;
        plan.options.retry.max_retries_total = Some(0);
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Provider];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(aggregate.queue.retry_budget_remaining, Some(0));
    }
}

mod provider_rotation_tests {
    use super::*;

    struct DirtyCandidateThenSuccessExecutor {
        observed_roots: Arc<Mutex<Vec<std::path::PathBuf>>>,
        calls: AtomicUsize,
    }

    struct AdoptionExecutor {
        observed: Arc<Mutex<Option<crate::core::agent_task::AgentTaskAttemptWorkspace>>>,
    }

    impl AgentTaskExecutorAdapter for AdoptionExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let root = request.workspace.root.as_deref().expect("attempt root");
            assert!(std::path::Path::new(root).join("adopted.txt").is_file());
            self.observed
                .lock()
                .expect("observed workspace")
                .replace(request.workspace.attempt.expect("attempt ownership"));
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    impl AgentTaskExecutorAdapter for DirtyCandidateThenSuccessExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let root = request
                .workspace
                .root
                .as_deref()
                .map(std::path::PathBuf::from)
                .expect("attempt workspace");
            self.observed_roots
                .lock()
                .expect("observed roots")
                .push(root.clone());
            assert_eq!(
                request.executor.config["workspace_root"],
                root.display().to_string(),
                "provider workspace config follows the isolated attempt root"
            );
            assert_eq!(
                request.executor.config["cwd"],
                root.display().to_string(),
                "provider cwd follows the isolated attempt root"
            );
            if call == 0 {
                fs::write(root.join("candidate.txt"), "candidate\n").expect("candidate edit");
                let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Timeout);
                outcome.failure_classification = Some(AgentTaskFailureClassification::Timeout);
                return outcome;
            }
            assert!(
                !root.join("candidate.txt").exists(),
                "the rotated provider must receive a clean attempt checkout"
            );
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    fn rotation_policy(
        entries: Vec<AgentTaskProviderRotationEntry>,
    ) -> AgentTaskProviderRotationPolicy {
        AgentTaskProviderRotationPolicy {
            entries,
            max_attempts: None,
            ..AgentTaskProviderRotationPolicy::default()
        }
    }

    fn entry(backend: &str) -> AgentTaskProviderRotationEntry {
        AgentTaskProviderRotationEntry {
            backend: Some(backend.to_string()),
            ..AgentTaskProviderRotationEntry::default()
        }
    }

    fn enable_rotation(plan: &mut AgentTaskPlan) {
        plan.options.execution_budget = AgentTaskExecutionBudget {
            max_total_executions: 10,
            max_same_provider_retries: 0,
            max_provider_rotations: 10,
        };
    }

    fn provider_failure() -> (
        AgentTaskOutcomeStatus,
        Option<AgentTaskFailureClassification>,
    ) {
        (
            AgentTaskOutcomeStatus::ProviderError,
            Some(AgentTaskFailureClassification::Provider),
        )
    }

    fn success() -> (
        AgentTaskOutcomeStatus,
        Option<AgentTaskFailureClassification>,
    ) {
        (AgentTaskOutcomeStatus::Succeeded, None)
    }

    #[test]
    fn total_execution_budget_of_one_prevents_provider_rotation() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        plan.options.execution_budget = AgentTaskExecutionBudget {
            max_total_executions: 1,
            max_same_provider_retries: 0,
            max_provider_rotations: 1,
        };

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["exhausted"],
            "total_executions"
        );
    }

    #[test]
    fn rotates_to_next_entry_on_provider_failure_and_stops_at_first_success() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let observed = Arc::clone(&executor.observed);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![
            AgentTaskProviderRotationEntry {
                backend: Some("fallback-backend-a".to_string()),
                selector: Some("fallback-a.agent-task-executor".to_string()),
                model: Some("fallback-model-a".to_string()),
                provider_config: json!({ "provider": "fallback-provider-a" }),
            },
            entry("fallback-backend-b"),
        ]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed[0].executor.backend, "test");
        assert_eq!(observed[1].executor.backend, "fallback-backend-a");
        assert_eq!(
            observed[1].executor.selector.as_deref(),
            Some("fallback-a.agent-task-executor")
        );
        assert_eq!(
            observed[1].executor.model.as_deref(),
            Some("fallback-model-a")
        );
        assert_eq!(
            observed[1]
                .executor
                .config
                .get("provider")
                .and_then(Value::as_str),
            Some("fallback-provider-a")
        );

        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["attempt"], 1);
        assert_eq!(attempts[0]["backend"], "test");
        assert_eq!(attempts[0]["failure_classification"], "provider");
        assert_eq!(attempts[0]["status"], "provider_error");
        assert_eq!(attempts[1]["attempt"], 2);
        assert_eq!(attempts[1]["backend"], "fallback-backend-a");
        assert_eq!(attempts[1]["status"], "succeeded");
        assert!(aggregate.events.iter().any(|event| {
            event.message.as_deref() == Some("provider rotation queued: entry 1 of 2")
        }));
    }

    #[test]
    fn execution_budget_allows_exactly_one_total_provider_execution() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 0, 0);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0]
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
                .expect("budget reason")
                .data["exhausted_budget"],
            "max_provider_executions"
        );
    }

    #[test]
    fn execution_budget_caps_same_provider_retries() {
        let executor =
            RotationScriptedExecutor::new(vec![provider_failure(), provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(3, 1, 0);
        plan.options.retry.max_attempts = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            aggregate.outcomes[0]
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
                .expect("budget reason")
                .data["exhausted_budget"],
            "max_same_provider_retries"
        );
    }

    #[test]
    fn execution_budget_caps_provider_rotations_and_reports_exact_reason() {
        let executor =
            RotationScriptedExecutor::new(vec![provider_failure(), provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(3, 0, 1);
        plan.options.rotation = Some(rotation_policy(vec![
            entry("fallback-backend-a"),
            entry("fallback-backend-b"),
        ]));

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            aggregate.outcomes[0]
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
                .expect("budget reason")
                .data["exhausted_budget"],
            "max_provider_rotations"
        );
    }

    #[test]
    fn total_execution_budget_takes_precedence_over_retry_and_rotation_caps() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 9, 9);
        plan.options.retry.max_attempts = 10;
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0]
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
                .expect("budget reason")
                .data["exhausted_budget"],
            "max_provider_executions"
        );
    }

    #[test]
    fn execution_budget_allows_retry_then_rotation_within_total() {
        let executor =
            RotationScriptedExecutor::new(vec![provider_failure(), provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(3, 1, 1);
        plan.options.retry.max_attempts = 3;
        let mut policy = rotation_policy(vec![entry("fallback-backend-a")]);
        policy.max_attempts = Some(3);
        plan.options.rotation = Some(policy);

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
    }

    #[test]
    fn rotation_preserves_uncommitted_candidate_and_dispatches_next_provider_from_clean_baseline() {
        let _home = crate::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::concurrency_tests::init_git_workspace(&workspace);
        let observed_roots = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(DirtyCandidateThenSuccessExecutor {
            observed_roots: Arc::clone(&observed_roots),
            calls: AtomicUsize::new(0),
        })
        .with_run_id("run-8081");
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].executor.model = Some("primary-model".to_string());
        plan.tasks[0].executor.config = json!({
            "workspace_root": workspace.display().to_string(),
            "cwd": workspace.display().to_string(),
            "nested": { "workspace_root": workspace.display().to_string() },
        });
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        assert!(
            !workspace.join("candidate.txt").exists(),
            "the managed task worktree must remain untouched"
        );
        let roots = observed_roots.lock().expect("observed roots");
        assert_eq!(roots.len(), 2);
        assert_ne!(roots[0], workspace);
        assert_ne!(roots[1], workspace);
        assert_ne!(roots[0], roots[1]);
        let candidate = aggregate.outcomes[0]
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "task-1-attempt-1-uncommitted-changes")
            .expect("failed attempt patch candidate is retained for promotion");
        assert_eq!(candidate.kind, "patch");
        assert_eq!(candidate.metadata["producer_attempt"], 1);
        assert_eq!(candidate.metadata["provider_rotation_index"], 0);
        assert_eq!(candidate.metadata["provider_backend"], "test");
        assert_eq!(candidate.metadata["provider_model"], "primary-model");
        assert!(candidate
            .sha256
            .as_deref()
            .is_some_and(|sha256| sha256.len() == 64));
        assert_eq!(candidate.metadata["run_id"], "run-8081");
        assert_eq!(candidate.metadata["task_id"], "task-1");
        let patch = fs::read_to_string(candidate.path.as_deref().expect("candidate path"))
            .expect("candidate patch remains available");
        assert!(patch.contains("diff --git a/candidate.txt b/candidate.txt"));
        assert!(candidate
            .path
            .as_deref()
            .is_some_and(|path| path.contains("agent-task/attempt-patches/run-8081/task-1")));
        assert!(
            !roots[0].exists() && !roots[1].exists(),
            "attempt checkouts are retired after their executor threads stop"
        );
    }

    #[test]
    fn explicit_candidate_adoption_materializes_verified_patch_with_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::concurrency_tests::init_git_workspace(&workspace);
        let candidate_file = workspace.join("adopted.txt");
        fs::write(&candidate_file, "candidate\n").expect("candidate change");
        git_output(&workspace, &["add", "adopted.txt"]).expect("stage candidate");
        let patch = git_output_raw(
            &workspace,
            &[
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--",
                "adopted.txt",
            ],
        )
        .expect("candidate patch");
        git_output(&workspace, &["reset", "--hard", "HEAD"]).expect("restore clean task base");
        let patch_path = temp.path().join("candidate.patch");
        fs::write(&patch_path, &patch).expect("persist candidate patch");
        let expected_fingerprint = fingerprint(patch.as_bytes());
        let observed = Arc::new(Mutex::new(None));
        let scheduler = AgentTaskScheduler::new(AdoptionExecutor {
            observed: Arc::clone(&observed),
        });
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].workspace.attempt =
            Some(crate::core::agent_task::AgentTaskAttemptWorkspace {
                identity: "adoption-request".to_string(),
                base_ref: "ignored-before-materialization".to_string(),
                base_fingerprint: "ignored-before-materialization".to_string(),
                adoption: Some(crate::core::agent_task::AgentTaskCandidateAdoption {
                    source_attempt: "attempt-provider-a".to_string(),
                    patch_path: patch_path.display().to_string(),
                    patch_fingerprint: expected_fingerprint.clone(),
                    provider_backend: "provider-a".to_string(),
                    provider_model: Some("model-a".to_string()),
                    decision: "continue verified candidate".to_string(),
                }),
            });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        let attempt = observed
            .lock()
            .expect("observed workspace")
            .clone()
            .expect("attempt");
        assert_ne!(attempt.identity, "adoption-request");
        assert!(attempt.base_fingerprint.starts_with("sha256:"));
        let adoption = attempt.adoption.expect("adoption provenance");
        assert_eq!(adoption.source_attempt, "attempt-provider-a");
        assert_eq!(adoption.patch_fingerprint, expected_fingerprint);
        assert_eq!(adoption.provider_backend, "provider-a");
        assert_eq!(adoption.provider_model.as_deref(), Some("model-a"));
    }

    #[test]
    fn primary_success_does_not_rotate() {
        let executor = RotationScriptedExecutor::new(vec![success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation")
            .is_none());
    }

    #[test]
    fn rotates_on_transient_and_timeout_classifications() {
        for classification in [
            AgentTaskFailureClassification::Transient,
            AgentTaskFailureClassification::Timeout,
            AgentTaskFailureClassification::Stalled,
            AgentTaskFailureClassification::RateLimited,
        ] {
            let status = if classification == AgentTaskFailureClassification::Timeout {
                AgentTaskOutcomeStatus::Timeout
            } else {
                AgentTaskOutcomeStatus::ProviderError
            };
            let executor =
                RotationScriptedExecutor::new(vec![(status, Some(classification)), success()]);
            let calls = Arc::clone(&executor.calls);
            let scheduler = AgentTaskScheduler::new(executor);
            let mut plan = plan_with_tasks(1);
            plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
            enable_rotation(&mut plan);

            let aggregate = scheduler.run(plan);

            assert_eq!(
                aggregate.status,
                AgentTaskAggregateStatus::Succeeded,
                "classification {classification:?} should rotate"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn does_not_rotate_on_task_level_failure_classifications() {
        for classification in [
            AgentTaskFailureClassification::ExecutionFailed,
            AgentTaskFailureClassification::PolicyDenied,
            AgentTaskFailureClassification::InvalidInput,
            AgentTaskFailureClassification::CapabilityMissing,
        ] {
            let executor = RotationScriptedExecutor::new(vec![
                (AgentTaskOutcomeStatus::Failed, Some(classification)),
                success(),
            ]);
            let calls = Arc::clone(&executor.calls);
            let scheduler = AgentTaskScheduler::new(executor);
            let mut plan = plan_with_tasks(1);
            plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
            enable_rotation(&mut plan);

            let aggregate = scheduler.run(plan);

            assert_eq!(
                aggregate.status,
                AgentTaskAggregateStatus::Failed,
                "classification {classification:?} must not rotate"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "classification {classification:?} must not re-dispatch"
            );
            assert_eq!(
                aggregate.outcomes[0].failure_classification,
                Some(classification)
            );
            assert!(aggregate.outcomes[0]
                .metadata
                .pointer("/provider_rotation")
                .is_none());
        }
    }

    #[test]
    fn rotation_exhausts_entries_and_records_attempt_sequence_in_order() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![
            entry("fallback-backend-a"),
            entry("fallback-backend-b"),
        ]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed.len(), 3);
        assert_eq!(observed[1].executor.backend, "fallback-backend-a");
        assert_eq!(observed[2].executor.backend, "fallback-backend-b");
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts
                .iter()
                .map(|attempt| attempt["rotation_index"].as_u64().expect("rotation index"))
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(attempts
            .iter()
            .all(|attempt| attempt["failure_classification"] == "provider"));
    }

    #[test]
    fn rotation_respects_configured_max_attempts_bound() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                entry("fallback-backend-a"),
                entry("fallback-backend-b"),
                entry("fallback-backend-c"),
            ],
            max_attempts: Some(2),
            ..AgentTaskProviderRotationPolicy::default()
        });
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 2);
    }

    #[test]
    fn request_metadata_rotation_overrides_plan_policy() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("plan-fallback")]));
        enable_rotation(&mut plan);
        plan.tasks[0].metadata = json!({
            "provider_rotation": {
                "entries": [{ "backend": "request-fallback" }]
            }
        });

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed[1].executor.backend, "request-fallback");
    }

    #[test]
    fn no_rotation_policy_keeps_single_attempt_behavior_unchanged() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let plan = plan_with_tasks(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation")
            .is_none());
        assert!(!aggregate.events.iter().any(|event| event
            .message
            .as_deref()
            .is_some_and(|message| { message.contains("provider rotation") })));
    }
}

mod timeout_tests {
    use super::*;

    #[test]
    fn normalizes_slow_task_to_timeout() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn timeout_with_completed_runtime_artifacts_is_discoverable_and_promotable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("fix.patch");
        fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        let agent_result_path = artifact_root.join("agent-result.json");
        fs::write(
            &agent_result_path,
            serde_json::to_string(&AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("patch ready".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "fix".to_string(),
                    kind: "patch".to_string(),
                    name: Some("fix.patch".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: json!({ "role": "patch" }),
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "runtime_bundle".to_string(),
                    uri: artifact_root.display().to_string(),
                    label: Some("runtime bundle".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: json!({}),
            })
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::CandidateRecoverable
        );
        assert_eq!(aggregate.totals.candidate_recoverable, 1);
        assert_eq!(aggregate.totals.timed_out, 0);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1"
                && event.state == AgentTaskState::CandidateRecoverable));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "transcript"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "agent_result"
                && evidence.uri == agent_result_path.display().to_string()));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "scheduler_timeout"
                && diagnostic
                    .data
                    .get("candidate_recoverable")
                    .and_then(Value::as_bool)
                    == Some(true)
        }));
    }

    #[test]
    fn timeout_with_empty_patch_artifacts_and_actionable_false_stays_timed_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("patch.diff");
        let mounted_patch_path = artifact_root.join("mount-5.patch");
        fs::write(&patch_path, "").expect("patch diff");
        fs::write(&mounted_patch_path, "").expect("mounted patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        fs::write(
            artifact_root.join("agent-result.json"),
            serde_json::to_string(&json!({
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": "task-1",
                "status": "succeeded",
                "summary": "runtime produced no actionable patch",
                "actionable": false,
                "artifacts": [
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "patch",
                        "kind": "patch",
                        "name": "patch.diff",
                        "path": patch_path.display().to_string(),
                        "mime": "text/x-diff",
                        "metadata": { "role": "patch" }
                    },
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "mount-5",
                        "kind": "patch",
                        "name": "mount-5.patch",
                        "path": mounted_patch_path.display().to_string(),
                        "mime": "text/x-patch",
                        "metadata": { "role": "patch" }
                    }
                ],
                "evidence_refs": [],
                "diagnostics": [],
                "metadata": {}
            }))
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.succeeded, 0);
        assert_eq!(aggregate.totals.timed_out, 1);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::TimedOut));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "completed_runtime_late_provider_race"
                && diagnostic
                    .data
                    .get("actionable_patch")
                    .and_then(Value::as_bool)
                    == Some(false)
        }));
    }
}

mod artifact_binding_tests {
    use super::*;

    #[test]
    fn runtime_bundle_artifacts_materialize_required_typed_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("runtime-123");
        let files = bundle.join("files");
        fs::create_dir_all(&files).expect("runtime files");
        let patch_path = files.join("patch.diff");
        let transcript_path = files.join("transcript.json");
        fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
        fs::write(&transcript_path, "{\"events\":[]}").expect("transcript");

        let scheduler = AgentTaskScheduler::new(RuntimeBundleOutcomeExecutor {
            patch_path: patch_path.clone(),
            transcript_path: transcript_path.clone(),
        });
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].expected_artifacts = vec![
            "patch".to_string(),
            "agent_result".to_string(),
            "transcript".to_string(),
        ];

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 1);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        for name in ["patch", "agent_result", "transcript"] {
            assert!(
                outcome
                    .typed_artifacts
                    .iter()
                    .any(|artifact| artifact.name == name),
                "missing typed artifact {name}"
            );
        }
        let patch = outcome
            .typed_artifacts
            .iter()
            .find(|artifact| artifact.name == "patch")
            .expect("patch typed artifact");
        assert_eq!(
            patch
                .artifact
                .as_ref()
                .and_then(|artifact| artifact.path.as_deref()),
            Some(patch_path.to_str().expect("patch path"))
        );
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.required_typed_artifacts_normalized"
        }));
    }

    #[test]
    fn templates_prior_output_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-dag", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.tasks[1].executor.config = json!({
            "github_issue": "{{outputs.issue_number}}",
            "instructions": "Use issue {{ outputs.issue_number }}"
        });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(design.instructions, "Build design for issue #3447");
        assert_eq!(design.executor.config["github_issue"], json!(3447));
        assert_eq!(design.executor.config["instructions"], "Use issue 3447");
        assert_eq!(design.metadata["generated_from_outputs"], json!(true));
        assert_eq!(
            design.metadata["resolved_output_bindings"]["issue_number"],
            json!(3447)
        );
        let idea_succeeded_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "idea" && event.state == AgentTaskState::Succeeded)
            .expect("idea succeeded event");
        let design_running_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "design" && event.state == AgentTaskState::Running)
            .expect("design running event");
        assert!(idea_succeeded_index < design_running_index);
    }

    #[test]
    fn binds_typed_artifact_payload_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-dag",
            vec![request("idea"), request("design")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.concept_packet}}" });
        plan.artifact_outputs.insert(
            "idea".to_string(),
            vec![AgentTaskArtifactOutputDeclaration {
                name: "concept_packet".to_string(),
                kind: "concept_packet".to_string(),
                schema: Some("example/concept-packet/v1".to_string()),
                artifact_id: None,
                payload_path: Some("/title".to_string()),
            }],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("example/concept-packet/v1".to_string()),
                            artifact_id: Some("concept".to_string()),
                            payload_path: Some("/title".to_string()),
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(design.inputs["packet"], json!("Demo concept"));
        assert_eq!(aggregate.artifact_lineage.len(), 1);
        assert_eq!(aggregate.artifact_lineage[0].name, "concept_packet");
        assert_eq!(aggregate.artifact_lineage[0].payload, json!("Demo concept"));
    }

    #[test]
    fn required_concept_packet_binding_uses_canonical_typed_artifact() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(ConceptPacketExecutor {
            observed: Arc::clone(&observed),
            emit_concept_packet: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-concept-packet-typed-artifact",
            vec![request("idea"), request("build")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[0].artifact_declarations = vec![concept_packet_declaration()];
        plan.tasks[1].inputs = json!({ "concept_packet": "{{outputs.concept_packet}}" });
        plan.output_dependencies.insert(
            "build".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let build = observed
            .iter()
            .find(|request| request.task_id == "build")
            .expect("build request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(build.inputs["concept_packet"]["title"], "Typed concept");
    }

    #[test]
    fn required_concept_packet_binding_fails_without_canonical_typed_artifact() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(ConceptPacketExecutor {
            observed: Arc::clone(&observed),
            emit_concept_packet: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-concept-packet-missing-typed-artifact",
            vec![request("idea"), request("build")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[0].artifact_declarations = vec![concept_packet_declaration()];
        plan.output_dependencies.insert(
            "build".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert!(observed.iter().all(|request| request.task_id != "build"));
        let idea = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "idea")
            .expect("idea outcome");
        assert!(idea.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.required_typed_artifacts_missing"
                && diagnostic.message.contains("concept_packet")
        }));
        let build = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "build")
            .expect("build skipped outcome");
        assert!(build.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required artifact binding")
                && diagnostic.message.contains("concept_packet")
        }));
    }

    #[test]
    fn binds_artifacts_to_generic_child_run_ids_for_durable_fanout() {
        let scheduler = AgentTaskScheduler::new(GenericChildRunExecutor);
        let mut plan = AgentTaskPlan::new(
            "fuzz/campaign-1",
            vec![request("case-a"), request("case-b")],
        );
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.child_runs.len(), 2);
        let mut child_runs = aggregate.child_runs.clone();
        child_runs.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        assert_eq!(child_runs[0].task_id, "case-a");
        assert_eq!(child_runs[0].run_id, "child-case-a");
        assert_eq!(child_runs[0].provider.as_deref(), Some("generic-fuzz"));
        assert_eq!(child_runs[0].state, AgentTaskState::Succeeded);
        assert_eq!(aggregate.artifact_bindings.len(), 2);
        let mut artifact_bindings = aggregate.artifact_bindings.clone();
        artifact_bindings.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        assert_eq!(artifact_bindings[0].task_id, "case-a");
        assert_eq!(artifact_bindings[0].run_id, "child-case-a");
        assert_eq!(artifact_bindings[0].artifact_id, "artifact-case-a");
        assert_eq!(artifact_bindings[0].kind, "fuzz-report");
        assert_eq!(
            artifact_bindings[0].path.as_deref(),
            Some("artifacts/case-a/report.json")
        );
    }

    #[test]
    fn skips_required_typed_artifact_binding_when_artifact_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-skip",
            vec![request("idea"), request("design")],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert!(observed.iter().all(|request| request.task_id != "design"));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required artifact binding")
        }));
    }

    #[test]
    fn optional_typed_artifact_binding_uses_default() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-default",
            vec![request("idea"), request("design")],
        );
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.finding_packet}}" });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: false,
                        default: json!({ "findings": [] }),
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(design.inputs["packet"], json!({ "findings": [] }));
    }

    #[test]
    fn skips_downstream_task_when_required_output_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-skip", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(aggregate.totals.skipped, 1);
        assert_eq!(aggregate.totals.failed, 0);
        assert!(observed.iter().all(|request| request.task_id != "design"));
        assert!(aggregate
            .events
            .iter()
            .any(|event| { event.task_id == "design" && event.state == AgentTaskState::Skipped }));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert_eq!(skipped.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            skipped.failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required output binding")
        }));
    }
}

mod retry_failure_tests {
    use super::*;

    #[test]
    fn preserves_partial_failure_evidence() {
        let mut statuses = HashMap::new();
        statuses.insert("task-2".to_string(), AgentTaskOutcomeStatus::Failed);
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
        );
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.failed, 1);
        let failed = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "task-2")
            .expect("failed task outcome");
        assert_eq!(failed.evidence_refs[0].kind, "log");
    }

    #[test]
    fn failed_single_task_is_not_also_counted_as_queued() {
        let mut statuses = HashMap::new();
        statuses.insert("task-1".to_string(), AgentTaskOutcomeStatus::Failed);
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));

        let aggregate = scheduler.run(plan_with_tasks(1));

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.queue.queued, 0);
    }

    #[test]
    fn retries_failed_tasks_until_success() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-1" && event.state == AgentTaskState::Queued && event.attempt == 2
        }));
    }
}

mod cancellation_tests {
    use super::*;

    #[test]
    fn cancellation_token_callbacks_fire_once_and_after_existing_cancel() {
        let token = AgentTaskCancellationToken::default();
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_token = Arc::clone(&callback_count);
        token.on_cancel(Arc::new(move || {
            callback_count_for_token.fetch_add(1, Ordering::SeqCst);
        }));

        token.cancel();
        token.cancel();

        assert_eq!(callback_count.load(Ordering::SeqCst), 1);

        let immediate_count = Arc::new(AtomicUsize::new(0));
        let immediate_count_for_token = Arc::clone(&immediate_count);
        token.on_cancel(Arc::new(move || {
            immediate_count_for_token.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(immediate_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_stops_queued_tasks_and_notifies_running_executor() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(100));
        let cancel_calls = Arc::clone(&executor.cancel_calls);
        let running = Arc::clone(&executor.running);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 1;
        let token = AgentTaskCancellationToken::default();
        let worker_token = token.clone();

        let handle = thread::spawn(move || scheduler.run_with_cancellation(plan, worker_token));
        while running.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        token.cancel();
        let aggregate = handle.join().expect("scheduler thread");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Cancelled);
        assert!(aggregate.totals.cancelled >= 2);
        assert!(cancel_calls
            .lock()
            .expect("cancel calls")
            .contains(&"task-1".to_string()));
    }
}

mod plan_projection_tests {
    use super::*;

    #[test]
    fn runs_matrix_cells_through_generic_scheduler_and_preserves_axes() {
        let mut statuses = HashMap::new();
        statuses.insert(
            "fanout/site-smoke[model=gpt-5.5,prompt=site-b]".to_string(),
            AgentTaskOutcomeStatus::Failed,
        );
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
        let matrix_plan = expand_agent_task_matrix(
            "fanout/site-smoke",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt-5.5".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site-a".to_string(), "site-b".to_string()],
                },
            ],
            request("template"),
        )
        .expect("matrix expands");
        let mut schedule_plan = AgentTaskPlan::new(
            matrix_plan.plan_id.clone(),
            matrix_plan
                .cells
                .iter()
                .map(|cell| cell.task.clone())
                .collect(),
        );
        schedule_plan.options.max_concurrency = 2;

        let schedule = scheduler.run(schedule_plan);
        let matrix = AgentTaskMatrixAggregate::from_outcomes(&matrix_plan, &schedule.outcomes);

        assert_eq!(schedule.plan_id, "fanout/site-smoke");
        assert_eq!(schedule.totals.succeeded, 3);
        assert_eq!(schedule.totals.failed, 1);
        assert_eq!(matrix.cells.len(), 4);
        assert!(!matrix.passed);
        let failed = matrix
            .cells
            .iter()
            .find(|cell| cell.status == Some(AgentTaskOutcomeStatus::Failed))
            .expect("failed matrix cell");
        assert_eq!(failed.axes["model"], "gpt-5.5");
        assert_eq!(failed.axes["prompt"], "site-b");
        assert_eq!(failed.evidence_refs[0].kind, "log");
    }

    #[test]
    fn static_batch_plans_remain_compatible_without_output_dependencies() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let plan_json = serde_json::to_string(&plan_with_tasks(2)).expect("plan json");
        let plan: AgentTaskPlan = serde_json::from_str(&plan_json).expect("static plan decodes");

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.skipped, 0);
        assert_eq!(aggregate.queue.max_concurrency, 1);
        assert!(aggregate.queue.adaptive_concurrency.is_none());
    }

    #[test]
    fn plan_level_component_contracts_are_preserved_on_executor_requests() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let raw = serde_json::json!({
            "schema": AGENT_TASK_PLAN_SCHEMA,
            "plan_id": "plan-components",
            "component_contracts": [{
                "slug": "generic-component",
                "path": "/workspace/generic-component",
                "loadAs": "plugin",
                "activate": true,
                "opaque_executor_hint": { "preserve": true }
            }],
            "tasks": [{
                "task_id": "task-components",
                "executor": { "backend": "test" },
                "instructions": "run"
            }]
        });
        let plan: AgentTaskPlan = serde_json::from_value(raw).expect("plan parses");

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let request = observed.first().expect("request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(request.component_contracts.len(), 1);
        assert_eq!(
            request.component_contracts[0].slug.as_deref(),
            Some("generic-component")
        );
        assert_eq!(
            request.component_contracts[0].path.as_deref(),
            Some("/workspace/generic-component")
        );
        assert_eq!(request.component_contracts[0].extra["loadAs"], "plugin");
        assert_eq!(request.component_contracts[0].extra["activate"], true);
        assert_eq!(
            request.component_contracts[0].extra["opaque_executor_hint"]["preserve"],
            true
        );
    }

    #[test]
    fn legacy_agent_task_plan_json_round_trips_through_homeboy_plan_projection() {
        let mut plan =
            AgentTaskPlan::new("plan-projection", vec![request("idea"), request("design")]);
        plan.group_key = Some("group-a".to_string());
        plan.options.max_concurrency = 2;
        plan.metadata = json!({ "source": "compat" });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: vec!["idea".to_string()],
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );
        plan.artifact_outputs.insert(
            "idea".to_string(),
            vec![AgentTaskArtifactOutputDeclaration {
                name: "concept_packet".to_string(),
                kind: "concept_packet".to_string(),
                schema: Some("example/concept-packet/v1".to_string()),
                artifact_id: Some("concept".to_string()),
                payload_path: Some("/title".to_string()),
            }],
        );

        let raw = serde_json::to_string(&plan).expect("serialize legacy contract");
        let value: Value = serde_json::from_str(&raw).expect("serialized json");
        assert_eq!(value["schema"], AGENT_TASK_PLAN_SCHEMA);
        assert!(value.get("homeboy_plan").is_none());

        let decoded: AgentTaskPlan = serde_json::from_str(&raw).expect("legacy plan decodes");
        let projected = AgentTaskPlan::from_homeboy_plan(decoded.homeboy_plan.clone());

        assert_eq!(
            decoded.homeboy_plan.kind,
            crate::core::plan::PlanKind::AgentTask
        );
        assert_eq!(projected.schema, AGENT_TASK_PLAN_SCHEMA);
        assert_eq!(projected.plan_id, plan.plan_id);
        assert_eq!(projected.group_key, plan.group_key);
        assert_eq!(projected.tasks, plan.tasks);
        assert_eq!(projected.output_dependencies, plan.output_dependencies);
        assert_eq!(projected.artifact_outputs, plan.artifact_outputs);
        assert_eq!(projected.options, plan.options);
        assert_eq!(projected.metadata, plan.metadata);
    }

    #[test]
    fn scheduler_executes_from_projected_homeboy_plan() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-homeboy-projection",
            vec![request("idea"), request("design")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: vec!["idea".to_string()],
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );
        plan.rebuild_homeboy_plan();
        let projected = AgentTaskPlan::from_homeboy_plan(plan.homeboy_plan.clone());

        let aggregate = scheduler.run(projected);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(design.instructions, "Build design for issue #3447");
    }
}

fn concept_packet_declaration() -> AgentTaskArtifactDeclaration {
    AgentTaskArtifactDeclaration {
        name: "concept_packet".to_string(),
        artifact_type: Some("concept_packet".to_string()),
        artifact_schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
        path: None,
        required: true,
        description: None,
        metadata: Value::Null,
    }
}

struct ConceptPacketExecutor {
    observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
    emit_concept_packet: bool,
}

impl AgentTaskExecutorAdapter for ConceptPacketExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        self.observed
            .lock()
            .expect("observed requests")
            .push(request.clone());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: if self.emit_concept_packet {
                vec![AgentTaskTypedArtifact {
                    name: "concept_packet".to_string(),
                    artifact_type: Some("concept_packet".to_string()),
                    artifact_schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                    payload: json!({ "title": "Typed concept" }),
                    artifact: None,
                    metadata: json!({ "source": "sample-runtime/artifact-result-envelope/v1" }),
                }]
            } else {
                Vec::new()
            },
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct GenericChildRunExecutor;

impl AgentTaskExecutorAdapter for GenericChildRunExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("generic fuzz case completed".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: format!("artifact-{}", request.task_id),
                kind: "fuzz-report".to_string(),
                name: Some("report.json".to_string()),
                label: Some("Fuzz report".to_string()),
                role: Some("fuzz_report".to_string()),
                semantic_key: Some("fuzz.report".to_string()),
                path: Some(format!("artifacts/{}/report.json", request.task_id)),
                url: None,
                mime: Some("application/json".to_string()),
                size_bytes: Some(512),
                sha256: Some(format!("sha256:{}", request.task_id)),
                metadata: json!({ "case_id": request.task_id }),
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({ "case_id": request.task_id }),
            workflow: None,
            follow_up: None,
            metadata: json!({
                "provider": "generic-fuzz",
                "child_run_id": format!("child-{}", request.task_id)
            }),
        }
    }
}
