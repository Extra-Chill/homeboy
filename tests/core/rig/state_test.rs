//! State persistence tests for `src/core/rig/state.rs`.
//!
//! The load/save path is gated on `~/.config/homeboy/` (hard to test without
//! touching real user state), so this module exercises serde round-tripping
//! which is the meaningful invariant.

use crate::rig::state::{RigState, ServiceState, SharedPathState};

#[test]
fn test_state_round_trips_empty() {
    let state = RigState::default();
    let json = serde_json::to_string(&state).expect("serialize");
    let parsed: RigState = serde_json::from_str(&json).expect("parse");
    assert!(parsed.last_up.is_none());
    assert!(parsed.services.is_empty());
    assert!(parsed.shared_paths.is_empty());
}

#[test]
fn test_state_round_trips_with_service() {
    let mut state = RigState {
        last_up: Some("2026-04-24T13:00:00Z".to_string()),
        last_check: None,
        last_check_result: None,
        services: Default::default(),
        shared_paths: Default::default(),
    };
    state.services.insert(
        "tarball".to_string(),
        ServiceState {
            pid: Some(12345),
            started_at: Some("2026-04-24T12:59:00Z".to_string()),
            status: "running".to_string(),
        },
    );
    let json = serde_json::to_string(&state).expect("serialize");
    let parsed: RigState = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed.last_up.as_deref(), Some("2026-04-24T13:00:00Z"));
    assert_eq!(
        parsed.services.get("tarball").and_then(|s| s.pid),
        Some(12345)
    );
    assert_eq!(
        parsed.services.get("tarball").map(|s| s.status.as_str()),
        Some("running")
    );
}

#[test]
fn test_state_round_trips_with_shared_path() {
    let mut state = RigState::default();
    state.shared_paths.insert(
        "/worktree/node_modules".to_string(),
        SharedPathState {
            target: "/primary/node_modules".to_string(),
            created_at: "2026-04-26T13:00:00Z".to_string(),
        },
    );

    let json = serde_json::to_string(&state).expect("serialize");
    let parsed: RigState = serde_json::from_str(&json).expect("parse");
    let entry = parsed.shared_paths.get("/worktree/node_modules").unwrap();
    assert_eq!(entry.target, "/primary/node_modules");
    assert_eq!(entry.created_at, "2026-04-26T13:00:00Z");
}

#[test]
fn test_state_tolerates_missing_optional_fields() {
    // Minimum shape produced by legacy writers or partial serialization.
    let json = r#"{"services": {"svc": {"status": "stopped"}}}"#;
    let parsed: RigState = serde_json::from_str(json).expect("parse");
    assert!(parsed.last_up.is_none());
    let svc = parsed.services.get("svc").unwrap();
    assert!(svc.pid.is_none());
    assert_eq!(svc.status, "stopped");
}
