//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod timeout_tests {
    use super::*;

    struct ReturnedTimeoutOnceExecutor {
        calls: Arc<AtomicUsize>,
        running: Arc<AtomicUsize>,
        max_running: Arc<AtomicUsize>,
    }

    impl AgentTaskExecutorAdapter for ReturnedTimeoutOnceExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(running, Ordering::SeqCst);
            self.running.fetch_sub(1, Ordering::SeqCst);
            let mut result = outcome(
                request.task_id,
                if call == 0 {
                    AgentTaskOutcomeStatus::Timeout
                } else {
                    AgentTaskOutcomeStatus::Succeeded
                },
            );
            if call == 0 {
                result.failure_classification = Some(AgentTaskFailureClassification::Timeout);
            }
            result
        }
    }

    struct ReturnedTimeoutExecutor;

    impl AgentTaskExecutorAdapter for ReturnedTimeoutExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::Timeout);
            result.failure_classification = Some(AgentTaskFailureClassification::Timeout);
            result
        }
    }

    #[test]
    fn retries_a_returned_provider_timeout_with_declared_budget() {
        let calls = Arc::new(AtomicUsize::new(0));
        let max_running = Arc::new(AtomicUsize::new(0));
        let scheduler = AgentTaskScheduler::new(Arc::new(ReturnedTimeoutOnceExecutor {
            calls: Arc::clone(&calls),
            running: Arc::new(AtomicUsize::new(0)),
            max_running: Arc::clone(&max_running),
        }));
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 2;
        plan.options.execution_budget = AgentTaskExecutionBudget::new(2, 1, 0);
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Timeout];

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(max_running.load(Ordering::SeqCst), 1);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-1" && event.state == AgentTaskState::Queued && event.attempt == 2
        }));
        let retry = aggregate.outcomes[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
            .expect("timeout retry evidence");
        assert_eq!(retry.data["status"], "timeout");
    }

    #[test]
    fn persistent_timeout_stops_after_declared_same_provider_budget() {
        let scheduler = AgentTaskScheduler::new(Arc::new(ReturnedTimeoutExecutor));
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 3;
        plan.options.execution_budget = AgentTaskExecutionBudget::new(3, 1, 0);
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Timeout];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        assert_eq!(
            aggregate
                .events
                .iter()
                .filter(|event| event.state == AgentTaskState::Running)
                .map(|event| event.attempt)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2
        );
        assert_eq!(
            aggregate.outcomes[0]
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
                .count(),
            1
        );
    }

    #[test]
    fn normalizes_slow_task_to_timeout() {
        let scheduler = AgentTaskScheduler::new(Arc::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        )));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed
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
    fn expired_execution_deadline_skips_materialization_and_provider_dispatch() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::ZERO);
        let started = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget.deadline_unix_ms =
            Some(crate::agent_task_timeout::now_unix_ms().saturating_sub(1));

        let aggregate = scheduler.run(plan);

        assert_eq!(started.load(Ordering::SeqCst), 0);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        let diagnostic = aggregate.outcomes[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.execution_deadline_exceeded")
            .expect("deadline diagnostic");
        assert_eq!(diagnostic.data["completed_phase"], "materialization");
        assert_eq!(diagnostic.data["remaining_budget_ms"], 0);
    }

    #[test]
    fn execution_deadline_is_propagated_to_the_executor_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(ConceptPacketExecutor {
            observed: Arc::clone(&observed),
            emit_concept_packet: false,
        }));
        let mut plan = plan_with_tasks(1);
        let deadline = crate::agent_task_timeout::now_unix_ms().saturating_add(60_000);
        plan.options.execution_budget.deadline_unix_ms = Some(deadline);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Succeeded
        );
        assert_eq!(
            observed.lock().expect("observed request")[0]
                .limits
                .execution_deadline_unix_ms,
            Some(deadline)
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

        let scheduler = AgentTaskScheduler::new(Arc::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        )));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::PartialRecoverable
        );
        assert_eq!(aggregate.totals.candidate_recoverable, 1);
        assert_eq!(aggregate.totals.recoverable_candidates, 1);
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
    fn timeout_after_writing_patch_reconciles_required_artifacts_and_authenticates_candidate() {
        struct PatchThenTimeout;

        impl AgentTaskExecutorAdapter for PatchThenTimeout {
            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                let artifact_root = request.metadata["artifact_root"]
                    .as_str()
                    .expect("artifact root");
                fs::write(
                    std::path::Path::new(artifact_root).join("fix.patch"),
                    "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
                )
                .expect("write patch before timeout");
                fs::write(
                    std::path::Path::new(artifact_root).join("transcript.log"),
                    "patch written before provider timeout",
                )
                .expect("write transcript before timeout");
                thread::sleep(Duration::from_millis(25));
                AgentTaskOutcome {
                    task_id: request.task_id,
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("provider completed late".to_string()),
                    ..Default::default()
                }
            }
        }

        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let artifact_root = temp.path().join("task-1-artifacts");
            fs::create_dir_all(&artifact_root).expect("artifact root");
            let mut plan = plan_with_required_artifacts(&["patch", "agent_result", "transcript"]);
            plan.tasks[0].limits.timeout_ms = Some(1);
            plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });
            crate::agent_task_lifecycle::submit_plan(&plan, Some("timeout-patch-run"))
                .expect("submit run");
            crate::agent_task_service::run_submitted(
                "timeout-patch-run".to_string(),
                Arc::new(PatchThenTimeout),
            )
            .expect("run timed-out provider");
            let aggregate = crate::agent_task_lifecycle::read_aggregate("timeout-patch-run")
                .expect("persisted aggregate");

            assert_eq!(
                aggregate.status,
                crate::agent_task_scheduler::AgentTaskAggregateStatus::PartialRecoverable
            );
            let outcome = &aggregate.outcomes[0];
            assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
            let mut typed = outcome
                .typed_artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect::<Vec<_>>();
            typed.sort_unstable();
            assert_eq!(typed, vec!["agent_result", "patch", "transcript"]);
            let patch = outcome
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "patch")
                .expect("captured patch");
            assert!(patch
                .sha256
                .as_deref()
                .is_some_and(|sha256| !sha256.is_empty()));
            assert_eq!(patch.metadata["task_id"], "task-1");
            assert_eq!(patch.metadata["run_id"], "timeout-patch-run");
            assert!(!outcome.diagnostics.iter().any(|diagnostic| {
                diagnostic.class == "agent_task.required_typed_artifacts_missing"
            }));
        });
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

        let scheduler = AgentTaskScheduler::new(Arc::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        )));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed
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
