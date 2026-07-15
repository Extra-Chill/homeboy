//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

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
        plan.options.execution_budget.max_provider_executions = 2;
        plan.options.execution_budget.max_same_provider_retries = 1;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-1" && event.state == AgentTaskState::Queued && event.attempt == 2
        }));
    }

    #[test]
    fn default_unbounded_budget_does_not_retry_without_a_retry_policy() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let plan = AgentTaskPlan::new("default-no-retry", vec![request("task-1")]);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn legacy_retry_policy_retries_with_the_legacy_unbounded_budget() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = AgentTaskPlan::new("legacy-retry", vec![request("task-1")]);
        plan.options.retry.max_attempts = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn lower_retry_cap_wins_over_execution_budget() {
        let executor = RetryTwiceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 2;
        plan.options.execution_budget.max_provider_executions = 3;
        plan.options.execution_budget.max_same_provider_retries = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn lower_execution_budget_wins_over_retry_cap() {
        let executor = RetryTwiceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 3;
        plan.options.execution_budget.max_provider_executions = 2;
        plan.options.execution_budget.max_same_provider_retries = 1;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
