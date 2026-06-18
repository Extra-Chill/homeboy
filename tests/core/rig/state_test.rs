//! State persistence tests for `src/core/rig/state.rs`.
//!
//! The load/save path is gated on `~/.config/homeboy/` (hard to test without
//! touching real user state), so this module exercises serde round-tripping
//! which is the meaningful invariant.

use std::collections::BTreeMap;

use crate::core::rig::spec::RigResourcesSpec;
use crate::core::rig::state::{
    ComponentSnapshot, MaterializedRigState, RigState, RigStateSnapshot, ServiceState,
    SharedPathState,
};

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
        materialized: None,
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
fn test_state_round_trips_with_materialized_ownership() {
    let state = RigState {
        materialized: Some(MaterializedRigState {
            rig_id: "studio".to_string(),
            materialized_at: "2026-04-30T13:00:00Z".to_string(),
            resources: RigResourcesSpec {
                exclusive: vec!["studio-dev".to_string()],
                paths: vec!["/tmp/studio".to_string()],
                ports: vec![9724],
                process_patterns: vec!["app-server-child".to_string()],
            },
            components: Default::default(),
        }),
        ..RigState::default()
    };

    let json = serde_json::to_string(&state).expect("serialize");
    let parsed: RigState = serde_json::from_str(&json).expect("parse");
    let materialized = parsed.materialized.expect("materialized ownership");
    assert_eq!(materialized.rig_id, "studio");
    assert_eq!(materialized.resources.ports, vec![9724]);
    assert_eq!(
        materialized.resources.process_patterns,
        vec!["app-server-child"]
    );
}

fn snapshot_with_component(component_id: &str, path: &str) -> RigStateSnapshot {
    let mut components = BTreeMap::new();
    components.insert(
        component_id.to_string(),
        ComponentSnapshot {
            path: path.to_string(),
            declared_path: None,
            sha: Some("aaaaaaaa".to_string()),
            branch: Some("main".to_string()),
        },
    );
    RigStateSnapshot {
        rig_id: "studio".to_string(),
        captured_at: "2026-05-04T00:00:00Z".to_string(),
        components,
    }
}

#[test]
fn set_effective_component_path_records_override_and_preserves_declared() {
    let mut snapshot = snapshot_with_component("studio", "/Users/user/Developer/studio");

    snapshot.set_effective_component_path(
        "studio",
        "/Users/user/Developer/studio@worktree",
        |path| {
            assert_eq!(path, "/Users/user/Developer/studio@worktree");
            (Some("bbbbbbbb".to_string()), Some("feature".to_string()))
        },
    );

    let component = snapshot.components.get("studio").expect("component");
    assert_eq!(component.path, "/Users/user/Developer/studio@worktree");
    assert_eq!(
        component.declared_path.as_deref(),
        Some("/Users/user/Developer/studio")
    );
    assert_eq!(component.sha.as_deref(), Some("bbbbbbbb"));
    assert_eq!(component.branch.as_deref(), Some("feature"));
}

#[test]
fn set_effective_component_path_is_noop_when_path_matches() {
    let mut snapshot = snapshot_with_component("studio", "/Users/user/Developer/studio");

    snapshot.set_effective_component_path("studio", "/Users/user/Developer/studio", |_| {
        panic!("git lookup must not run when paths already match")
    });

    let component = snapshot.components.get("studio").expect("component");
    assert_eq!(component.path, "/Users/user/Developer/studio");
    assert!(component.declared_path.is_none());
    assert_eq!(component.sha.as_deref(), Some("aaaaaaaa"));
    assert_eq!(component.branch.as_deref(), Some("main"));
}

#[test]
fn set_effective_component_path_ignores_unknown_component() {
    let mut snapshot = snapshot_with_component("studio", "/Users/user/Developer/studio");

    snapshot.set_effective_component_path("missing", "/tmp/anywhere", |_| {
        panic!("git lookup must not run when component is unknown");
    });

    let component = snapshot.components.get("studio").expect("component");
    assert_eq!(component.path, "/Users/user/Developer/studio");
    assert!(component.declared_path.is_none());
    assert!(snapshot.components.get("missing").is_none());
}

#[test]
fn set_effective_component_path_clears_stale_git_metadata_for_non_repo_path() {
    let mut snapshot = snapshot_with_component("studio", "/Users/user/Developer/studio");

    snapshot.set_effective_component_path("studio", "/tmp/not-a-repo", |_| (None, None));

    let component = snapshot.components.get("studio").expect("component");
    assert_eq!(component.path, "/tmp/not-a-repo");
    assert_eq!(
        component.declared_path.as_deref(),
        Some("/Users/user/Developer/studio")
    );
    assert!(component.sha.is_none());
    assert!(component.branch.is_none());
}

#[test]
fn component_snapshot_round_trips_declared_path() {
    let snapshot = ComponentSnapshot {
        path: "/tmp/effective".to_string(),
        declared_path: Some("/tmp/declared".to_string()),
        sha: None,
        branch: None,
    };
    let json = serde_json::to_string(&snapshot).expect("serialize");
    assert!(json.contains("\"declared_path\":\"/tmp/declared\""));
    let parsed: ComponentSnapshot = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed.declared_path.as_deref(), Some("/tmp/declared"));
}

#[test]
fn component_snapshot_omits_declared_path_when_absent() {
    let snapshot = ComponentSnapshot {
        path: "/tmp/effective".to_string(),
        declared_path: None,
        sha: None,
        branch: None,
    };
    let json = serde_json::to_string(&snapshot).expect("serialize");
    assert!(
        !json.contains("declared_path"),
        "json should omit declared_path when None: {json}"
    );
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
