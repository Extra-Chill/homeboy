//! A record task's state is a function of its outcome when it has one (#15718).

use super::*;
use crate::agent_task::{AgentTaskArtifact, AgentTaskCandidate};
use homeboy_core::test_support::with_isolated_home;
use std::fs;

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

fn record_task_state(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
) -> AgentTaskState {
    store
        .read_record(run_id)
        .expect("read record")
        .tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .expect("record task")
        .state()
}

#[test]
fn failed_task_recovered_by_deferred_candidate_reconciliation_reads_back_recoverable() {
    with_isolated_home(|home| {
        let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        let run_id = "recovered-task-state";
        let candidate_root = home.path().join("candidate-repo");
        let candidate = committed_candidate(&candidate_root, run_id);
        let mut plan = test_plan();
        plan.tasks[0].workspace.root = Some(candidate_root.display().to_string());

        let descriptor = home.path().join("deferred-cleanup.json");
        fs::write(
            &descriptor,
            serde_json::to_vec(&json!({
                "schema": "homeboy/agent-task-deferred-cleanup/v1",
                "run_id": run_id,
                "task_id": "task-a",
                "attempt": 1,
                "status": "candidate_recovered",
                "candidate": candidate,
            }))
            .expect("descriptor json"),
        )
        .expect("write descriptor");
        let mut aggregate = succeeded_aggregate(&plan);
        let outcome = &mut aggregate.outcomes[0];
        outcome.status = AgentTaskOutcomeStatus::Timeout;
        outcome.artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "deferred-cleanup".to_string(),
            kind: "cleanup_action".to_string(),
            role: Some("cleanup_action".to_string()),
            path: Some(descriptor.display().to_string()),
            metadata: json!({ "run_id": run_id, "task_id": "task-a", "attempt": 1 }),
            ..Default::default()
        });
        aggregate.events[0].state = AgentTaskState::Failed;
        aggregate.totals = AgentTaskAggregateTotals::from_outcomes(1, &aggregate.outcomes);
        store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit run");
        record_run_aggregate_in_store(&store, run_id, &plan, &aggregate)
            .expect("record failed aggregate");

        assert!(reconcile_deferred_candidate_in_store(&store, run_id).expect("reconcile"));

        let reconciled = store.read_aggregate(run_id).expect("reconciled aggregate");
        assert_eq!(
            reconciled.outcomes[0].status,
            AgentTaskOutcomeStatus::CandidateRecoverable
        );
        assert_eq!(
            record_task_state(&store, run_id, "task-a"),
            AgentTaskState::CandidateRecoverable
        );
        assert_eq!(
            reconciled.events.last().map(|event| event.state),
            Some(AgentTaskState::CandidateRecoverable),
            "the task's last event agrees with its rewritten outcome"
        );
        assert_eq!(
            store.read_record(run_id).expect("record").state(),
            AgentTaskRunState::PartialRecoverable
        );
    });
}

#[test]
fn stale_task_event_does_not_override_terminal_outcome() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "stale-task-event";
    let mut plan = test_plan();
    let mut task_b = plan.tasks[0].clone();
    task_b.task_id = "task-b".to_string();
    plan.tasks.push(task_b);
    store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submit run");
    let mut aggregate = succeeded_aggregate(&plan);
    // The last event for `task-a` contradicts its succeeded outcome; `task-b`
    // has no outcome yet, so its event is the only state it has.
    for (task_id, state) in [
        ("task-a", AgentTaskState::Failed),
        ("task-b", AgentTaskState::Running),
    ] {
        aggregate.events.push(AgentTaskProgressEvent {
            task_id: task_id.to_string(),
            state,
            attempt: 1,
            message: None,
        });
    }
    record_run_aggregate_in_store(&store, run_id, &plan, &aggregate).expect("record aggregate");

    assert_eq!(
        record_task_state(&store, run_id, "task-a"),
        AgentTaskState::Succeeded
    );
    assert_eq!(
        record_task_state(&store, run_id, "task-b"),
        AgentTaskState::Running
    );
}
