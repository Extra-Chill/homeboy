//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::super::fixtures::*;
use super::super::*;
use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskArtifactDeclaration,
    AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

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
