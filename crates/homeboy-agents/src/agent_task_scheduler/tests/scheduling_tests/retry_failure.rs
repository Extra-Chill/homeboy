//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;
use homeboy_lifecycle_contract::RunExecutionState;

mod retry_failure_tests {
    use super::*;

    #[test]
    fn preserves_partial_failure_evidence() {
        let mut statuses = HashMap::new();
        statuses.insert("task-2".to_string(), AgentTaskOutcomeStatus::Failed);
        let scheduler = AgentTaskScheduler::new(Arc::new(RecordingExecutor::new(
            statuses,
            Duration::from_millis(0),
        )));
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status(),
            homeboy_lifecycle_contract::RunExecutionState::PartialFailure
        );
        // A failed task is not also counted as queued.
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.queue.queued, 0);
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
    fn retries_failed_tasks_until_success() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget.max_provider_executions = 2;
        plan.options.execution_budget.max_same_provider_retries = 1;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status(), RunExecutionState::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-1"
                && event.state == ControlPlaneState::Queued
                && event.attempt == 2
        }));
    }

    /// The default unbounded execution budget never retries on its own; an
    /// explicit retry policy does.
    #[test]
    fn default_unbounded_budget_retries_only_under_a_retry_policy() {
        for (max_attempts, expected_status, expected_attempts) in [
            (None, RunExecutionState::Failed, 1),
            (Some(2), RunExecutionState::Succeeded, 2),
        ] {
            let executor = RetryOnceExecutor::default();
            let attempts = Arc::clone(&executor.attempts);
            let scheduler = AgentTaskScheduler::new(Arc::new(executor));
            let mut plan = AgentTaskPlan::new("retry-policy", vec![request("task-1")]);
            if let Some(max_attempts) = max_attempts {
                plan.options.retry.max_attempts = max_attempts;
            }

            let aggregate = scheduler.run(plan);

            assert_eq!(aggregate.status(), expected_status, "{max_attempts:?}");
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                expected_attempts,
                "{max_attempts:?}"
            );
        }
    }

    /// Whichever of the retry cap and the execution budget is lower bounds
    /// the attempts.
    #[test]
    fn the_lower_of_retry_cap_and_execution_budget_wins() {
        for (max_attempts, max_provider_executions, max_same_provider_retries) in
            [(2, 3, 2), (3, 2, 1)]
        {
            let executor = RetryTwiceExecutor::default();
            let attempts = Arc::clone(&executor.attempts);
            let scheduler = AgentTaskScheduler::new(Arc::new(executor));
            let mut plan = plan_with_tasks(1);
            plan.options.retry.max_attempts = max_attempts;
            plan.options.execution_budget.max_provider_executions = max_provider_executions;
            plan.options.execution_budget.max_same_provider_retries = max_same_provider_retries;

            let aggregate = scheduler.run(plan);

            assert_eq!(aggregate.status(), RunExecutionState::Failed);
            assert_eq!(attempts.load(Ordering::SeqCst), 2, "{max_attempts}");
        }
    }
}
