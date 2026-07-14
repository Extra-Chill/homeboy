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
