//! Schema round-trip tests for `BenchSpec::default_baseline_rig`.
//!
//! Pin the JSON shape so a future refactor can't quietly change how
//! the field serializes — consumers (rig spec authors, downstream
//! tooling) read this directly off disk.

use crate::rig::spec::{BenchSpec, RigSpec};

/// Parses a minimal RigSpec JSON via serde and returns the embedded
/// `BenchSpec` (or panics).
fn bench_from(json: &str) -> BenchSpec {
    let spec: RigSpec = serde_json::from_str(json).expect("parse RigSpec");
    spec.bench.expect("bench block present")
}

#[test]
fn test_bench_spec_deserializes_both_fields() {
    let spec = bench_from(
        r#"{
            "id": "candidate",
            "bench": {
                "default_component": "homeboy",
                "default_baseline_rig": "homeboy-main"
            }
        }"#,
    );
    assert_eq!(spec.default_component.as_deref(), Some("homeboy"));
    assert_eq!(spec.default_baseline_rig.as_deref(), Some("homeboy-main"));
}

#[test]
fn test_bench_spec_default_component_only_back_compat() {
    // Pre-PR specs declare only `default_component`; the new field
    // must default to None so existing rigs keep parsing.
    let spec = bench_from(
        r#"{
            "id": "legacy",
            "bench": { "default_component": "homeboy" }
        }"#,
    );
    assert_eq!(spec.default_component.as_deref(), Some("homeboy"));
    assert!(spec.default_baseline_rig.is_none());
}

#[test]
fn test_bench_spec_default_baseline_only_orthogonal() {
    // The two fields are independent — a rig may declare only the
    // baseline reference without pinning a default component.
    let spec = bench_from(
        r#"{
            "id": "candidate",
            "bench": { "default_baseline_rig": "homeboy-main" }
        }"#,
    );
    assert!(spec.default_component.is_none());
    assert_eq!(spec.default_baseline_rig.as_deref(), Some("homeboy-main"));
}

#[test]
fn test_rig_spec_without_bench_block_back_compat() {
    // Rig specs that don't bench at all must still parse, with the
    // entire `bench` field as None.
    let json = r#"{ "id": "no-bench" }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    assert!(spec.bench.is_none());
}

#[test]
fn test_bench_spec_round_trip_preserves_both_fields() {
    let original_json = r#"{
        "id": "candidate",
        "bench": {
            "default_component": "homeboy",
            "default_baseline_rig": "homeboy-main"
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(original_json).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let reparsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");

    let bench = reparsed.bench.expect("bench preserved");
    assert_eq!(bench.default_component.as_deref(), Some("homeboy"));
    assert_eq!(bench.default_baseline_rig.as_deref(), Some("homeboy-main"));
}

#[test]
fn test_bench_spec_skips_serializing_none_fields() {
    // `skip_serializing_if = "Option::is_none"` keeps re-serialized
    // specs minimal — a rig that only sets one of the two fields must
    // not gain a `null` entry for the other when round-tripped.
    let json = r#"{
        "id": "candidate",
        "bench": { "default_baseline_rig": "homeboy-main" }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    assert!(
        !re_serialized.contains("default_component"),
        "expected default_component absent from re-serialized JSON, got: {}",
        re_serialized
    );
    assert!(re_serialized.contains("default_baseline_rig"));
}

#[test]
fn test_bench_spec_self_reference_parses_cleanly() {
    // The dispatcher rejects self-reference at runtime, but the spec
    // itself must still parse — the self-reference detection is a
    // dispatch-time concern, not a parse-time one. Splits the
    // responsibility so a stale-on-disk spec doesn't crash `rig list`
    // / `rig show`.
    let spec = bench_from(
        r#"{
            "id": "homeboy-main",
            "bench": { "default_baseline_rig": "homeboy-main" }
        }"#,
    );
    assert_eq!(spec.default_baseline_rig.as_deref(), Some("homeboy-main"));
}

#[test]
fn test_bench_spec_unknown_fields_ignored() {
    // serde's default is to silently accept extra keys. Pin that —
    // it's the back-compat story for adding more fields after this
    // PR (e.g. matrix expansion in #1466 follow-ups).
    let spec = bench_from(
        r#"{
            "id": "future",
            "bench": {
                "default_baseline_rig": "main",
                "future_matrix_field": ["a", "b"]
            }
        }"#,
    );
    assert_eq!(spec.default_baseline_rig.as_deref(), Some("main"));
}
