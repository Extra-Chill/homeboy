//! Lifecycle pipeline-step tests.
//!
//! The `lifecycle` step promotes `homeboy/lifecycle-contract/v1` — already
//! shipped, already versioned — into a rig step. These tests exercise the real
//! pipeline dispatch path so the serde wiring, ordering, and phase execution
//! are all covered, not just the inner helper.

use std::fs;

use crate::pipeline::run_pipeline;
use crate::spec::RigSpec;
use crate::state::RigState;
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

        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");

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

        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);

        let state = RigState::load(&rig.id).expect("state");
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

        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);

        let state = RigState::load(&rig.id).expect("state");
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

        let up = run_pipeline(&rig, "up", true).expect("up runs");
        assert!(up.is_success(), "outcomes: {:?}", up.steps);
        assert_eq!(
            RigState::load(&rig.id)
                .expect("state")
                .lifecycle_snapshots
                .len(),
            1
        );

        let down = run_pipeline(&rig, "down", true).expect("down runs");
        assert!(down.is_success(), "outcomes: {:?}", down.steps);
        assert!(
            RigState::load(&rig.id)
                .expect("state")
                .lifecycle_snapshots
                .is_empty(),
            "teardown reaps the handles its step owns"
        );
    });
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

// ------------------------------------------------------------------
// Workload-referenced contracts (#10317)
//
// `WorkloadSpec.lifecycle` was parsed and serialized but had no reader: no
// call site anywhere in the workspace read the field or the accessor. These
// tests are that reader's contract. The point of the reference form is that
// one declaration on the workload serves every op the rig runs against it —
// an inline contract has to be restated per op, and two copies drift.
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

        let up = run_pipeline(&rig, "up", true).expect("up runs");
        assert!(up.is_success(), "outcomes: {:?}", up.steps);
        assert_eq!(up.steps[0].kind, "lifecycle");

        let down = run_pipeline(&rig, "down", true).expect("down runs");
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

        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
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

    let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(fs::read_to_string(&marker).expect("marker"), "b");
}

/// Ambiguity is never resolved by guessing.
#[test]
fn test_lifecycle_workload_ref_rejects_ambiguous_selection() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-workload-ambiguous",
            "fuzz_workloads": {
                "generic": [
                    {
                        "path": "fuzz/a.workload.json",
                        "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                    },
                    {
                        "path": "fuzz/b.workload.json",
                        "lifecycle": { "phases": [{ "id": "b", "phase": "prepare", "command": "true" }] }
                    }
                ]
            },
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "workload": { "kind": "fuzz", "extension": "generic" }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("workload.path"), "{error}");
    assert!(error.contains("fuzz/a.workload.json"), "{error}");
    assert!(error.contains("fuzz/b.workload.json"), "{error}");
}

#[test]
fn test_lifecycle_workload_ref_rejects_unknown_extension() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-workload-unknown-extension",
            "fuzz_workloads": {
                "generic": [{
                    "path": "fuzz/a.workload.json",
                    "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                }]
            },
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "workload": { "kind": "fuzz", "extension": "missing" }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("fuzz_workloads"), "{error}");
    assert!(error.contains("generic"), "{error}");
}

#[test]
fn test_lifecycle_workload_ref_rejects_unknown_path() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-workload-unknown-path",
            "fuzz_workloads": {
                "generic": [{
                    "path": "fuzz/a.workload.json",
                    "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                }]
            },
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "workload": {
                        "kind": "fuzz",
                        "extension": "generic",
                        "path": "fuzz/typo.workload.json"
                    }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("fuzz/typo.workload.json"), "{error}");
    assert!(error.contains("fuzz/a.workload.json"), "{error}");
}

/// A workload that exists but declares nothing is not silently a no-op.
#[test]
fn test_lifecycle_workload_ref_rejects_workload_without_contract() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-workload-no-contract",
            "fuzz_workloads": {
                "generic": [{ "path": "fuzz/a.workload.json" }]
            },
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "workload": { "kind": "fuzz", "extension": "generic" }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("declares a lifecycle contract"), "{error}");
}

#[test]
fn test_lifecycle_step_rejects_both_inline_and_workload_sources() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-both-sources",
            "fuzz_workloads": {
                "generic": [{
                    "path": "fuzz/a.workload.json",
                    "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                }]
            },
            "pipeline": {
                "up": [{
                    "kind": "lifecycle",
                    "lifecycle": { "phases": [{ "id": "inline", "phase": "prepare", "command": "true" }] },
                    "workload": { "kind": "fuzz", "extension": "generic" }
                }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("not both"), "{error}");
}

#[test]
fn test_lifecycle_step_rejects_neither_inline_nor_workload_source() {
    let rig = rig_from_json(
        r#"{
            "id": "lifecycle-no-source",
            "pipeline": {
                "up": [{ "kind": "lifecycle", "op": "prepare" }]
            }
        }"#,
    );

    let out = run_pipeline(&rig, "up", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("declares neither `lifecycle` nor `workload`"),
        "{error}"
    );
}

/// Pre-existing inline-contract specs keep parsing and running untouched:
/// making `lifecycle` optional is additive, and `PipelineStep` does not use
/// `deny_unknown_fields`, so no already-authored rig file changes meaning.
#[test]
fn test_inline_lifecycle_specs_round_trip_unchanged() {
    let json = r#"{
        "id": "lifecycle-roundtrip",
        "pipeline": {
            "up": [{
                "kind": "lifecycle",
                "op": "seed",
                "lifecycle": { "phases": [{ "id": "seed", "phase": "seed", "command": "true" }] }
            }]
        }
    }"#;

    let rig = rig_from_json(json);
    let reserialized = serde_json::to_value(&rig).expect("serialize");
    let step = &reserialized["pipeline"]["up"][0];

    assert_eq!(step["kind"], "lifecycle");
    assert_eq!(step["op"], "seed");
    assert_eq!(step["lifecycle"]["phases"][0]["id"], "seed");
    // An absent workload reference stays absent on the way out.
    assert!(step.get("workload").is_none());
}
