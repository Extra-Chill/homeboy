//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod adaptive_concurrency_tests {
    use super::*;
    use std::sync::Condvar;

    #[derive(Default)]
    struct OverlapState {
        entered: usize,
        released: bool,
    }

    struct CoordinatedExecutor {
        state: Arc<(Mutex<OverlapState>, Condvar)>,
    }

    impl AgentTaskExecutorAdapter for CoordinatedExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let (state, changed) = &*self.state;
            let mut state = state.lock().expect("overlap state");
            state.entered += 1;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).expect("overlap release");
            }
            drop(state);
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    #[test]
    fn adaptive_concurrency_scales_up_when_runner_slots_are_available() {
        let state = Arc::new((Mutex::new(OverlapState::default()), Condvar::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(CoordinatedExecutor {
            state: Arc::clone(&state),
        }));
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 1;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            max_concurrency: Some(3),
            runner_capacity: Some(3),
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let run = thread::spawn(move || scheduler.run(plan));
        let (overlap, changed) = &*state;
        let overlap = overlap.lock().expect("overlap state");
        let (mut overlap, wait) = changed
            .wait_timeout_while(overlap, Duration::from_secs(10), |state| state.entered < 3)
            .expect("bounded overlap wait");
        let entered_before_release = overlap.entered;
        overlap.released = true;
        changed.notify_all();
        drop(overlap);

        assert!(
            !wait.timed_out(),
            "adaptive scheduler started only {entered_before_release} tasks before the bounded release"
        );
        let aggregate = run.join().expect("scheduler run");
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(
            aggregate.status,
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(entered_before_release, 3);
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
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
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
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
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
        let scheduler = AgentTaskScheduler::new(Arc::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        )));
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
