use std::path::PathBuf;

use crate::core::rig::spec::RigSpec;
use crate::core::rig::{
    check_groups_for_extension_workloads, extension_ids_for_workloads, workloads_for_extension,
    RigWorkloadKind,
};

#[test]
fn test_bench_workloads_for_extension_filters_and_expands_paths() {
    std::env::set_var("HOMEBOY_TEST_BENCH_ROOT", "/tmp/private-benches");
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "components": {
                "playground": { "path": "/tmp/playground" }
            },
            "bench_workloads": {
                "wordpress": [
                    "${env.HOMEBOY_TEST_BENCH_ROOT}/cold-boot.php",
                    "${components.playground.path}/fixtures/wc-loaded.php"
                ],
                "nodejs": ["/tmp/node-only.bench.ts"]
            }
        }"#,
    )
    .expect("parse rig spec");

    let workloads = workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "wordpress");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-benches/cold-boot.php"),
            PathBuf::from("/tmp/playground/fixtures/wc-loaded.php"),
        ]
    );
    assert!(workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "rust").is_empty());
}

#[test]
fn test_invocation_requirements_for_extension_workloads() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "playground-bench",
            "bench_workloads": {
                "nodejs": [
                    {
                        "path": "/tmp/playground-server.bench.mjs",
                        "port_range_size": 8,
                        "named_leases": ["browser-profile"]
                    },
                    {
                        "path": "/tmp/playground-browser.bench.mjs",
                        "port_range_size": 3,
                        "named_leases": ["browser-profile", "wasm-cache"]
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");

    let requirements = crate::core::rig::invocation_requirements_for_extension_workloads(
        &rig_spec,
        crate::core::rig::RigWorkloadKind::Bench,
        "nodejs",
    );

    assert_eq!(requirements.port_range_size, Some(8));
    assert_eq!(
        requirements.named_leases,
        vec!["browser-profile".to_string(), "wasm-cache".to_string()]
    );
}

#[test]
fn test_trace_workloads_for_extension_filters_and_expands_paths() {
    std::env::set_var("HOMEBOY_TEST_TRACE_ROOT", "/tmp/private-traces");
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "components": {
                "studio": { "path": "/tmp/studio" }
            },
            "trace_workloads": {
                "nodejs": [
                    "${env.HOMEBOY_TEST_TRACE_ROOT}/create-site.trace.mjs",
                    "${components.studio.path}/bench/admin-load.trace.mjs"
                ],
                "wordpress": ["/tmp/wp.trace.php"]
            }
        }"#,
    )
    .expect("parse rig spec");

    let workloads = workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "nodejs");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-traces/create-site.trace.mjs"),
            PathBuf::from("/tmp/studio/bench/admin-load.trace.mjs"),
        ]
    );
    assert!(workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "rust").is_empty());
}

#[test]
fn test_extension_workloads_expand_package_root_when_available() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio-agent-sdk",
            "bench_workloads": {
                "nodejs": ["${package.root}/bench/studio-agent-runtime.bench.mjs"]
            },
            "trace_workloads": {
                "nodejs": ["${package.root}/bench/studio-app-create-site.trace.mjs"]
            }
        }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/Automattic/studio");

    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, Some(&package), "nodejs"),
        vec![PathBuf::from(
            "/tmp/homeboy-rigs/Automattic/studio/bench/studio-agent-runtime.bench.mjs"
        )]
    );
    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, Some(&package), "nodejs"),
        vec![PathBuf::from(
            "/tmp/homeboy-rigs/Automattic/studio/bench/studio-app-create-site.trace.mjs"
        )]
    );
}

#[test]
fn test_extension_workloads_leave_package_root_unexpanded_without_metadata() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "manual",
            "bench_workloads": {
                "nodejs": ["${package.root}/bench/manual.bench.mjs"]
            },
            "trace_workloads": {
                "nodejs": ["${package.root}/bench/manual.trace.mjs"]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "nodejs"),
        vec![PathBuf::from("${package.root}/bench/manual.bench.mjs")]
    );
    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "nodejs"),
        vec![PathBuf::from("${package.root}/bench/manual.trace.mjs")]
    );
}

#[test]
fn test_check_groups_for_extension_workloads() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "components": {
                "studio": { "path": "/tmp/studio" }
            },
            "trace_workloads": {
                "nodejs": [
                    {
                        "path": "${components.studio.path}/bench/create-site.trace.mjs",
                        "check_groups": ["desktop-app", "nodejs-trace"]
                    },
                    {
                        "path": "/tmp/other.trace.mjs",
                        "check_groups": ["desktop-app"]
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "nodejs"),
        vec![
            PathBuf::from("/tmp/studio/bench/create-site.trace.mjs"),
            PathBuf::from("/tmp/other.trace.mjs"),
        ]
    );
    assert_eq!(
        check_groups_for_extension_workloads(&rig_spec, RigWorkloadKind::Trace, "nodejs")
            .expect("scoped groups"),
        vec!["desktop-app".to_string(), "nodejs-trace".to_string()]
    );
}

#[test]
fn test_string_workloads_keep_full_check_contract() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "trace_workloads": {
                "nodejs": ["/tmp/create-site.trace.mjs"]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        check_groups_for_extension_workloads(&rig_spec, RigWorkloadKind::Trace, "nodejs"),
        None
    );
}

#[test]
fn test_mixed_workload_declarations_keep_full_check_contract() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "bench_workloads": {
                "nodejs": [
                    { "path": "/tmp/scoped.bench.mjs", "check_groups": ["desktop-app"] },
                    "/tmp/legacy.bench.mjs"
                ]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        check_groups_for_extension_workloads(&rig_spec, RigWorkloadKind::Bench, "nodejs"),
        None
    );
}

#[test]
fn test_extension_ids_for_workloads_are_sorted_by_kind() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "bench_workloads": {
                "wordpress": ["/tmp/wp.bench.php"],
                "nodejs": ["/tmp/node.bench.mjs"]
            },
            "trace_workloads": {
                "rust": ["/tmp/rust.trace.rs"],
                "nodejs": ["/tmp/node.trace.mjs"]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        extension_ids_for_workloads(&rig_spec, RigWorkloadKind::Bench),
        vec!["nodejs".to_string(), "wordpress".to_string()]
    );
    assert_eq!(
        extension_ids_for_workloads(&rig_spec, RigWorkloadKind::Trace),
        vec!["nodejs".to_string(), "rust".to_string()]
    );
}
