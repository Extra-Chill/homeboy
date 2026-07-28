use std::path::PathBuf;

use crate::spec::{LifecycleWorkloadKind, LifecycleWorkloadRef, RigSpec};
use crate::{
    check_groups_for_extension_workloads, env_provider_extensions_for_extension_workloads,
    extension_ids_for_workloads, extension_workload_inputs, required_component_id_for_workload,
    required_extension_ids_for_workload, runner_capabilities_for_extension,
    trace_dependencies_for_extension, workload_lifecycle_contract,
    workload_path_expansions_for_extension, workloads_for_extension, RigWorkloadKind,
};

#[test]
fn workload_component_selection_preserves_command_policy() {
    let rig_spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "selection",
        "bench": {
            "default_component": "bench-default",
            "components": ["bench-first", "bench-second"]
        },
        "fuzz": { "default_component": "fuzz-default" },
        "trace": { "default_component": "trace-default" }
    }))
    .expect("parse rig spec");

    assert_eq!(
        required_component_id_for_workload(&rig_spec, RigWorkloadKind::Bench, Some("explicit"))
            .expect("explicit component"),
        "explicit"
    );
    assert_eq!(
        super::component_ids_for_workload(&rig_spec, RigWorkloadKind::Bench, None),
        vec!["bench-first", "bench-second"]
    );
    assert_eq!(
        required_component_id_for_workload(&rig_spec, RigWorkloadKind::Fuzz, None)
            .expect("fuzz default"),
        "fuzz-default"
    );
    assert!(super::component_ids_for_workload(&rig_spec, RigWorkloadKind::Trace, None).is_empty());
}

#[test]
fn missing_workload_component_keeps_command_specific_diagnostic() {
    let rig_spec: RigSpec =
        serde_json::from_value(serde_json::json!({ "id": "missing" })).expect("parse rig spec");

    let bench_error = required_component_id_for_workload(&rig_spec, RigWorkloadKind::Bench, None)
        .expect_err("bench component is required");
    assert!(bench_error.message.contains("bench.default_component"));
}

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
fn test_env_provider_extensions_for_extension_workloads_are_deduplicated() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "fixture-bench",
            "bench_workloads": {
                "fixture-runtime": [
                    {
                        "path": "/tmp/one.bench.mjs",
                        "env_provider_extensions": ["fixture-runtime", "fixture-runtime"]
                    },
                    {
                        "path": "/tmp/two.bench.mjs",
                        "env_provider_extensions": ["artifact-helper", ""]
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        env_provider_extensions_for_extension_workloads(
            &rig_spec,
            RigWorkloadKind::Bench,
            "fixture-runtime",
        ),
        vec!["artifact-helper".to_string(), "fixture-runtime".to_string()]
    );
}

#[test]
fn required_workload_extensions_include_environment_and_default_components() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "fixture-bench",
            "components": {
                "app": {
                    "path": "/tmp/app",
                    "extensions": { "component-helper": {}, "shared": {} }
                }
            },
            "bench": { "components": ["app"] },
            "bench_workloads": {
                "shared": [{
                    "path": "/tmp/one.bench.mjs",
                    "env_provider_extensions": ["environment-helper", "shared"]
                }]
            }
        }"#,
    )
    .expect("parse rig spec");

    assert_eq!(
        required_extension_ids_for_workload(&rig_spec, RigWorkloadKind::Bench, None),
        vec![
            "component-helper".to_string(),
            "environment-helper".to_string(),
            "shared".to_string(),
        ]
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

    let requirements = crate::invocation_requirements_for_extension_workloads(
        &rig_spec,
        crate::RigWorkloadKind::Bench,
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
fn test_fuzz_workloads_for_extension_filters_and_expands_paths() {
    std::env::set_var("HOMEBOY_TEST_FUZZ_ROOT", "/tmp/private-fuzz");
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "wordpress-plugin-fuzz",
            "components": {
                "woocommerce": { "path": "/tmp/woocommerce" }
            },
            "fuzz_workloads": {
                "wordpress": [
                    { "path": "${env.HOMEBOY_TEST_FUZZ_ROOT}/checkout-create-order.json" },
                    { "path": "${components.woocommerce.path}/fuzz/shipping-cache.json" }
                ],
                "other": [{ "path": "/tmp/other.json" }]
            }
        }"#,
    )
    .expect("parse rig spec");

    let workloads = workloads_for_extension(&rig_spec, RigWorkloadKind::Fuzz, None, "wordpress");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-fuzz/checkout-create-order.json"),
            PathBuf::from("/tmp/woocommerce/fuzz/shipping-cache.json"),
        ]
    );
    assert!(workloads_for_extension(&rig_spec, RigWorkloadKind::Fuzz, None, "missing").is_empty());
}

#[test]
fn test_fuzz_workload_inputs_include_paths_and_invocation_requirements() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "plugin-fuzz",
            "fuzz_workloads": {
                "extension-a": [
                    {
                        "path": "${package.root}/fuzz/parser.json",
                        "port_range_size": 4,
                        "named_leases": ["browser-profile"]
                    },
                    {
                        "path": "${package.root}/fuzz/rest.json",
                        "port_range_size": 2,
                        "named_leases": ["browser-profile", "network-proxy"]
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/plugin-fuzz");

    let inputs = extension_workload_inputs(
        &rig_spec,
        RigWorkloadKind::Fuzz,
        Some(&package),
        "extension-a",
    );

    assert_eq!(
        inputs.workload_paths,
        vec![
            PathBuf::from("/tmp/homeboy-rigs/plugin-fuzz/fuzz/parser.json"),
            PathBuf::from("/tmp/homeboy-rigs/plugin-fuzz/fuzz/rest.json"),
        ]
    );
    assert_eq!(inputs.invocation_requirements.port_range_size, Some(4));
    assert_eq!(
        inputs.invocation_requirements.named_leases,
        vec!["browser-profile".to_string(), "network-proxy".to_string()]
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
            },
            "fuzz_workloads": {
                "extension-a": [{ "path": "${package.root}/fuzz/studio-agent-runtime.json" }]
            }
        }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/example-org/studio");

    assert_eq!(
        workloads_for_extension(
            &rig_spec,
            RigWorkloadKind::Bench,
            Some(&package),
            "extension-a"
        ),
        vec![PathBuf::from(
            "/tmp/homeboy-rigs/example-org/studio/bench/studio-agent-runtime.bench.mjs"
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
            "/tmp/homeboy-rigs/example-org/studio/bench/studio-app-create-site.trace.mjs"
        )]
    );
    assert_eq!(
        workloads_for_extension(
            &rig_spec,
            RigWorkloadKind::Fuzz,
            Some(&package),
            "extension-a"
        ),
        vec![PathBuf::from(
            "/tmp/homeboy-rigs/example-org/studio/fuzz/studio-agent-runtime.json"
        )]
    );
}

#[test]
fn test_workload_path_expansions_preserve_declared_and_expanded_paths() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "wc-stripe-trace",
            "components": {
                "stripe": { "path": "/tmp/woocommerce-gateway-stripe" }
            },
            "trace_workloads": {
                "extension-a": [
                    { "path": "${components.stripe.path}/bench/ece-product-page-waterfall.trace.mjs" }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/wc-stripe-trace");

    let expansions = workload_path_expansions_for_extension(
        &rig_spec,
        RigWorkloadKind::Trace,
        Some(&package),
        "extension-a",
    );

    assert_eq!(expansions.len(), 1);
    assert_eq!(
        expansions[0].declared_path,
        "${components.stripe.path}/bench/ece-product-page-waterfall.trace.mjs"
    );
    assert_eq!(
        expansions[0].expanded_path,
        PathBuf::from("/tmp/woocommerce-gateway-stripe/bench/ece-product-page-waterfall.trace.mjs")
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

#[test]
fn test_trace_workload_dependencies_expand_paths_and_capabilities_dedupe() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "stripe-ece",
            "components": {
                "woocommerce": { "path": "/tmp/woocommerce-package" }
            },
            "trace_workloads": {
                "extension-a": [
                    {
                        "path": "/tmp/ece.trace.mjs",
                        "dependencies": [
                            {
                                "id": "sample-package",
                                "kind": "package",
                                "source": "release-package-or-build-artifact",
                                "path": "${components.woocommerce.path}",
                                "plugin_file": "package/entrypoint.txt",
                                "requires_built_assets": true
                            }
                        ],
                        "runner_capabilities": [
                            "sample-runtime.recipe-run",
                            "browser-probe.assertions"
                        ]
                    },
                    {
                        "path": "/tmp/ece-second.trace.mjs",
                        "runner_capabilities": ["sample-runtime.recipe-run"]
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec");

    let dependencies = trace_dependencies_for_extension(&rig_spec, None, "extension-a");
    assert_eq!(dependencies.len(), 1);
    assert_eq!(
        dependencies[0].path.as_deref(),
        Some("/tmp/woocommerce-package")
    );
    assert_eq!(
        runner_capabilities_for_extension(&rig_spec, "extension-a"),
        vec![
            "browser-probe.assertions".to_string(),
            "sample-runtime.recipe-run".to_string()
        ]
    );
}

// ------------------------------------------------------------------
// `WorkloadSpec.lifecycle` resolution (#10317)
//
// Before this reader existed the field was parsed, defaulted in four test
// constructors, serialized — and never read by anything. These tests pin the
// resolution contract, which is fail-closed in every direction.
// ------------------------------------------------------------------

fn workload_ref(
    kind: LifecycleWorkloadKind,
    extension: &str,
    path: Option<&str>,
) -> LifecycleWorkloadRef {
    LifecycleWorkloadRef {
        kind,
        extension: extension.to_string(),
        path: path.map(str::to_string),
    }
}

fn lifecycle_rig() -> RigSpec {
    serde_json::from_str(
        r#"{
            "id": "workload-lifecycle",
            "fuzz_workloads": {
                "generic": [
                    { "path": "fuzz/plain.workload.json" },
                    {
                        "path": "fuzz/sandboxed.workload.json",
                        "lifecycle": {
                            "phases": [
                                { "id": "make", "phase": "prepare", "extension_hook": "sandbox-runtime.create" },
                                { "id": "reap", "phase": "teardown", "extension_hook": "sandbox-runtime.destroy" }
                            ]
                        }
                    }
                ]
            },
            "bench_workloads": {
                "runner": [
                    {
                        "path": "bench/a.mjs",
                        "lifecycle": { "phases": [{ "id": "a", "phase": "prepare", "command": "true" }] }
                    },
                    {
                        "path": "bench/b.mjs",
                        "lifecycle": { "phases": [{ "id": "b", "phase": "prepare", "command": "true" }] }
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig spec")
}

#[test]
fn workload_lifecycle_contract_resolves_the_only_declaring_workload() {
    let rig_spec = lifecycle_rig();

    let contract = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(LifecycleWorkloadKind::Fuzz, "generic", None),
    )
    .expect("resolves the single workload carrying a contract");

    assert_eq!(contract.schema, "homeboy/lifecycle-contract/v1");
    assert_eq!(contract.phases.len(), 2);
    assert_eq!(
        contract.phases[0].extension_hook.as_deref(),
        Some("sandbox-runtime.create")
    );
}

#[test]
fn workload_lifecycle_contract_selects_by_declared_path() {
    let rig_spec = lifecycle_rig();

    let contract = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(LifecycleWorkloadKind::Bench, "runner", Some("bench/b.mjs")),
    )
    .expect("resolves by path");

    assert_eq!(contract.phases[0].id, "b");
}

#[test]
fn workload_lifecycle_contract_refuses_to_guess_between_candidates() {
    let rig_spec = lifecycle_rig();

    let error = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(LifecycleWorkloadKind::Bench, "runner", None),
    )
    .expect_err("two candidates is ambiguous");

    let message = error.to_string();
    assert!(message.contains("workload.path"), "{message}");
    assert!(message.contains("bench/a.mjs"), "{message}");
    assert!(message.contains("bench/b.mjs"), "{message}");
}

#[test]
fn workload_lifecycle_contract_rejects_a_workload_without_a_contract() {
    let rig_spec = lifecycle_rig();

    let error = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(
            LifecycleWorkloadKind::Fuzz,
            "generic",
            Some("fuzz/plain.workload.json"),
        ),
    )
    .expect_err("an explicitly selected workload with no contract is an error, not a no-op");

    assert!(
        error.to_string().contains("declares no lifecycle contract"),
        "{error}"
    );
}

#[test]
fn workload_lifecycle_contract_rejects_unknown_extension_and_path() {
    let rig_spec = lifecycle_rig();

    let unknown_extension = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(LifecycleWorkloadKind::Fuzz, "nope", None),
    )
    .expect_err("unknown extension");
    assert!(
        unknown_extension
            .to_string()
            .contains("declared extensions"),
        "{unknown_extension}"
    );

    let unknown_path = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(
            LifecycleWorkloadKind::Fuzz,
            "generic",
            Some("fuzz/nope.json"),
        ),
    )
    .expect_err("unknown path");
    assert!(
        unknown_path.to_string().contains("declared paths"),
        "{unknown_path}"
    );
}

#[test]
fn workload_lifecycle_contract_reports_an_empty_map_without_panicking() {
    let rig_spec: RigSpec = serde_json::from_str(r#"{ "id": "empty" }"#).expect("parse rig spec");

    let error = workload_lifecycle_contract(
        &rig_spec,
        &workload_ref(LifecycleWorkloadKind::Trace, "runner", None),
    )
    .expect_err("no trace_workloads at all");

    assert!(error.to_string().contains("trace_workloads"), "{error}");
}
