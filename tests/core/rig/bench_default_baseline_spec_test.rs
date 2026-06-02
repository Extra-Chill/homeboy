//! Schema round-trip tests for `BenchSpec::default_baseline_rig`.
//!
//! Pin the JSON shape so a future refactor can't quietly change how
//! the field serializes — consumers (rig spec authors, downstream
//! tooling) read this directly off disk.

use crate::core::extension::bench::BenchGateOp;
use crate::core::rig::spec::{BenchMetricGateCondition, BenchSpec, RigSpec, WorkloadSpec};

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
                "default_baseline_rig": "homeboy-main",
                "warmup_iterations": 3
            }
        }"#,
    );
    assert_eq!(spec.default_component.as_deref(), Some("homeboy"));
    assert!(spec.components.is_empty());
    assert_eq!(spec.default_baseline_rig.as_deref(), Some("homeboy-main"));
    assert_eq!(spec.warmup_iterations, Some(3));
}

#[test]
fn test_bench_spec_deserializes_axes() {
    let spec = bench_from(
        r#"{
            "id": "studio-sdk-bfb",
            "bench": {
                "default_component": "studio",
                "axes": {
                    "runtime": "sdk",
                    "substrate": "bfb"
                }
            }
        }"#,
    );

    assert_eq!(spec.axes.get("runtime").map(String::as_str), Some("sdk"));
    assert_eq!(spec.axes.get("substrate").map(String::as_str), Some("bfb"));
}

#[test]
fn test_bench_spec_deserializes_component_matrix() {
    let spec = bench_from(
        r#"{
            "id": "mdi-substrates",
            "bench": {
                "components": ["mdi-sdi", "mdi-mirror", "mdi-primary"]
            }
        }"#,
    );

    assert_eq!(
        spec.components,
        vec![
            "mdi-sdi".to_string(),
            "mdi-mirror".to_string(),
            "mdi-primary".to_string(),
        ]
    );
    assert!(spec.default_component.is_none());
}

#[test]
fn test_rig_spec_deserializes_bench_workloads_by_extension() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "bench_workloads": {
                "extension-b": [
                    { "path": "/private/benches/cold-boot.php" },
                    { "path": "~/benches/wc-loaded.php" }
                ],
                "extension-a": [{ "path": "/private/benches/electron-startup.bench.ts" }]
            }
        }"#,
    )
    .expect("parse RigSpec");

    let extension_b: Vec<&str> = spec
        .bench_workloads
        .get("extension-b")
        .expect("extension-b workloads")
        .iter()
        .map(|workload| workload.path())
        .collect();
    assert_eq!(
        extension_b,
        vec!["/private/benches/cold-boot.php", "~/benches/wc-loaded.php"]
    );
    let extension_a: Vec<&str> = spec
        .bench_workloads
        .get("extension-a")
        .expect("extension-a workloads")
        .iter()
        .map(|workload| workload.path())
        .collect();
    assert_eq!(
        extension_a,
        vec!["/private/benches/electron-startup.bench.ts"]
    );
}

#[test]
fn test_rig_spec_deserializes_trace_workloads_by_extension() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "trace_workloads": {
                "extension-a": [
                    { "path": "${package.root}/bench/studio-app-create-site.trace.mjs" },
                    { "path": "~/traces/window-close.trace.mjs" }
                ],
                "extension-b": [{ "path": "/private/traces/wp-admin-load.trace.php" }]
            }
        }"#,
    )
    .expect("parse RigSpec");

    let extension_a: Vec<&str> = spec
        .trace_workloads
        .get("extension-a")
        .expect("extension-a workloads")
        .iter()
        .map(|workload| workload.path())
        .collect();
    assert_eq!(
        extension_a,
        vec![
            "${package.root}/bench/studio-app-create-site.trace.mjs",
            "~/traces/window-close.trace.mjs",
        ]
    );
    let extension_b: Vec<&str> = spec
        .trace_workloads
        .get("extension-b")
        .expect("extension-b workloads")
        .iter()
        .map(|workload| workload.path())
        .collect();
    assert_eq!(extension_b, vec!["/private/traces/wp-admin-load.trace.php"]);
}

#[test]
fn test_trace_phase_preset() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "trace_workloads": {
                "extension-a": [
                    {
                        "path": "${package.root}/bench/studio.trace.mjs",
                        "trace_phase_presets": {
                            "startup": ["launch", "ready"]
                        }
                    }
                ]
            }
        }"#,
    )
    .expect("parse RigSpec");

    let workload = spec
        .trace_workloads
        .get("extension-a")
        .and_then(|workloads| workloads.first())
        .expect("extension-a trace workload");

    assert_eq!(workload.trace_phase_preset("missing"), None);
    assert_eq!(
        workload.trace_phase_preset("startup"),
        Some(["launch".to_string(), "ready".to_string()].as_slice())
    );
    let workload_without_preset: WorkloadSpec =
        serde_json::from_str(r#"{"path":"trace.mjs"}"#).expect("parse workload");
    assert_eq!(workload_without_preset.trace_phase_preset("startup"), None);
}

#[test]
fn test_trace_default_phase_preset() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "trace_workloads": {
                "extension-a": [
                    {
                        "path": "${package.root}/bench/studio.trace.mjs",
                        "trace_default_phase_preset": "startup"
                    }
                ]
            }
        }"#,
    )
    .expect("parse RigSpec");

    let workload = spec
        .trace_workloads
        .get("extension-a")
        .and_then(|workloads| workloads.first())
        .expect("extension-a trace workload");

    assert_eq!(workload.trace_default_phase_preset(), Some("startup"));
    let workload_without_default: WorkloadSpec =
        serde_json::from_str(r#"{"path":"trace.mjs"}"#).expect("parse workload");
    assert_eq!(workload_without_default.trace_default_phase_preset(), None);
}

#[test]
fn test_to_gates() {
    let condition = BenchMetricGateCondition {
        equals: Some(1.0),
        gte: Some(0.5),
        lte: Some(2.0),
    };

    let gates = condition.to_gates("native_block_quality_pass");

    assert_eq!(gates.len(), 3);
    assert!(gates.iter().any(|gate| {
        gate.metric == "native_block_quality_pass"
            && gate.op == BenchGateOp::Eq
            && gate.value == 1.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.metric == "native_block_quality_pass"
            && gate.op == BenchGateOp::Gte
            && gate.value == 0.5
    }));
    assert!(gates.iter().any(|gate| {
        gate.metric == "native_block_quality_pass"
            && gate.op == BenchGateOp::Lte
            && gate.value == 2.0
    }));
    assert!(BenchMetricGateCondition {
        equals: None,
        gte: None,
        lte: None,
    }
    .to_gates("metric")
    .is_empty());
}

#[test]
fn test_rig_component_deserializes_extension_config() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "components": {
                "studio": {
                    "path": "~/Developer/studio",
                    "extensions": {
                        "extension-a": {
                            "package_manager": "pnpm",
                            "workspace": "apps/studio"
                        }
                    }
                }
            },
            "bench": { "default_component": "studio" }
        }"#,
    )
    .expect("parse RigSpec");

    let component = spec.components.get("studio").expect("studio component");
    let extensions = component.extensions.as_ref().expect("extensions present");
    let extension_a = extensions.get("extension-a").expect("extension-a config");

    assert_eq!(component.path, "~/Developer/studio");
    assert_eq!(
        extension_a.settings.get("package_manager"),
        Some(&serde_json::json!("pnpm"))
    );
    assert_eq!(
        extension_a.settings.get("workspace"),
        Some(&serde_json::json!("apps/studio"))
    );
}

#[test]
fn test_rig_component_extension_config_round_trips() {
    let original_json = r#"{
        "id": "studio",
        "components": {
            "studio": {
                "path": "/tmp/studio",
                "extensions": {
                    "extension-a": { "package_manager": "pnpm" }
                }
            }
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(original_json).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let reparsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");

    let extensions = reparsed
        .components
        .get("studio")
        .and_then(|component| component.extensions.as_ref())
        .expect("extensions preserved");
    assert!(extensions.contains_key("extension-a"));
    assert!(re_serialized.contains("extensions"));
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
    assert!(spec.components.is_empty());
    assert!(spec.default_baseline_rig.is_none());
    assert!(spec.warmup_iterations.is_none());
    assert!(spec.axes.is_empty());
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
    assert!(spec.bench_workloads.is_empty());
    assert!(spec.trace_workloads.is_empty());
}

#[test]
fn test_bench_spec_round_trip_preserves_both_fields() {
    let original_json = r#"{
        "id": "candidate",
            "bench": {
                "default_component": "homeboy",
                "default_baseline_rig": "homeboy-main",
                "warmup_iterations": 4
            }
        }"#;
    let spec: RigSpec = serde_json::from_str(original_json).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let reparsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");

    let bench = reparsed.bench.expect("bench preserved");
    assert_eq!(bench.default_component.as_deref(), Some("homeboy"));
    assert!(bench.components.is_empty());
    assert_eq!(bench.default_baseline_rig.as_deref(), Some("homeboy-main"));
    assert_eq!(bench.warmup_iterations, Some(4));
}

#[test]
fn test_bench_spec_round_trip_preserves_component_matrix() {
    let original_json = r#"{
        "id": "mdi-substrates",
        "bench": {
            "components": ["mdi-sdi", "mdi-mirror"],
            "default_baseline_rig": "mdi-main"
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(original_json).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let reparsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");

    let bench = reparsed.bench.expect("bench preserved");
    assert_eq!(
        bench.components,
        vec!["mdi-sdi".to_string(), "mdi-mirror".to_string()]
    );
    assert_eq!(bench.default_baseline_rig.as_deref(), Some("mdi-main"));
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
    assert!(
        !re_serialized.contains("components"),
        "expected empty components absent from re-serialized JSON, got: {}",
        re_serialized
    );
    assert!(re_serialized.contains("default_baseline_rig"));
    assert!(!re_serialized.contains("warmup_iterations"));
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
