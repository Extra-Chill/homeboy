//! On-disk JSON contract tests for rig `bench` blocks and component
//! extension config.
//!
//! Rig spec authors and downstream tooling read these fields directly off
//! disk, so pin both how they parse and how a re-serialized spec stays
//! minimal.

use crate::spec::RigSpec;

#[test]
fn test_rig_spec_parses_bench_block_and_component_extension_config() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "candidate",
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
            "bench": {
                "default_component": "studio",
                "default_baseline_rig": "homeboy-main",
                "warmup_iterations": 3,
                "axes": { "runtime": "sdk", "substrate": "bfb" }
            }
        }"#,
    )
    .expect("parse RigSpec");

    let bench = spec.bench.as_ref().expect("bench block present");
    assert_eq!(bench.default_component.as_deref(), Some("studio"));
    assert!(bench.components.is_empty());
    assert_eq!(bench.default_baseline_rig.as_deref(), Some("homeboy-main"));
    assert_eq!(bench.warmup_iterations, Some(3));
    assert_eq!(bench.axes.get("runtime").map(String::as_str), Some("sdk"));
    assert_eq!(bench.axes.get("substrate").map(String::as_str), Some("bfb"));

    let component = spec.components.get("studio").expect("studio component");
    let extension_a = component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("extension-a"))
        .expect("extension-a config");
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
fn test_rig_spec_round_trip_keeps_extension_config_and_omits_unset_bench_fields() {
    let spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "candidate",
            "components": {
                "studio": {
                    "path": "/tmp/studio",
                    "extensions": {
                        "extension-a": { "package_manager": "pnpm" }
                    }
                }
            },
            "bench": { "default_baseline_rig": "homeboy-main" }
        }"#,
    )
    .expect("parse");
    let re_serialized = serde_json::to_value(&spec).expect("serialize");

    assert_eq!(
        re_serialized["components"]["studio"]["extensions"]["extension-a"]["package_manager"],
        "pnpm"
    );
    // A rig that sets one bench field must not gain `null`/empty entries for
    // the others when round-tripped.
    let bench = re_serialized["bench"].as_object().expect("bench object");
    assert_eq!(bench["default_baseline_rig"], "homeboy-main");
    for absent in ["default_component", "components", "warmup_iterations"] {
        assert!(
            !bench.contains_key(absent),
            "expected `{absent}` absent from re-serialized bench: {bench:?}"
        );
    }
}
