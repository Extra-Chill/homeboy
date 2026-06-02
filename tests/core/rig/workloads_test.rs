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
                "extension-b": [
                    { "path": "${env.HOMEBOY_TEST_BENCH_ROOT}/cold-boot.php" },
                    { "path": "${components.playground.path}/fixtures/wc-loaded.php" }
                ],
                "extension-a": [{ "path": "/tmp/node-only.bench.ts" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    let workloads = workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "extension-b");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-benches/cold-boot.php"),
            PathBuf::from("/tmp/playground/fixtures/wc-loaded.php"),
        ]
    );
    assert!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "extension-c").is_empty()
    );
}

#[test]
fn test_workload_string_shorthand_is_rejected() {
    let err = serde_json::from_str::<RigSpec>(
        r#"{
            "id": "studio",
            "bench_workloads": {
                "extension-a": ["/tmp/legacy.bench.mjs"]
            }
        }"#,
    )
    .expect_err("string workload shorthand should be rejected");

    assert!(
        err.to_string().contains("invalid type: string"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn test_invocation_requirements_for_extension_workloads() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "playground-bench",
            "bench_workloads": {
                "extension-a": [
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
        "extension-a",
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
                "extension-a": [
                    { "path": "${env.HOMEBOY_TEST_TRACE_ROOT}/create-site.trace.mjs" },
                    { "path": "${components.studio.path}/bench/admin-load.trace.mjs" }
                ],
                "extension-b": [{ "path": "/tmp/wp.trace.php" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    let workloads = workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "extension-a");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-traces/create-site.trace.mjs"),
            PathBuf::from("/tmp/studio/bench/admin-load.trace.mjs"),
        ]
    );
    assert!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "extension-c").is_empty()
    );
}

#[test]
fn test_extension_workloads_expand_package_root_when_available() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio-agent-sdk",
            "bench_workloads": {
                "extension-a": [{ "path": "${package.root}/bench/studio-agent-runtime.bench.mjs" }]
            },
            "trace_workloads": {
                "extension-a": [{ "path": "${package.root}/bench/studio-app-create-site.trace.mjs" }]
            }
        }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/Automattic/studio");

    assert_eq!(
        workloads_for_extension(
            &rig_spec,
            RigWorkloadKind::Bench,
            Some(&package),
            "extension-a"
        ),
        vec![PathBuf::from(
            "/tmp/homeboy-rigs/Automattic/studio/bench/studio-agent-runtime.bench.mjs"
        )]
    );
    assert_eq!(
        workloads_for_extension(
            &rig_spec,
            RigWorkloadKind::Trace,
            Some(&package),
            "extension-a"
        ),
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
                "extension-a": [{ "path": "${package.root}/bench/manual.bench.mjs" }]
            },
            "trace_workloads": {
                "extension-a": [{ "path": "${package.root}/bench/manual.trace.mjs" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Bench, None, "extension-a"),
        vec![PathBuf::from("${package.root}/bench/manual.bench.mjs")]
    );
    assert_eq!(
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "extension-a"),
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
                "extension-a": [
                    {
                        "path": "${components.studio.path}/bench/create-site.trace.mjs",
                        "check_groups": ["desktop-app", "extension-a-trace"]
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
        workloads_for_extension(&rig_spec, RigWorkloadKind::Trace, None, "extension-a"),
        vec![
            PathBuf::from("/tmp/studio/bench/create-site.trace.mjs"),
            PathBuf::from("/tmp/other.trace.mjs"),
        ]
    );
    assert_eq!(
        check_groups_for_extension_workloads(&rig_spec, RigWorkloadKind::Trace, "extension-a")
            .expect("scoped groups"),
        vec!["desktop-app".to_string(), "extension-a-trace".to_string()]
    );
}

#[test]
fn test_workloads_without_check_groups_keep_full_check_contract() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "trace_workloads": {
                "extension-a": [{ "path": "/tmp/create-site.trace.mjs" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        check_groups_for_extension_workloads(&rig_spec, RigWorkloadKind::Trace, "extension-a"),
        None
    );
}

#[test]
fn test_extension_ids_for_workloads_are_sorted_by_kind() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "studio",
            "bench_workloads": {
                "extension-b": [{ "path": "/tmp/wp.bench.php" }],
                "extension-a": [{ "path": "/tmp/node.bench.mjs" }]
            },
            "trace_workloads": {
                "extension-c": [{ "path": "/tmp/rust.trace.rs" }],
                "extension-a": [{ "path": "/tmp/node.trace.mjs" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        extension_ids_for_workloads(&rig_spec, RigWorkloadKind::Bench),
        vec!["extension-a".to_string(), "extension-b".to_string()]
    );
    assert_eq!(
        extension_ids_for_workloads(&rig_spec, RigWorkloadKind::Trace),
        vec!["extension-a".to_string(), "extension-c".to_string()]
    );
}
