//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
//!
//! Durable exactly-once cook side-effect operation claims (#8357). These use
//! injected result values and never perform real Git/GitHub mutations, so the
//! primitive's crash/restart contract is deterministic (AC#7).
#![cfg(test)]

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::agent_task_lifecycle::{
    claim_cook_operation, complete_cook_operation, operation_claim, operation_lease_is_active,
    ClaimOutcome, ClaimState,
};
use homeboy_core::test_support::with_isolated_home;

const LEASE: Duration = Duration::from_secs(300);

fn seed_run(run_id: &str) {
    submit_plan(&test_plan(), Some(run_id)).expect("seed run");
}

#[test]
fn first_claim_is_acquired_and_second_pass_sees_lease_held() {
    with_isolated_home(|_| {
        seed_run("op-claim-1");
        assert_eq!(
            claim_cook_operation("op-claim-1", "promote:abc", LEASE).expect("first claim"),
            ClaimOutcome::Acquired
        );
        // A concurrent pass with a still-fresh lease must not repeat the effect.
        assert_eq!(
            claim_cook_operation("op-claim-1", "promote:abc", LEASE)
                .expect("second observes lease"),
            ClaimOutcome::LeaseHeld
        );
    });
}

#[test]
fn completed_operation_returns_immutable_result_without_release() {
    with_isolated_home(|_| {
        seed_run("op-claim-2");
        claim_cook_operation("op-claim-2", "finalize:xyz", LEASE).expect("claim");
        complete_cook_operation("op-claim-2", "finalize:xyz", json!({"pr": 42})).expect("complete");

        // A resumed controller gets the recorded result, never a fresh lease.
        match claim_cook_operation("op-claim-2", "finalize:xyz", LEASE).expect("resume") {
            ClaimOutcome::AlreadyCompleted(result) => assert_eq!(result, json!({"pr": 42})),
            other => panic!("expected AlreadyCompleted, got {other:?}"),
        }
    });
}

#[test]
fn completing_twice_keeps_the_first_result() {
    with_isolated_home(|_| {
        seed_run("op-claim-3");
        claim_cook_operation("op-claim-3", "retry:run-2", LEASE).expect("claim");
        complete_cook_operation("op-claim-3", "retry:run-2", json!({"attempt": 2}))
            .expect("first complete");
        complete_cook_operation("op-claim-3", "retry:run-2", json!({"attempt": 999}))
            .expect("second complete is a no-op");

        let claim = operation_claim("op-claim-3", "retry:run-2")
            .expect("read")
            .expect("claim present");
        assert_eq!(claim.state, ClaimState::Completed);
        assert_eq!(claim.result, Some(json!({"attempt": 2})));
    });
}

#[test]
fn expired_lease_is_reclaimable_by_a_new_pass() {
    with_isolated_home(|_| {
        seed_run("op-claim-4");
        // A zero-length lease is immediately expired, modelling a crashed
        // controller whose lease elapsed before it recorded a result.
        assert_eq!(
            claim_cook_operation("op-claim-4", "promote:def", Duration::from_secs(0))
                .expect("first claim"),
            ClaimOutcome::Acquired
        );
        // The next pass finds the expired lease and reclaims it.
        assert_eq!(
            claim_cook_operation("op-claim-4", "promote:def", LEASE).expect("reclaim"),
            ClaimOutcome::Acquired
        );
    });
}

#[test]
fn distinct_operation_keys_are_independent() {
    with_isolated_home(|_| {
        seed_run("op-claim-5");
        assert_eq!(
            claim_cook_operation("op-claim-5", "promote:a", LEASE).expect("promote"),
            ClaimOutcome::Acquired
        );
        // A different operation on the same run gets its own fresh lease.
        assert_eq!(
            claim_cook_operation("op-claim-5", "finalize:a", LEASE).expect("finalize"),
            ClaimOutcome::Acquired
        );
    });
}

#[test]
fn completing_without_a_claim_is_an_invariant_error() {
    with_isolated_home(|_| {
        seed_run("op-claim-6");
        let error = complete_cook_operation("op-claim-6", "never-claimed", json!({}))
            .expect_err("terminal without claim must error");
        assert!(error.message.contains("without its durable claim"));
    });
}

#[test]
fn active_lease_is_reported_until_completion() {
    with_isolated_home(|_| {
        seed_run("op-claim-7");
        claim_cook_operation("op-claim-7", "promote:live", LEASE).expect("claim");
        assert!(operation_lease_is_active("op-claim-7", "promote:live").expect("active"));

        complete_cook_operation("op-claim-7", "promote:live", json!({})).expect("complete");
        assert!(
            !operation_lease_is_active("op-claim-7", "promote:live").expect("inactive"),
            "a completed operation no longer holds an active lease"
        );
    });
}

#[test]
fn unknown_operation_has_no_claim_or_active_lease() {
    with_isolated_home(|_| {
        seed_run("op-claim-8");
        assert!(operation_claim("op-claim-8", "absent")
            .expect("read")
            .is_none());
        assert!(!operation_lease_is_active("op-claim-8", "absent").expect("inactive"));
    });
}
