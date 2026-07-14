//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

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
