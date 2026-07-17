//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed
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
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(adaptive.effective_concurrency, 2);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Held
                && decision.reason.contains("configured ceiling")
        }));
    }
}
