//! The detached Cook handoff `(phase, event)` legality table (#18938).
#![cfg(test)]

use super::*;
use CookHandoffEvent::{Claim, Complete, Expire, Fail, FailClaimed, Reserve, Supervise};
use RunExecutionState::{Cancelled, Failed, Queued, Running, Succeeded};

fn root(state: RunExecutionState, handoff: Value) -> AgentTaskRunRecord {
    serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1", "run_id": "cook", "plan_id": "plan",
        "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
        "metadata": { "detached_cook_handoff": handoff },
    }))
    .expect("record")
}

/// Every legal `(phase, event)` pair, plus a sample of refused ones. The store
/// is empty: no Cook index is published and no reserved attempt record exists.
#[test]
fn cook_handoff_event_table_lists_every_legal_phase_event_pair() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let identity = homeboy_core::process::process_start_identity(std::process::id())
        .expect("read process identity")
        .expect("process identity is observable");
    let past = "2020-01-01T00:00:00Z";
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let pre_supervisor = json!({"cook_id": "cook", "admission_state": "pre_supervisor"});
    let owned = json!({"cook_id": "cook", "admission_state": "pre_supervisor",
        "launcher_id": "l1", "admission_deadline_at": past});
    let owned_live = json!({"cook_id": "cook", "admission_state": "pre_supervisor",
        "launcher_id": "l1", "admission_deadline_at": future});
    let supervising = json!({"cook_id": "cook", "admission_state": "supervising"});
    let reserved = json!({"cook_id": "cook", "admission_state": "supervising",
        "materializing_attempt_run_id": "a1"});
    let materialized = json!({"cook_id": "cook", "admission_state": "materialized",
        "attempt_run_id": "a1"});
    let failed = json!({"cook_id": "cook", "admission_state": "failed", "reason": "exited"});
    let failed_reserved = json!({"cook_id": "cook", "admission_state": "failed",
        "materializing_attempt_run_id": "a1"});
    let foreign = json!({"cook_id": "other", "admission_state": "pre_supervisor"});
    let supervise = Supervise("l1", std::process::id(), &identity, "job");

    let rows = [
        // Legal pairs.
        (Queued, &pre_supervisor, Claim("l2"), true),
        (Queued, &owned, Claim("l2"), true),
        (Failed, &failed, Claim("l2"), true),
        (Running, &owned, supervise, true),
        (Running, &owned, FailClaimed("l1", "gone"), true),
        (Running, &supervising, Reserve("a1"), true),
        (Running, &reserved, Reserve("a1"), true),
        (Succeeded, &materialized, Reserve("a2"), true),
        (Running, &supervising, Complete("a1"), true),
        (Running, &reserved, Complete("a1"), true),
        (Running, &supervising, Fail("gone"), true),
        (Running, &supervising, Expire("lapsed"), true),
        (Running, &reserved, Fail("gone"), true),
        (Running, &reserved, Expire("lapsed"), true),
        // Refused pairs.
        (Queued, &owned_live, Claim("l2"), false),
        (Running, &supervising, Claim("l2"), false),
        (Cancelled, &pre_supervisor, Claim("l2"), false),
        (Failed, &failed_reserved, Claim("l2"), false),
        (Running, &owned, Supervise("l2", 1, &identity, "job"), false),
        (Running, &supervising, supervise, false),
        (Cancelled, &owned, supervise, false),
        (Running, &owned, FailClaimed("l2", "gone"), false),
        (Running, &supervising, FailClaimed("l1", "gone"), false),
        (Running, &reserved, Reserve("a2"), false),
        (Cancelled, &supervising, Reserve("a1"), false),
        (Failed, &failed, Reserve("a1"), false),
        (Succeeded, &materialized, Complete("a1"), false),
        (Cancelled, &supervising, Complete("a1"), false),
        (Succeeded, &materialized, Fail("gone"), false),
        (Failed, &failed, Fail("gone"), false),
        (Cancelled, &supervising, Expire("lapsed"), false),
        (Queued, &foreign, Claim("l2"), false),
        (Running, &foreign, Fail("gone"), false),
    ];
    for (state, handoff, event, legal) in rows {
        let record = root(state, handoff.clone());
        assert_eq!(
            cook_handoff_event_admits(&store, "cook", &record, event),
            legal,
            "{state:?} {handoff} {event:?}"
        );
    }
}
