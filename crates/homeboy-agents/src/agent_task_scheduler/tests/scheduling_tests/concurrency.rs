//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

pub(super) mod concurrency_tests {
    use super::*;

    pub(crate) fn init_git_workspace(path: &std::path::Path) {
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.options
            .per_executor_concurrency
            .insert("test".to_string(), 1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
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
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        for task in &mut plan.tasks {
            task.limits.exclusive_resource_keys = vec!["cache:shared".to_string()];
        }

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
        assert!(aggregate
            .outcomes
            .iter()
            .all(|outcome| outcome.status == AgentTaskOutcomeStatus::Succeeded));
        assert_eq!(
            aggregate
                .events
                .iter()
                .filter(|event| event.state == AgentTaskState::Running)
                .map(|event| event.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["task-1", "task-2"]
        );
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
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
        let scheduler = crate::agent_task_scheduler::AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[1].workspace.root = Some(workspace.join(".").display().to_string());
        plan.tasks[2].workspace.root = Some(child.display().to_string());

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 3);
        assert_eq!(aggregate.queue.max_concurrency, 3);
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
        let serialized = aggregate
            .events
            .iter()
            .filter(|event| event.state == AgentTaskState::Running)
            .map(|event| event.task_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            serialized.len(),
            3,
            "all overlapping tasks must be admitted through the scheduler"
        );
        assert_eq!(serialized[0], "task-1");
        assert_eq!(serialized, vec!["task-1", "task-2", "task-3"]);
    }

    #[derive(Clone)]
    struct LateMutatingExecutor {
        events: Arc<Mutex<Vec<String>>>,
        finished: Arc<AtomicBool>,
        attempt_roots: Arc<Mutex<Vec<std::path::PathBuf>>>,
        scratch_roots: Arc<Mutex<Vec<std::path::PathBuf>>>,
        scratch_active_while_running: Arc<AtomicBool>,
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
                let scratch_root = std::path::PathBuf::from(
                    request.executor.config["runtime_env"]["TMPDIR"]
                        .as_str()
                        .expect("scheduler scratch root"),
                );
                self.scratch_roots
                    .lock()
                    .expect("scratch roots")
                    .push(scratch_root.clone());
                std::thread::sleep(Duration::from_millis(3_000));
                let run_id = scratch_root
                    .ancestors()
                    .nth(4)
                    .and_then(std::path::Path::file_name)
                    .expect("scheduler scratch run id");
                let scratch_index = scratch_root
                    .ancestors()
                    .nth(6)
                    .expect("controller scratch root")
                    .join("test-indexes")
                    .join(run_id)
                    .join("resources.json");
                let active = serde_json::from_str::<Value>(
                    &fs::read_to_string(scratch_index).expect("scratch index"),
                )
                .expect("scratch index JSON")["resources"]
                    .as_array()
                    .expect("scratch resources")
                    .iter()
                    .any(|resource| {
                        resource["path"] == scratch_root.display().to_string()
                            && resource["lifecycle_state"] == "active"
                    });
                self.scratch_active_while_running
                    .store(active, Ordering::SeqCst);
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
        let scratch_roots = Arc::new(Mutex::new(Vec::new()));
        let scratch_active_while_running = Arc::new(AtomicBool::new(false));
        let scheduler = AgentTaskScheduler::new(LateMutatingExecutor {
            events: Arc::clone(&events),
            finished: Arc::clone(&finished),
            attempt_roots: Arc::clone(&attempt_roots),
            scratch_roots: Arc::clone(&scratch_roots),
            scratch_active_while_running: Arc::clone(&scratch_active_while_running),
        });
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[1].workspace.root = Some(workspace.display().to_string());
        plan.tasks[2].workspace.root = Some(unrelated.display().to_string());

        let started = Instant::now();
        let aggregate = scheduler.run(plan);
        assert!(started.elapsed() < Duration::from_secs(2));
        while !finished.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let scratch_root = scratch_roots.lock().expect("scratch roots")[0].clone();
        let run_id = scratch_root
            .ancestors()
            .nth(4)
            .and_then(std::path::Path::file_name)
            .expect("scheduler scratch run id");
        let scratch_index = scratch_root
            .ancestors()
            .nth(6)
            .expect("controller scratch root")
            .join("test-indexes")
            .join(run_id)
            .join("resources.json");
        assert!(scratch_active_while_running.load(Ordering::SeqCst));
        let cleanup_action = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "task-1")
            .and_then(|outcome| {
                outcome
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.kind == "cleanup_action")
            })
            .and_then(|artifact| artifact.path.as_deref())
            .expect("deferred cleanup action path");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let action: Value = serde_json::from_str(
                &fs::read_to_string(cleanup_action).expect("deferred cleanup action"),
            )
            .expect("deferred cleanup JSON");
            if action["status"] == "candidate_recovered" {
                let candidates = action["candidate_artifacts"]
                    .as_array()
                    .expect("recovered candidates");
                assert!(candidates.iter().any(|artifact| {
                    artifact["kind"] == "patch"
                        && artifact["sha256"]
                            .as_str()
                            .is_some_and(|sha| sha.len() == 64)
                }));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "deferred cleanup did not recover"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let scratch: Value = serde_json::from_str::<Value>(
            &fs::read_to_string(scratch_index).expect("scratch index"),
        )
        .expect("scratch index JSON")["resources"]
            .as_array()
            .expect("scratch resources")
            .iter()
            .find(|resource| resource["path"] == scratch_root.display().to_string())
            .cloned()
            .expect("released scratch resource");
        assert_eq!(scratch["terminal_reason"], "scheduler_timeout_completion");
        assert!(scratch["terminal_evidence"]["outcome"]["artifacts"]
            .as_array()
            .is_some_and(|artifacts| !artifacts.is_empty()));
        let events = events.lock().expect("events");

        assert!(events.iter().any(|event| event == "task-3-started"));
        assert!(!events.iter().any(|event| event == "task-2-started"));
        assert!(aggregate
            .outcomes
            .iter()
            .any(|outcome| outcome.task_id == "task-1"
                && outcome.status == AgentTaskOutcomeStatus::Timeout
                && outcome
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.kind == "cleanup_action")));
        assert!(aggregate.outcomes.iter().any(|outcome| {
            outcome.task_id == "task-2" && outcome.status == AgentTaskOutcomeStatus::Failed
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
        // Attempt worktrees are now retained when they hold work (#8579), so
        // isolate the controller-scratch home to keep those retained checkouts
        // out of the developer's real `~/.local/share/homeboy`.
        let _home = homeboy_core::test_support::HomeGuard::new();
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
        plan.options.execution_budget.max_provider_executions = 2;
        plan.options.execution_budget.max_same_provider_retries = 1;
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Transient];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let workspaces = workspaces.lock().expect("workspaces");
        assert_eq!(workspaces.len(), 2);
        assert_ne!(workspaces[0], workspaces[1]);
        assert!(workspaces.iter().all(|workspace| workspace != &source));
        // Attempt worktrees are intentionally retained for lifecycle cleanup when
        // they still hold work (#8579): attempt 1 left uncommitted changes and
        // attempt 2 committed beyond its base. The work itself is preserved as a
        // promoted patch artifact (asserted below), so scheduler cleanup does not
        // force-remove the checkouts and records a retention diagnostic instead.
        assert!(aggregate.outcomes[0]
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.class == "agent_task.attempt_workspace_retained" }));
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
