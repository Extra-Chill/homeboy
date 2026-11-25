//! Lifecycle pipeline-step tests.
//!
//! The `lifecycle` step promotes `homeboy/lifecycle-contract/v1` — already
//! shipped, already versioned — into a rig step. These tests exercise the real
//! pipeline dispatch path so the serde wiring, ordering, and phase execution
//! are all covered, not just the inner helper.

use std::fs;

use crate::pipeline::run_pipeline;
use crate::spec::RigSpec;
use homeboy_core::test_support::with_isolated_home;

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

    let out =
        run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("pipeline runs");

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

    let out =
        run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        "lifecycle-env:seed:load-fixture"
    );
}

#[test]
fn test_lifecycle_snapshot_handle_is_visible_to_later_phases() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let marker = tmp.path().join("handle.txt");
        let marker_arg = marker.to_string_lossy();

        // The first snapshot phase prints a bare locator; the second sees it as
        // an opaque handle. That is the whole sandbox contract — Homeboy
        // carries the id forward without interpreting it.
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

        let out = run_pipeline(&crate::state::test_state_store(), &rig, "up", true)
            .expect("pipeline runs");

        assert!(out.is_success(), "outcomes: {:?}", out.steps);
        assert_eq!(
            fs::read_to_string(&marker).expect("marker"),
            "capture|opaque-handle-1"
        );
    });
}

#[test]
fn test_lifecycle_snapshot_handle_lands_in_rig_state() {
    with_isolated_home(|_home| {
        let rig = rig_from_json(
            r#"{
                "id": "lifecycle-state",
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "id": "provision",
                        "op": "snapshot",
                        "lifecycle": {
                            "phases": [{
                                "id": "capture",
                                "phase": "snapshot",
                                "command": "printf 'opaque://sandbox/7f3'"
                            }]
                        }
                    }]
                }
            }"#,
        );

        let out = run_pipeline(&crate::state::test_state_store(), &rig, "up", true)
            .expect("pipeline runs");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);

        let state = crate::state::test_state_store()
            .load(&rig.id)
            .expect("state");
        let entry = state
            .lifecycle_snapshots
            .get("capture")
            .expect("captured handle");
        assert_eq!(entry.step, "provision");
        assert_eq!(entry.snapshot.kind, "lifecycle_snapshot");
        assert_eq!(
            entry.snapshot.locator.as_deref(),
            Some("opaque://sandbox/7f3")
        );
        assert!(entry.snapshot.created_at.is_some());
    });
}

#[test]
fn test_lifecycle_snapshot_handle_can_be_a_full_contract_ref() {
    with_isolated_home(|_home| {
        // A runtime that already speaks the contract hands back a full ref.
        let rig = rig_from_json(
            r#"{
                "id": "lifecycle-state-ref",
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "id": "provision",
                        "op": "snapshot",
                        "lifecycle": {
                            "phases": [{
                                "id": "capture",
                                "phase": "snapshot",
                                "command": "printf '{\"id\":\"sandbox-7f3\",\"kind\":\"workspace\",\"locator\":\"opaque://sandbox/7f3\"}'"
                            }]
                        }
                    }]
                }
            }"#,
        );

        let out = run_pipeline(&crate::state::test_state_store(), &rig, "up", true)
            .expect("pipeline runs");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);

        let state = crate::state::test_state_store()
            .load(&rig.id)
            .expect("state");
        let entry = state
            .lifecycle_snapshots
            .get("sandbox-7f3")
            .expect("captured handle");
        assert_eq!(entry.snapshot.kind, "workspace");
        assert_eq!(entry.snapshot.phase_id.as_deref(), Some("capture"));
    });
}

#[test]
fn test_lifecycle_teardown_reaps_the_handles_its_step_owns() {
    with_isolated_home(|_home| {
        let rig = rig_from_json(
            r#"{
                "id": "lifecycle-teardown",
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "id": "provision",
                        "op": "snapshot",
                        "lifecycle": {
                            "phases": [{
                                "id": "capture",
                                "phase": "snapshot",
                                "command": "printf 'opaque://sandbox/7f3'"
                            }]
                        }
                    }],
                    "down": [{
                        "kind": "lifecycle",
                        "id": "provision",
                        "op": "teardown",
                        "lifecycle": {
                            "phases": [{
                                "id": "reap",
                                "phase": "teardown",
                                "command": "test -n \"$HOMEBOY_LIFECYCLE_PHASE_ID\""
                            }]
                        }
                    }]
                }
            }"#,
        );

        let up =
            run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("up runs");
        assert!(up.is_success(), "outcomes: {:?}", up.steps);
        assert_eq!(
            crate::state::test_state_store()
                .load(&rig.id)
                .expect("state")
                .lifecycle_snapshots
                .len(),
            1
        );

        let down =
            run_pipeline(&crate::state::test_state_store(), &rig, "down", true).expect("down runs");
        assert!(down.is_success(), "outcomes: {:?}", down.steps);
        assert!(
            crate::state::test_state_store()
                .load(&rig.id)
                .expect("state")
                .lifecycle_snapshots
                .is_empty(),
            "teardown reaps the handles its step owns"
        );
    });
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

    let out = run_pipeline(&crate::state::test_state_store(), &rig, "up", true)
        .expect("pipeline reports failure");

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

    let out =
        run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert!(marker.exists(), "an optional phase failure is not fatal");
}

// ------------------------------------------------------------------
// Workload-referenced contracts: one declaration on the workload serves
// every op the rig runs against it.
// ------------------------------------------------------------------

#[test]
fn test_lifecycle_step_resolves_a_workload_declared_contract() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let log = tmp.path().join("workload.log");
        let log_arg = log.to_string_lossy();

        // One contract, two ops, zero duplication.
        let rig = rig_from_json(&format!(
            r#"{{
                "id": "lifecycle-workload-ref",
                "fuzz_workloads": {{
                    "generic": [{{
                        "path": "fuzz/read.workload.json",
                        "lifecycle": {{
                            "phases": [
                                {{ "id": "make", "phase": "prepare", "command": "printf 'prepared\n' >> {log}" }},
                                {{ "id": "reap", "phase": "teardown", "command": "printf 'torn\n' >> {log}" }}
                            ]
                        }}
                    }}]
                }},
                "pipeline": {{
                    "up": [{{
                        "kind": "lifecycle",
                        "op": "prepare",
                        "workload": {{ "kind": "fuzz", "extension": "generic" }}
                    }}],
                    "down": [{{
                        "kind": "lifecycle",
                        "op": "teardown",
                        "workload": {{ "kind": "fuzz", "extension": "generic" }}
                    }}]
                }}
            }}"#,
            log = log_arg
        ));

        let up =
            run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("up runs");
        assert!(up.is_success(), "outcomes: {:?}", up.steps);
        assert_eq!(up.steps[0].kind, "lifecycle");

        let down =
            run_pipeline(&crate::state::test_state_store(), &rig, "down", true).expect("down runs");
        assert!(down.is_success(), "outcomes: {:?}", down.steps);

        assert_eq!(
            fs::read_to_string(&log).expect("log"),
            "prepared\ntorn\n",
            "the same workload contract governed both ops"
        );
    });
}

#[test]
fn test_lifecycle_workload_ref_resolves_bench_and_trace_maps() {
    for (kind, map) in [("bench", "bench_workloads"), ("trace", "trace_workloads")] {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let marker = tmp.path().join(format!("{kind}.txt"));
        let marker_arg = marker.to_string_lossy();

        let rig = rig_from_json(&format!(
            r#"{{
                "id": "lifecycle-workload-{kind}",
                "{map}": {{
                    "runner": [{{
                        "path": "workloads/{kind}.json",
                        "lifecycle": {{
                            "phases": [{{ "id": "make", "phase": "prepare", "command": "printf {kind} > {marker}" }}]
                        }}
                    }}]
                }},
                "pipeline": {{
                    "up": [{{
                        "kind": "lifecycle",
                        "workload": {{ "kind": "{kind}", "extension": "runner" }}
                    }}]
                }}
            }}"#,
            kind = kind,
            map = map,
            marker = marker_arg
        ));

        let out = run_pipeline(&crate::state::test_state_store(), &rig, "up", true)
            .expect("pipeline runs");
        assert!(out.is_success(), "{kind} outcomes: {:?}", out.steps);
        assert_eq!(fs::read_to_string(&marker).expect("marker"), kind);
    }
}

#[test]
fn test_lifecycle_workload_ref_disambiguates_by_path() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("picked.txt");
    let marker_arg = marker.to_string_lossy();

    let rig = rig_from_json(&format!(
        r#"{{
            "id": "lifecycle-workload-path",
            "fuzz_workloads": {{
                "generic": [
                    {{
                        "path": "fuzz/a.workload.json",
                        "lifecycle": {{
                            "phases": [{{ "id": "make", "phase": "prepare", "command": "printf a > {marker}" }}]
                        }}
                    }},
                    {{
                        "path": "fuzz/b.workload.json",
                        "lifecycle": {{
                            "phases": [{{ "id": "make", "phase": "prepare", "command": "printf b > {marker}" }}]
                        }}
                    }}
                ]
            }},
            "pipeline": {{
                "up": [{{
                    "kind": "lifecycle",
                    "workload": {{
                        "kind": "fuzz",
                        "extension": "generic",
                        "path": "fuzz/b.workload.json"
                    }}
                }}]
            }}
        }}"#,
        marker = marker_arg
    ));

    let out =
        run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(fs::read_to_string(&marker).expect("marker"), "b");
}

/// Every malformed lifecycle step fails the pipeline with an actionable error;
/// ambiguity and missing contracts are never resolved by guessing or no-op.
#[test]
fn test_lifecycle_step_rejects_invalid_declarations() {
    let one_fuzz_workload = r#""fuzz_workloads": {
        "generic": [{
            "path": "fuzz/a.workload.json",
            "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
        }]
    },"#;
    let cases: [(&str, &str, String, &[&str]); 11] = [
        (
            "unknown-schema",
            "up",
            r#""pipeline": { "up": [{ "kind": "lifecycle", "lifecycle": {
                "schema": "homeboy/lifecycle-contract/v0",
                "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
            } }] }"#
                .to_string(),
            &["expected schema homeboy/lifecycle-contract/v1"],
        ),
        (
            "missing-phase",
            "down",
            r#""pipeline": { "down": [{ "kind": "lifecycle", "op": "teardown", "lifecycle": {
                "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
            } }] }"#
                .to_string(),
            &["declares no 'teardown' phase"],
        ),
        (
            "empty-phase",
            "up",
            r#""pipeline": { "up": [{ "kind": "lifecycle", "lifecycle": {
                "phases": [{ "id": "prepare", "phase": "prepare" }]
            } }] }"#
                .to_string(),
            &["neither extension_hook nor command"],
        ),
        (
            "undeclared-component",
            "up",
            r#""pipeline": { "up": [{ "kind": "lifecycle", "component": "missing", "lifecycle": {
                "phases": [{ "id": "prepare", "phase": "prepare", "command": "true" }]
            } }] }"#
                .to_string(),
            &["missing"],
        ),
        (
            "malformed-hook",
            "up",
            r#""pipeline": { "up": [{ "kind": "lifecycle", "lifecycle": {
                "phases": [{ "id": "prepare", "phase": "prepare", "extension_hook": "notqualified" }]
            } }] }"#
                .to_string(),
            &["<extension-id>.<action-id>"],
        ),
        (
            "ambiguous-workload",
            "up",
            r#""fuzz_workloads": { "generic": [
                {
                    "path": "fuzz/a.workload.json",
                    "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                },
                {
                    "path": "fuzz/b.workload.json",
                    "lifecycle": { "phases": [{ "id": "b", "phase": "prepare", "command": "true" }] }
                }
            ] },
            "pipeline": { "up": [{ "kind": "lifecycle",
                "workload": { "kind": "fuzz", "extension": "generic" } }] }"#
                .to_string(),
            &["workload.path", "fuzz/a.workload.json", "fuzz/b.workload.json"],
        ),
        (
            "unknown-workload-extension",
            "up",
            format!(
                r#"{one_fuzz_workload} "pipeline": {{ "up": [{{ "kind": "lifecycle",
                    "workload": {{ "kind": "fuzz", "extension": "missing" }} }}] }}"#
            ),
            &["fuzz_workloads", "generic"],
        ),
        (
            "unknown-workload-path",
            "up",
            format!(
                r#"{one_fuzz_workload} "pipeline": {{ "up": [{{ "kind": "lifecycle",
                    "workload": {{ "kind": "fuzz", "extension": "generic", "path": "fuzz/typo.workload.json" }} }}] }}"#
            ),
            &["fuzz/typo.workload.json", "fuzz/a.workload.json"],
        ),
        (
            "workload-without-contract",
            "up",
            r#""fuzz_workloads": { "generic": [{ "path": "fuzz/a.workload.json" }] },
            "pipeline": { "up": [{ "kind": "lifecycle",
                "workload": { "kind": "fuzz", "extension": "generic" } }] }"#
                .to_string(),
            &["declares a lifecycle contract"],
        ),
        (
            "both-sources",
            "up",
            format!(
                r#"{one_fuzz_workload} "pipeline": {{ "up": [{{ "kind": "lifecycle",
                    "lifecycle": {{ "phases": [{{ "id": "inline", "phase": "prepare", "command": "true" }}] }},
                    "workload": {{ "kind": "fuzz", "extension": "generic" }} }}] }}"#
            ),
            &["not both"],
        ),
        (
            "no-source",
            "up",
            r#""pipeline": { "up": [{ "kind": "lifecycle", "op": "prepare" }] }"#.to_string(),
            &["declares neither `lifecycle` nor `workload`"],
        ),
    ];

    for (name, phase, body, needles) in cases {
        let rig = rig_from_json(&format!(r#"{{ "id": "lifecycle-{name}", {body} }}"#));
        let out = run_pipeline(&crate::state::test_state_store(), &rig, phase, true)
            .expect("pipeline reports failure");

        assert!(!out.is_success(), "{name} should fail");
        assert_eq!(out.steps[0].kind, "lifecycle", "{name}");
        let error = out.steps[0].error.as_deref().unwrap_or_default();
        for needle in needles {
            assert!(error.contains(needle), "{name}: {error}");
        }
    }
}
