use std::sync::{Arc, Barrier};

use super::{succeeded_aggregate, test_plan};
use crate::agent_task_lifecycle::{
    records::schemas, AgentTaskLabHandoff, AgentTaskLabHandoffAuthority, AgentTaskLabHandoffState,
    AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
};
use homeboy_core::run_lifecycle_record::{RunExecutionState, RunLifecycleRecord};
use serde_json::json;

fn record(store: &AgentTaskLifecycleStore, run_id: &str, marker: &str) -> AgentTaskRunRecord {
    let plan = test_plan();
    AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id: run_id.to_string(),
        plan_id: plan.plan_id,
        state: AgentTaskRunState::Queued,
        submitted_at: "2026-08-13T00:00:00Z".to_string(),
        updated_at: None,
        plan_path: store.controller_plan_path(run_id).display().to_string(),
        aggregate_path: None,
        totals: None,
        tasks: Vec::new(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle: RunLifecycleRecord::with_execution_state(RunExecutionState::Queued),
        lab_handoff: None,
        candidate_adoption: None,
        adoption_run_id: None,
        acceptance: None,
        workspace_identity: None,
        workspace_lifecycle_revision: 0,
        workspace_owner_lease: None,
        workspace_claim: None,
        metadata: json!({ "store_marker": marker }),
    }
}

#[test]
fn lifecycle_stores_isolate_identical_ids_and_lock_domains() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right = AgentTaskLifecycleStore::new(right_context.path_roots());
    let run_id = "same-run";
    let cook_id = "same-cook";

    let mut left_plan = test_plan();
    left_plan.plan_id = "left-plan".to_string();
    let mut right_plan = test_plan();
    right_plan.plan_id = "right-plan".to_string();
    let left_aggregate = succeeded_aggregate(&left_plan);
    let right_aggregate = succeeded_aggregate(&right_plan);

    left.write_controller_plan(run_id, &left_plan)
        .expect("write left plan");
    right
        .write_controller_plan(run_id, &right_plan)
        .expect("write right plan");
    left.write_aggregate(run_id, &left_aggregate)
        .expect("write left aggregate");
    right
        .write_aggregate(run_id, &right_aggregate)
        .expect("write right aggregate");
    left.write_cook_index_attempt(cook_id, 1, run_id, "left".to_string(), None)
        .expect("write left index");
    right
        .write_cook_index_attempt(cook_id, 2, run_id, "right".to_string(), None)
        .expect("write right index");

    assert_ne!(left.run_dir(run_id), right.run_dir(run_id));
    assert_ne!(
        left.controller_plan_path(run_id),
        right.controller_plan_path(run_id)
    );
    assert_ne!(left.aggregate_path(run_id), right.aggregate_path(run_id));
    assert_ne!(
        left.cook_index_path(cook_id),
        right.cook_index_path(cook_id)
    );
    assert_ne!(left.observation_db_path(), right.observation_db_path());
    assert_eq!(
        left.read_controller_plan(run_id).unwrap().plan_id,
        "left-plan"
    );
    assert_eq!(
        right.read_controller_plan(run_id).unwrap().plan_id,
        "right-plan"
    );
    assert_eq!(left.read_aggregate(run_id).unwrap().plan_id, "left-plan");
    assert_eq!(
        right.read_aggregate_bounded(run_id).unwrap().plan_id,
        "right-plan"
    );
    assert_eq!(left.read_cook_index(cook_id).unwrap().latest_run_id, run_id);
    assert_eq!(
        right.read_cook_index(cook_id).unwrap().attempts[0].attempt,
        2
    );
    assert!(left.cook_index_exists(cook_id));
    assert!(right.cook_index_exists(cook_id));

    left.open_observation_initialized()
        .expect("open left rooted observation DB");
    right
        .open_observation_initialized()
        .expect("open right rooted observation DB");
    left.open_observation_readonly()
        .expect("read left rooted observation DB");
    right
        .open_observation_readonly()
        .expect("read right rooted observation DB");
    assert!(left.observation_db_path().exists());
    assert!(right.observation_db_path().exists());

    // Both closures must enter together. A shared ambient lock would leave one
    // blocked before the barrier instead of proving independent root domains.
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let left_barrier = Arc::clone(&barrier);
        let left_store = left.clone();
        scope.spawn(move || {
            left_store
                .with_config_lock(|| {
                    left_barrier.wait();
                    Ok(())
                })
                .expect("left lock");
        });
        let right_barrier = Arc::clone(&barrier);
        let right_store = right.clone();
        scope.spawn(move || {
            right_store
                .with_config_lock(|| {
                    right_barrier.wait();
                    Ok(())
                })
                .expect("right lock");
        });
    });
}

#[test]
fn lifecycle_stores_isolate_same_run_record_writes_in_parallel() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right = AgentTaskLifecycleStore::new(right_context.path_roots());
    let run_id = "same-record";
    let barrier = Arc::new(Barrier::new(2));

    std::thread::scope(|scope| {
        let left_store = left.clone();
        let left_barrier = Arc::clone(&barrier);
        scope.spawn(move || {
            left_barrier.wait();
            left_store
                .write_record(&record(&left_store, run_id, "left"))
                .expect("left record");
        });
        let right_store = right.clone();
        let right_barrier = Arc::clone(&barrier);
        scope.spawn(move || {
            right_barrier.wait();
            right_store
                .write_record(&record(&right_store, run_id, "right"))
                .expect("right record");
        });
    });

    assert_eq!(
        left.read_record(run_id).unwrap().metadata["store_marker"],
        "left"
    );
    assert_eq!(
        right.read_record_bounded(run_id).unwrap().metadata["store_marker"],
        "right"
    );
    assert_ne!(left.observation_db_path(), right.observation_db_path());
}

#[test]
fn lifecycle_stores_submit_identical_run_ids_without_ambient_runtime() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right = AgentTaskLifecycleStore::new(right_context.path_roots());
    let run_id = "same-submission";
    let barrier = Arc::new(Barrier::new(2));

    let mut left_plan = test_plan();
    left_plan.plan_id = "left-submission-plan".to_string();
    let mut right_plan = test_plan();
    right_plan.plan_id = "right-submission-plan".to_string();

    std::thread::scope(|scope| {
        let left_store = left.clone();
        let left_barrier = Arc::clone(&barrier);
        scope.spawn(move || {
            left_barrier.wait();
            left_store
                .submit_plan_with_runtime_admission(&left_plan, run_id, |_| {
                    Ok(json!({ "store": "left" }))
                })
                .expect("submit left plan");
        });
        let right_store = right.clone();
        let right_barrier = Arc::clone(&barrier);
        scope.spawn(move || {
            right_barrier.wait();
            right_store
                .submit_plan_with_runtime_admission(&right_plan, run_id, |_| {
                    Ok(json!({ "store": "right" }))
                })
                .expect("submit right plan");
        });
    });

    let left_record = left.read_record(run_id).expect("read left record");
    let right_record = right.read_record(run_id).expect("read right record");
    assert_eq!(
        left.read_controller_plan(run_id).unwrap().plan_id,
        "left-submission-plan"
    );
    assert_eq!(
        right.read_controller_plan(run_id).unwrap().plan_id,
        "right-submission-plan"
    );
    assert_eq!(left_record.plan_id, "left-submission-plan");
    assert_eq!(right_record.plan_id, "right-submission-plan");
    assert_eq!(left_record.metadata["controller_runtime"]["store"], "left");
    assert_eq!(
        right_record.metadata["controller_runtime"]["store"],
        "right"
    );
    assert!(left_record.metadata.get("controller_admission").is_none());
    assert!(right_record.metadata.get("controller_admission").is_none());
}

#[test]
fn terminal_record_authority_is_written_only_below_its_lifecycle_root() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right = AgentTaskLifecycleStore::new(right_context.path_roots());
    let run_id = "same-terminal-record";
    let mut terminal = record(&left, run_id, "terminal");
    terminal.state = AgentTaskRunState::Succeeded;
    terminal.lifecycle = RunLifecycleRecord::with_execution_state(RunExecutionState::Succeeded);
    terminal.lab_handoff = Some(AgentTaskLabHandoff {
        state: AgentTaskLabHandoffState::Accepted,
        authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
        runner_id: "runner-a".to_string(),
        submission_key: None,
        payload_fingerprint: None,
        runner_job_id: Some("job-a".to_string()),
        submitted_at: None,
        acceptance_deadline_at: None,
        accepted_at: Some("2026-08-13T00:00:01Z".to_string()),
        expired_at: None,
        workspace_identity: None,
        workspace_lifecycle_revision: 0,
        workspace_owner_lease: None,
        workspace_claim: None,
    });
    terminal.metadata["remote_workspace"] = json!("/runner/workspace");

    left.write_record(&terminal).expect("terminal record");

    assert!(left_context
        .data_dir()
        .join("workspace-terminal-authority")
        .join("by-run")
        .read_dir()
        .expect("left terminal index")
        .next()
        .is_some());
    assert!(!right_context
        .data_dir()
        .join("workspace-terminal-authority")
        .exists());
    assert!(right.read_record(run_id).is_err());
}
