//! Lifecycle pipeline-step tests.
//!
//! The `lifecycle` step promotes `homeboy/lifecycle-contract/v1` — already
//! shipped, already versioned — into a rig step. These tests exercise the real
//! pipeline dispatch path so the serde wiring, ordering, and phase execution
//! are all covered, not just the inner helper.

use std::fs;

use crate::pipeline::run_pipeline;
use crate::spec::RigSpec;

fn rig_from_json(json: &str) -> RigSpec {
    serde_json::from_str(json).expect("parse rig")
}

#[test]
fn test_lifecycle_step_runs_matching_phases_in_declared_order() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let log = tmp.path().join("order.log");
    let log_arg = log.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-order",
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "op": "prepare",
                    "lifecycle": {{
                        "phases": [
                            {{ "id": "first", "phase": "prepare", "command": "printf 'first\n' >> {log}" }},
                            {{ "id": "seeded", "phase": "seed", "command": "printf 'seed\n' >> {log}" }},
                            {{ "id": "second", "phase": "prepare", "command": "printf 'second\n' >> {log}" }}
                        ]
                    }}
                }}]
            }}
        }}"#,
        log = log_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(out.steps[0].kind, "lifecycle");
    assert_eq!(out.steps[0].label, "lifecycle prepare");
    // Declared order is preserved and only the requested phase kind runs.
    assert_eq!(fs::read_to_string(&log).expect("log"), "first\nsecond\n");
}

#[test]
fn test_lifecycle_step_exposes_phase_identity_to_commands() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("env.txt");
    let marker_arg = marker.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-env",
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "op": "seed",
                    "lifecycle": {{
                        "phases": [{{
                            "id": "load-fixture",
                            "phase": "seed",
                            "command": "printf '%s:%s:%s' \"$HOMEBOY_RIG_ID\" \"$HOMEBOY_LIFECYCLE_PHASE\" \"$HOMEBOY_LIFECYCLE_PHASE_ID\" > {marker}"
                        }}]
                    }}
                }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        "lifecycle-env:seed:load-fixture"
    );
}

#[test]
fn test_lifecycle_snapshot_handle_is_visible_to_later_phases() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("handle.txt");
    let marker_arg = marker.to_string_lossy();

    // The first snapshot phase prints a bare locator; the second sees it as an
    // opaque handle. That is the whole sandbox contract — Homeboy carries the
    // id forward without interpreting it.
    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-handle",
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "op": "snapshot",
                    "lifecycle": {{
                        "phases": [
                            {{ "id": "capture", "phase": "snapshot", "command": "printf 'opaque-handle-1'" }},
                            {{ "id": "observe", "phase": "snapshot", "command": "printf '%s|%s' \"$HOMEBOY_LIFECYCLE_SNAPSHOT_ID\" \"$HOMEBOY_LIFECYCLE_SNAPSHOT_LOCATOR\" > {marker}" }}
                        ]
                    }}
                }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        "capture|opaque-handle-1"
    );
}

#[test]
fn test_lifecycle_step_rejects_unknown_contract_schema() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-schema",
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "lifecycle": {
                        "schema": "homeboy/lifecycle-contract/v0",
                        "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    assert_eq!(out.steps[0].kind, "lifecycle");
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("expected schema homeboy/lifecycle-contract/v1"),
        "{error}"
    );
}

#[test]
fn test_lifecycle_step_fails_when_op_has_no_declared_phase() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-missing-phase",
            "pipeline": {
                "down": [{
                    "kind": "lifecycle",
                    "op": "teardown",
                    "lifecycle": {
                        "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "down", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("declares no 'teardown' phase"), "{error}");
}

#[test]
fn test_lifecycle_step_rejects_phase_without_hook_or_command() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-empty-phase",
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "lifecycle": {
                        "phases": [{ "id": "prepare", "phase": "prepare" }]
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("neither extension_hook nor command"),
        "{error}"
    );
}

#[test]
fn test_lifecycle_step_fails_on_required_phase_failure() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("never.txt");
    let marker_arg = marker.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-required",
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "lifecycle": {{
                        "phases": [
                            {{ "id": "boom", "phase": "prepare", "command": "exit 3" }},
                            {{ "id": "after", "phase": "prepare", "command": "printf x > {marker}" }}
                        ]
                    }}
                }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("exited 3"), "{error}");
    assert!(!marker.exists(), "a required phase failure halts the step");
}

#[test]
fn test_lifecycle_step_continues_past_optional_phase_failure() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("after.txt");
    let marker_arg = marker.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-optional",
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "lifecycle": {{
                        "phases": [
                            {{ "id": "best-effort", "phase": "prepare", "command": "exit 1", "required": false }},
                            {{ "id": "after", "phase": "prepare", "command": "printf x > {marker}" }}
                        ]
                    }}
                }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert!(marker.exists(), "an optional phase failure is not fatal");
}

#[test]
fn test_lifecycle_step_fails_on_undeclared_component() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-component",
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "component": "missing",
                    "lifecycle": {
                        "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn test_lifecycle_step_rejects_malformed_extension_hook() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-hook",
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "lifecycle": {
                        "phases": [{
                            "id": "prepare",
                            "phase": "prepare",
                            "extension_hook": "notqualified"
                        }]
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("<extension-id>.<action-id>"), "{error}");
}

/// Non-breaking guarantee: a pipeline with no lifecycle step behaves exactly
/// as it did before the variant existed.
#[test]
fn test_pipeline_without_lifecycle_step_is_unchanged() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("plain.txt");
    let marker_arg = marker.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "no-lifecycle",
            "pipeline": {{
                "up": [{{ "kind": "command", "command": "printf x > {marker}" }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(out.steps[0].kind, "command");
    assert!(marker.exists());
}
