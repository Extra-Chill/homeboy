use super::*;

#[test]
fn renewing_cook_owner_blocks_until_expiry_and_replays_completion() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run = "renewing-cook-effect";
    store
        .submit_plan_with_runtime_admission(&test_plan(), run, |_| Ok(json!({})))
        .unwrap();
    // The heartbeat renews every lease/3, so each renewal has two thirds of
    // the lease as scheduling margin. A sub-second lease left ~200ms of headroom,
    // which a loaded CI runner can exceed and expire a live owner.
    let lease = std::time::Duration::from_secs(3);
    let ClaimOutcome::Acquired(owner) =
        claim_cook_operation_in_store(&store, run, "promote", lease).unwrap()
    else {
        panic!("first owner");
    };
    let initial_deadline = recorded_lease_deadline(&store, run);
    // Only renewal can keep the owner live once its initial deadline passes.
    sleep_until_past(&initial_deadline);
    assert!(matches!(
        claim_cook_operation_in_store(&store, run, "promote", lease).unwrap(),
        ClaimOutcome::LeaseHeld
    ));
    assert!(
        recorded_lease_deadline(&store, run) > initial_deadline,
        "heartbeat must extend the owner's lease"
    );
    // Dropping the owner joins its heartbeat, so the recorded deadline is
    // final; waiting past it makes expiry independent of scheduling.
    drop(owner);
    sleep_until_past(&recorded_lease_deadline(&store, run));
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
}

fn recorded_lease_deadline(store: &AgentTaskLifecycleStore, run: &str) -> String {
    store
        .operation_claim(run, "promote")
        .unwrap()
        .and_then(|claim| claim.lease_deadline)
        .expect("leased operation records its deadline")
}

fn sleep_until_past(deadline: &str) {
    let deadline = chrono::DateTime::parse_from_rfc3339(deadline)
        .expect("RFC 3339 lease deadline")
        .with_timezone(&chrono::Utc);
    loop {
        let now = chrono::Utc::now();
        if now > deadline {
            return;
        }
        let remaining = (deadline - now).to_std().unwrap_or_default();
        std::thread::sleep(remaining + std::time::Duration::from_millis(10));
    }
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
