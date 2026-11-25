//! Schema round-trip tests for `BenchSpec::default_baseline_rig`.
//!
//! Pin the JSON shape so a future refactor can't quietly change how
//! the field serializes — consumers (rig spec authors, downstream
//! tooling) read this directly off disk.

use crate::spec::{BenchSpec, RigSpec, WorkloadSpec};

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
