//! An aggregate's status and totals are one function of its outcomes, whichever
//! path wrote it (#15718).

use super::*;
use crate::agent_task::{AgentTaskArtifact, AgentTaskCandidate, AgentTaskOutcomeStatus};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskExecutionContext,
    AgentTaskExecutorAdapter, AgentTaskScheduler,
};
use homeboy_core::test_support::with_isolated_home;
use std::fs;
use std::sync::Arc;

/// `task-a` succeeds; `task-b` leaves a recoverable candidate.
struct MixedCandidateExecutor {
    candidate: AgentTaskCandidate,
}

impl AgentTaskExecutorAdapter for MixedCandidateExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let recoverable = request.task_id == "task-b";
        AgentTaskOutcome {
            task_id: request.task_id,
            status: if recoverable {
                AgentTaskOutcomeStatus::CandidateRecoverable
            } else {
                AgentTaskOutcomeStatus::Succeeded
            },
            candidate: recoverable.then(|| self.candidate.clone()),
            ..Default::default()
        }
    }
}

fn mixed_plan(candidate_root: &std::path::Path) -> AgentTaskPlan {
    let mut plan = test_plan();
    let mut task_b = plan.tasks[0].clone();
    task_b.task_id = "task-b".to_string();
    task_b.workspace.root = Some(candidate_root.display().to_string());
    plan.tasks.push(task_b);
    plan
}

fn committed_candidate(root: &std::path::Path, run_id: &str) -> AgentTaskCandidate {
    use crate::agent_task_scheduler::git_output;
    fs::create_dir_all(root).expect("candidate repo");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.test"],
    ] {
        git_output(root, &args).expect("git setup");
    }
    fs::write(root.join("candidate.txt"), "old\n").expect("base file");
    git_output(root, &["add", "."]).expect("git add");
    git_output(root, &["commit", "-qm", "base"]).expect("git commit");
    let base = git_output(root, &["rev-parse", "HEAD"]).expect("base sha");
    fs::write(root.join("candidate.txt"), "new\n").expect("candidate file");
    crate::agent_task_scheduler::candidate_from_workspace(root, &base, run_id)
        .expect("harvest candidate")
        .expect("candidate commit")
}

fn status_totals_state(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> (
    AgentTaskAggregateStatus,
    AgentTaskAggregateTotals,
    AgentTaskRunState,
) {
    let aggregate = store.read_aggregate(run_id).expect("read aggregate");
    let record = store.read_record(run_id).expect("read record");
    (aggregate.status(), aggregate.totals, record.state())
}

#[test]
fn mixed_outcomes_keep_status_and_totals_through_deferred_candidate_reconciliation() {
    with_isolated_home(|home| {
        let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        let candidate_root = home.path().join("candidate-repo");
        let candidate = committed_candidate(&candidate_root, "mixed-scheduler");
        let plan = mixed_plan(&candidate_root);

        // Scheduler path: the engine writes `[Succeeded, CandidateRecoverable]`.
        // It runs without the candidate root so it does not re-harvest it; the
        // controller plan keeps the root the reconciliation validates against.
        let mut scheduler_plan = plan.clone();
        scheduler_plan.tasks[1].workspace.root = None;
        let scheduled = AgentTaskScheduler::new(Arc::new(MixedCandidateExecutor {
            candidate: candidate.clone(),
        }))
        .run(scheduler_plan);
        let statuses = |aggregate: &AgentTaskAggregate| {
            aggregate
                .outcomes
                .iter()
                .map(|outcome| (outcome.task_id.clone(), outcome.status))
                .collect::<Vec<_>>()
        };
        let mixed = vec![
            ("task-a".to_string(), AgentTaskOutcomeStatus::Succeeded),
            (
                "task-b".to_string(),
                AgentTaskOutcomeStatus::CandidateRecoverable,
            ),
        ];
        assert_eq!(statuses(&scheduled), mixed);
        store
            .submit_plan_with_runtime_admission(&plan, "mixed-scheduler", |_| Ok(json!({})))
            .expect("submit scheduler run");
        record_run_aggregate_in_store(&store, "mixed-scheduler", &plan, &scheduled)
            .expect("record scheduler aggregate");
        let before = status_totals_state(&store, "mixed-scheduler");
        assert_eq!(before.0, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(before.1.succeeded, 1);
        assert_eq!(before.1.candidate_recoverable, 1);
        assert_eq!(before.1.recoverable_candidates, 1);
        assert_eq!(before.2, AgentTaskRunState::Succeeded);

        // Deferred-candidate path: the same scheduler aggregate, written while
        // `task-b` had timed out, is rewritten once its deferred cleanup
        // recovers the candidate.
        let run_id = "mixed-deferred";
        let descriptor = home.path().join("deferred-cleanup.json");
        fs::write(
            &descriptor,
            serde_json::to_vec(&json!({
                "schema": "homeboy/agent-task-deferred-cleanup/v1",
                "run_id": run_id,
                "task_id": "task-b",
                "attempt": 1,
                "status": "candidate_recovered",
                "candidate": candidate,
            }))
            .expect("descriptor json"),
        )
        .expect("write descriptor");
        let mut timed_out = scheduled.clone();
        let task_b = &mut timed_out.outcomes[1];
        task_b.status = AgentTaskOutcomeStatus::Timeout;
        task_b.candidate = None;
        task_b.artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "deferred-cleanup".to_string(),
            kind: "cleanup_action".to_string(),
            role: Some("cleanup_action".to_string()),
            path: Some(descriptor.display().to_string()),
            metadata: json!({ "run_id": run_id, "task_id": "task-b", "attempt": 1 }),
            ..Default::default()
        });
        timed_out.totals = AgentTaskAggregateTotals::from_outcomes(2, &timed_out.outcomes);
        store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit deferred run");
        record_run_aggregate_in_store(&store, run_id, &plan, &timed_out)
            .expect("record timed-out aggregate");

        assert!(reconcile_deferred_candidate_in_store(&store, run_id).expect("reconcile"));

        let reconciled = store.read_aggregate(run_id).expect("reconciled aggregate");
        assert_eq!(statuses(&reconciled), mixed);
        assert_eq!(status_totals_state(&store, run_id), before);
    });
}

#[test]
fn stale_persisted_status_reads_back_derived_from_outcomes() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "stale-aggregate-status";
    let plan = test_plan();
    store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submit run");
    let mut stale = serde_json::to_value(succeeded_aggregate(&plan)).expect("aggregate json");
    assert_eq!(
        stale["status"], "succeeded",
        "JSON keeps the derived status"
    );
    stale["status"] = json!("failed");
    let aggregate_path = store.aggregate_path(run_id);
    fs::create_dir_all(aggregate_path.parent().expect("run dir")).expect("run dir");
    fs::write(&aggregate_path, serde_json::to_vec(&stale).expect("bytes")).expect("write");

    let read = store.read_aggregate(run_id).expect("read aggregate");
    assert_eq!(read.status(), AgentTaskAggregateStatus::Succeeded);
    assert_eq!(
        serde_json::to_value(&read).expect("reserialize")["status"],
        "succeeded"
    );
}
