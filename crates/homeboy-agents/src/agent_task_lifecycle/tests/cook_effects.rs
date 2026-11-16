use super::*;

#[test]
fn renewing_cook_owner_blocks_until_expiry_and_replays_completion() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run = "renewing-cook-effect";
    store
        .submit_plan_with_runtime_admission(&test_plan(), run, |_| Ok(json!({})))
        .unwrap();
    let lease = std::time::Duration::from_millis(300);
    let ClaimOutcome::Acquired(owner) =
        claim_cook_operation_in_store(&store, run, "promote", lease).unwrap()
    else {
        panic!("first owner");
    };
    std::thread::sleep(lease * 3);
    assert!(matches!(
        claim_cook_operation_in_store(&store, run, "promote", lease).unwrap(),
        ClaimOutcome::LeaseHeld
    ));
    drop(owner);
    std::thread::sleep(lease * 2);
    let ClaimOutcome::Acquired(successor) =
        claim_cook_operation_in_store(&store, run, "promote", lease).unwrap()
    else {
        panic!("expired owner must be replaced");
    };
    let result = json!({"sha": "content-addressed-result"});
    successor.complete(result.clone()).unwrap();
    drop(successor);
    assert!(
        matches!(claim_cook_operation_in_store(&store, run, "promote", lease).unwrap(), ClaimOutcome::AlreadyCompleted(value) if value == result)
    );
    let recorded = store.operation_claim(run, "promote").unwrap().unwrap();
    assert_eq!(recorded.state, ClaimState::Completed);
    assert_eq!(recorded.result, Some(result));
}
