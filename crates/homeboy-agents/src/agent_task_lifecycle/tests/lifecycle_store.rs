use std::sync::{Arc, Barrier};

use super::{succeeded_aggregate, test_plan};
use crate::agent_task_lifecycle::AgentTaskLifecycleStore;

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
