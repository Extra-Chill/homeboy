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

#[test]
fn concurrent_completion_of_a_leased_effect_is_idempotent() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let lease = std::time::Duration::from_secs(30);
    let result = json!({"sha": "content-addressed-result"});
    for round in 0..20 {
        let run = format!("concurrent-cook-effect-{round}");
        store
            .submit_plan_with_runtime_admission(&test_plan(), &run, |_| Ok(json!({})))
            .unwrap();
        let ClaimOutcome::Acquired(owner) =
            claim_cook_operation_in_store(&store, &run, "promote", lease).unwrap()
        else {
            panic!("owner");
        };
        // The owner and crash-recovery passes complete the same
        // content-addressed effect at once; every completion succeeds.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut completions = vec![{
            let (barrier, result) = (barrier.clone(), result.clone());
            std::thread::spawn(move || {
                barrier.wait();
                owner.complete(result)
            })
        }];
        for _ in 0..3 {
            let (barrier, store, run, result) =
                (barrier.clone(), store.clone(), run.clone(), result.clone());
            completions.push(std::thread::spawn(move || {
                barrier.wait();
                complete_cook_operation_in_store(&store, &run, "promote", result)
            }));
        }
        for completion in completions {
            completion.join().unwrap().unwrap();
        }
        assert!(
            matches!(claim_cook_operation_in_store(&store, &run, "promote", lease).unwrap(), ClaimOutcome::AlreadyCompleted(value) if value == result)
        );
    }
}
