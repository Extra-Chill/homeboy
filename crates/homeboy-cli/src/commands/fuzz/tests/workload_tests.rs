use super::*;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn selected_rig_workload_projects_plannable_inventory_without_inventory_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workload_path = temp.path().join("component-fuzz.json");
    fs::write(
        &workload_path,
        serde_json::json!({
            "schema": "homeboy/fuzz-workload/v1",
            "id": "component-fuzz",
            "safety_class": "read_only",
            "surface_ids": ["component-runtime"],
            "operations": ["query", "domain.verify"],
            "target": {
                "type": "service",
                "component": "component-a",
                "slug": "component-a"
            }
        })
        .to_string(),
    )
    .expect("write workload");
    let workload = FuzzWorkloadOutput {
        id: "component-fuzz".to_string(),
        label: None,
        description: None,
        source: format!("rig_workloads:generic:{}", workload_path.display()),
        manifest_path: Some(workload_path.to_string_lossy().to_string()),
    };

    let inventory = build_target_inventory(
        "component-a",
        std::slice::from_ref(&workload),
        Some(&workload),
        Some("proof-1".to_string()),
        None,
    )
    .expect("project workload inventory");

    assert_eq!(inventory.workloads.len(), 1);
    assert_eq!(inventory.workloads[0].id, "component-fuzz");
    assert_eq!(inventory.targets.len(), 1);
    assert_eq!(inventory.targets[0].id, "component-a");
    assert_eq!(inventory.targets[0].kind, "service");
    assert_eq!(inventory.targets[0].operations.len(), 2);
    assert_eq!(inventory.surfaces[0].id, "component-runtime");
    assert_eq!(
        inventory.metadata["workload_inventory_source"],
        workload_path.to_string_lossy().as_ref()
    );

    let mut args = planner_args();
    args.run.workload_id = Some("component-fuzz".to_string());
    let metadata = plan_inventory_selection(&args, &inventory, None).expect("plan inventory");

    assert_eq!(
        metadata["selection"]["target_ids"],
        serde_json::json!(["component-a"])
    );
    assert_eq!(
        metadata["selection"]["operations"]
            .as_array()
            .expect("selected operations")
            .len(),
        2
    );
    assert_eq!(
        metadata["sampling"]["operation_strata"][1]["values"],
        serde_json::json!(["query", "domain.verify"])
    );

    args.strategy = FuzzPlanStrategy::CoverageGaps;
    let coverage_gaps =
        plan_inventory_selection(&args, &inventory, None).expect("plan coverage gaps");
    assert!(coverage_gaps["selection"]["operations"]
        .as_array()
        .expect("selected operations")
        .iter()
        .any(|operation| operation["operation_id"] == "domain.verify"));

    let explicit_path = temp.path().join("explicit-inventory.json");
    let explicit_inventory = FuzzTargetInventory::from_value(serde_json::json!({
        "id": "explicit-inventory",
        "targets": [{
            "id": "explicit-target",
            "kind": "service",
            "operations": [{ "id": "read", "kind": "read" }]
        }]
    }))
    .expect("explicit inventory");
    write_inventory(&explicit_path, &explicit_inventory);
    let inventory = build_target_inventory(
        "component-a",
        &[workload.clone()],
        Some(&workload),
        Some("proof-2".to_string()),
        Some(&explicit_path),
    )
    .expect("use explicit inventory");

    assert_eq!(inventory.targets.len(), 1);
    assert_eq!(inventory.targets[0].id, "explicit-target");
    assert!(inventory.workloads.is_empty());
    assert!(inventory
        .metadata
        .get("workload_inventory_source")
        .is_none());
}

#[test]
fn fuzz_workloads_include_rig_declared_paths() {
    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "package-fuzz",
        "components": {
            "package": {
                "path": "/tmp/package",
                "extensions": {
                    "generic": {
                        "settings": {}
                    }
                }
            }
        },
        "fuzz": {
            "default_component": "package"
        },
        "fuzz_workloads": {
            "generic": [
                { "path": "${package.root}/fuzz/checkout-create-order.json" }
            ]
        }
    }))
    .expect("parse rig spec");
    let component = rig_component_for_fuzz(&spec, "package").expect("rig component");
    let context = FuzzRigContext {
        spec,
        package_root: Some(std::path::PathBuf::from("/tmp/homeboy-rigs/package")),
    };

    let workloads = fuzz_workloads(&component, Some(&context), Some("generic"));

    assert!(workloads.iter().any(|workload| {
        workload.id == "checkout-create-order"
            && workload.manifest_path.as_deref()
                == Some("/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json")
            && workload.source
                == "rig_workloads:generic:/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json"
    }));
}

#[test]
fn node_package_fuzz_script_is_listed_and_selected_for_run() {
    with_isolated_home(|home| {
        let extension_dir = home.path().join(".config/homeboy/extensions/nodejs");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("nodejs.json"),
            serde_json::json!({
                "name": "Node.js",
                "version": "0.0.0",
                "fuzz": {
                    "extension_script": "fuzz.sh",
                    "workload_json_probes": [{
                        "path": "package.json",
                        "pointer": "/scripts/fuzz",
                        "id": "package-script-fuzz",
                        "label": "package.json scripts.fuzz"
                    }]
                }
            })
            .to_string(),
        )
        .expect("extension manifest");
        let component_dir = tempfile::tempdir().expect("component dir");
        fs::write(
            component_dir.path().join("package.json"),
            serde_json::json!({ "scripts": { "fuzz": "node scripts/fuzz.mjs" } }).to_string(),
        )
        .expect("package manifest");
        let args = FuzzListArgs {
            comp: PositionalComponentArgs {
                component: Some("node-package".to_string()),
                path: Some(component_dir.path().to_string_lossy().to_string()),
            },
            rig: None,
            remote_discovery: false,
            extension_override: ExtensionOverrideArgs {
                extensions: vec!["nodejs".to_string()],
            },
            setting_args: SettingArgs::default(),
        };

        let listed = run_list(args).expect("list package fuzz script");

        assert_eq!(listed.count, 1);
        assert_eq!(listed.workloads[0].id, "package-script-fuzz");
        assert_eq!(
            select_workload(&listed.workloads, None)
                .expect("select sole package workload")
                .expect("package workload")
                .id,
            "package-script-fuzz"
        );
    });
}

#[test]
fn resolve_component_id_uses_fuzz_default_component() {
    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "package-fuzz",
        "fuzz": {
            "default_component": "package"
        }
    }))
    .expect("parse rig spec");
    let comp = PositionalComponentArgs {
        component: None,
        path: None,
    };

    assert_eq!(
        resolve_component_id(&comp, Some(&spec)).expect("resolve component"),
        "package"
    );
}

#[test]
fn resolve_fuzz_context_preserves_rig_extensions_with_explicit_path_when_registry_missing() {
    with_isolated_home(|home| {
        let extension_dir = home.path().join(".config/homeboy/extensions/generic");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("generic.json"),
            serde_json::json!({
                "name": "generic",
                "version": "0.0.0",
                "fuzz": {
                    "workloads": [{ "id": "fixture" }]
                }
            })
            .to_string(),
        )
        .expect("extension manifest");
        let component_dir = tempfile::tempdir().expect("component dir");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "${env.HOMEBOY_EMPTY_PACKAGE_PATH}",
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "fuzz": {
                "default_component": "package"
            }
        }))
        .expect("parse rig spec");
        unsafe {
            std::env::remove_var("HOMEBOY_EMPTY_PACKAGE_PATH");
        }
        let context = FuzzRigContext {
            spec,
            package_root: None,
        };
        let comp = PositionalComponentArgs {
            component: Some("package".to_string()),
            path: Some(component_dir.path().to_string_lossy().to_string()),
        };

        let resolved = resolve_fuzz_context(
            "package",
            &comp,
            &SettingArgs::default(),
            &ExtensionOverrideArgs::default(),
            homeboy_extension_contract::ExtensionCapability::Fuzz,
            Some(&context),
        )
        .expect("resolve fuzz context");

        assert_eq!(
            resolved.component.local_path,
            component_dir.path().to_string_lossy()
        );
        assert!(resolved
            .component
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key("generic")));
    });
}

#[test]
fn resolve_fuzz_context_infers_single_rig_fuzz_workload_extension() {
    with_isolated_home(|home| {
        let extension_dir = home.path().join(".config/homeboy/extensions/generic");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("generic.json"),
            serde_json::json!({
                "name": "generic",
                "version": "0.0.0",
                "fuzz": { "workloads": [] }
            })
            .to_string(),
        )
        .expect("extension manifest");
        let component_dir = tempfile::tempdir().expect("component dir");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": component_dir.path().to_string_lossy(),
                    "extensions": {
                        "generic": { "settings": {} }
                    }
                }
            },
            "fuzz": { "default_component": "package" },
            "fuzz_workloads": {
                "generic": [{ "path": "${package.root}/fuzz/parser.json" }]
            }
        }))
        .expect("parse rig spec");
        let context = FuzzRigContext {
            spec,
            package_root: Some(home.path().join("rig-package")),
        };
        let comp = PositionalComponentArgs {
            component: Some("package".to_string()),
            path: None,
        };

        let resolved = resolve_fuzz_context(
            "package",
            &comp,
            &SettingArgs::default(),
            &ExtensionOverrideArgs::default(),
            homeboy_extension_contract::ExtensionCapability::Fuzz,
            Some(&context),
        )
        .expect("resolve fuzz context");

        assert_eq!(resolved.extension_id.as_deref(), Some("generic"));
    });
}

#[test]
fn resolve_fuzz_context_reports_explicit_extension_without_fuzz_capability() {
    with_isolated_home(|home| {
        let extension_dir = home.path().join(".config/homeboy/extensions/generic");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("generic.json"),
            serde_json::json!({
                "name": "generic",
                "version": "0.0.0",
                "test": { "extension_script": "test.sh" }
            })
            .to_string(),
        )
        .expect("extension manifest");
        let component_dir = tempfile::tempdir().expect("component dir");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": component_dir.path().to_string_lossy()
                }
            },
            "fuzz": { "default_component": "package" },
            "fuzz_workloads": {
                "generic": [{ "path": "fuzz/parser.json" }]
            }
        }))
        .expect("parse rig spec");
        let context = FuzzRigContext {
            spec,
            package_root: Some(home.path().join("rig-package")),
        };
        let comp = PositionalComponentArgs {
            component: Some("package".to_string()),
            path: None,
        };
        let extension_override = ExtensionOverrideArgs {
            extensions: vec!["generic".to_string()],
        };

        let err = resolve_fuzz_context(
            "package",
            &comp,
            &SettingArgs::default(),
            &extension_override,
            homeboy_extension_contract::ExtensionCapability::Fuzz,
            Some(&context),
        )
        .expect_err("old extension manifest should fail explicitly");

        assert!(err.message.contains(
            "explicit extension override 'generic' is installed but does not declare fuzz support"
        ));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Upgrade or refresh the runner extension")));
    });
}

#[test]
fn resolve_profile_workload_id_resolves_single_entry_profiles_only() {
    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "package-fuzz",
        "fuzz_profiles": {
            "lab": ["mysql-poc-lifecycle-fuzz"]
        }
    }))
    .expect("parse rig spec");

    assert_eq!(
        resolve_profile_workload_id(Some(&spec), Some("lab"), None)
            .expect("resolve profile workload")
            .as_deref(),
        Some("mysql-poc-lifecycle-fuzz")
    );

    let err = resolve_profile_workload_id(Some(&spec), Some("missing"), None)
        .expect_err("missing profile should fail");
    assert!(err
        .message
        .contains("does not declare fuzz profile 'missing'"));
    assert_eq!(err.details["tried"], serde_json::json!(["lab"]));

    // Only campaign expansion may combine a profile with a concrete workload.
    let err = resolve_profile_workload_id(None, Some("full-surface"), Some("db-fuzz"))
        .expect_err("profile + explicit workload must be rejected");
    assert!(err
        .message
        .contains("--profile and --workload cannot be combined"));
}

/// Listing is metadata-only: it must never run prepare steps or dependency
/// materialization, and their failures must not break the listing.
#[test]
fn fuzz_list_never_runs_prepare_or_dependency_materialization() {
    type WriteRig = fn(&Path, &Path);
    let cases: [(&str, WriteRig, &str); 5] = [
        (
            "slow prepare step",
            |home, component| {
                write_fuzz_prepare_rig(
                    home,
                    component,
                    "sleep 2; mkdir -p runtime && printf ready > runtime/ready.txt",
                )
            },
            "runtime/ready.txt",
        ),
        (
            "failing prepare step",
            |home, component| write_fuzz_prepare_rig(home, component, "false"),
            "runtime/ready.txt",
        ),
        (
            "dependency materialization",
            |home, component| {
                write_dependency_materialization_rig(
                    home,
                    component,
                    "mkdir -p runtime && printf ready > runtime/ready.txt",
                )
            },
            "runtime/ready.txt",
        ),
        (
            "failing dependency materialization",
            |home, component| write_dependency_materialization_rig(home, component, "true"),
            "runtime/ready.txt",
        ),
        (
            "plugin subpath dependency materialization",
            write_plugin_subpath_dependency_materialization_fuzz_rig,
            "plugins/woocommerce/vendor/autoload_packages.php",
        ),
    ];

    for (label, write_rig, materialized_output) in cases {
        with_isolated_home(|home| {
            let component_dir = home.path().join("component");
            fs::create_dir_all(component_dir.join("plugins/woocommerce")).expect("component dir");
            write_generic_fuzz_extension(home.path());
            write_rig(home.path(), &component_dir);

            let started = Instant::now();
            let output = run_list(fuzz_list_args()).expect("metadata listing should not prepare");

            assert_eq!(output.count, 1, "{label}");
            assert!(started.elapsed() < Duration::from_secs(1), "{label}");
            assert!(!component_dir.join(materialized_output).exists(), "{label}");
        });
    }
}

#[test]
fn fuzz_list_marks_forced_local_remote_discovery_as_requested() {
    with_isolated_home(|home| {
        let component_dir = home.path().join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        write_generic_fuzz_extension(home.path());
        write_fuzz_prepare_rig(home.path(), &component_dir, "false");
        let mut args = fuzz_list_args();
        args.remote_discovery = true;

        let output = run_list(args).expect("local metadata listing should not prepare");

        assert_eq!(
            output.diagnostics.executable_availability,
            "remote_discovery_requested"
        );
    });
}

#[test]
fn fuzz_list_reports_ambiguous_duplicate_rig_workloads_without_running_them() {
    with_isolated_home(|home| {
        let component_dir = home.path().join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        write_generic_fuzz_extension(home.path());
        let rig_dir = home.path().join(".config/homeboy/rigs");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("package-fuzz.json"),
            serde_json::json!({
                "id": "package-fuzz",
                "components": {
                    "package": {
                        "path": component_dir,
                        "extensions": { "generic": { "settings": {} } }
                    }
                },
                "fuzz": { "default_component": "package" },
                "fuzz_workloads": {
                    "generic": [
                        { "path": "fuzz/duplicate.json" },
                        { "path": "fuzz/duplicate.json" }
                    ]
                }
            })
            .to_string(),
        )
        .expect("rig config");

        let output = run_list(fuzz_list_args()).expect("fuzz list");

        assert_eq!(output.diagnostics.completeness, "ambiguous");
        assert!(output
            .diagnostics
            .ambiguity
            .iter()
            .any(|message| message.contains("duplicate declared fuzz workload id")));
    });
}

#[test]
fn fuzz_list_reports_stale_installed_rig_package_provenance() {
    with_isolated_home(|home| {
        write_generic_fuzz_extension(home.path());
        let component_dir = home.path().join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        let package = tempfile::tempdir().expect("rig package");
        let rig_dir = package.path().join("rigs/package-fuzz");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            serde_json::json!({
                "id": "package-fuzz",
                "components": {
                    "package": {
                        "path": component_dir,
                        "extensions": { "generic": { "settings": {} } }
                    }
                },
                "fuzz": { "default_component": "package" },
                "fuzz_workloads": { "generic": [{ "path": "fuzz/package.json" }] }
            })
            .to_string(),
        )
        .expect("rig config");
        for args in [
            vec!["init"],
            vec!["add", "."],
            vec![
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "initial",
            ],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(package.path())
                .status()
                .expect("run git");
            assert!(status.success(), "git command should succeed");
        }
        homeboy::rig::install(
            &homeboy::core::paths::homeboy().expect("config root"),
            package.path().to_string_lossy().as_ref(),
            Some("package-fuzz"),
            false,
        )
        .expect("install rig package");
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "advance source",
            ])
            .current_dir(package.path())
            .status()
            .expect("advance source revision");
        assert!(status.success());

        let output = run_list(fuzz_list_args()).expect("fuzz list");

        assert_eq!(output.diagnostics.completeness, "incomplete");
        assert!(output
            .diagnostics
            .ambiguity
            .iter()
            .any(|message| message.contains("rig package freshness is Stale")));
        assert!(output
            .rig_package
            .as_ref()
            .is_some_and(|package| !package.freshness_verified));
    });
}

fn fuzz_list_args() -> FuzzListArgs {
    FuzzListArgs {
        comp: PositionalComponentArgs {
            component: None,
            path: None,
        },
        rig: Some("package-fuzz".to_string()),
        remote_discovery: false,
        extension_override: ExtensionOverrideArgs::default(),
        setting_args: SettingArgs::default(),
    }
}

fn write_generic_fuzz_extension(home: &Path) {
    let extension_dir = home.join(".config/homeboy/extensions/generic");
    fs::create_dir_all(&extension_dir).expect("extension dir");
    fs::write(
        extension_dir.join("generic.json"),
        serde_json::json!({
            "name": "generic",
            "version": "0.0.0",
            "fuzz": {
                "workloads": [{ "id": "fixture" }]
            }
        })
        .to_string(),
    )
    .expect("extension manifest");
}

fn write_fuzz_prepare_rig(home: &Path, component_dir: &Path, prepare_command: &str) {
    let rig_dir = home.join(".config/homeboy/rigs");
    fs::create_dir_all(&rig_dir).expect("rig dir");
    fs::write(
        rig_dir.join("package-fuzz.json"),
        serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": component_dir.to_string_lossy(),
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "fuzz": {
                "default_component": "package"
            },
            "pipeline": {
                "fuzz_prepare": [
                    {
                        "kind": "requirement",
                        "id": "runtime-ready",
                        "file": "runtime/ready.txt",
                        "cwd": component_dir.to_string_lossy(),
                        "prepare_command": prepare_command,
                        "prepare_phases": ["fuzz_prepare"],
                        "label": "materialize runtime dependency"
                    }
                ]
            }
        })
        .to_string(),
    )
    .expect("rig config");
}

fn write_dependency_materialization_rig(home: &Path, component_dir: &Path, command: &str) {
    let rig_dir = home.join(".config/homeboy/rigs");
    fs::create_dir_all(&rig_dir).expect("rig dir");
    fs::write(
        rig_dir.join("package-fuzz.json"),
        serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": component_dir.to_string_lossy(),
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "requirements": {
                "dependency_materialization": [
                    {
                        "id": "runtime-dependencies",
                        "command": command,
                        "component": "package",
                        "cwd": "${components.package.path}",
                        "expected_outputs": [
                            { "path": "runtime/ready.txt", "kind": "file" }
                        ],
                        "safety": "writes_working_tree"
                    }
                ]
            },
            "fuzz": {
                "default_component": "package"
            },
            "pipeline": {
                "fuzz_prepare": [
                    {
                        "kind": "requirement",
                        "id": "runtime-ready",
                        "file": "runtime/ready.txt",
                        "cwd": component_dir.to_string_lossy(),
                        "label": "runtime dependency is present"
                    }
                ]
            }
        })
        .to_string(),
    )
    .expect("rig config");
}

fn write_plugin_subpath_dependency_materialization_fuzz_rig(home: &Path, component_dir: &Path) {
    let rig_dir = home.join(".config/homeboy/rigs");
    fs::create_dir_all(&rig_dir).expect("rig dir");
    fs::write(
        rig_dir.join("package-fuzz.json"),
        serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": component_dir.to_string_lossy(),
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "requirements": {
                "dependency_materialization": [
                    {
                        "id": "woocommerce-php-package-dependencies",
                        "command": "mkdir -p plugins/woocommerce/vendor && printf ready > plugins/woocommerce/vendor/autoload_packages.php",
                        "component": "package",
                        "cwd": "${components.package.path}",
                        "inputs": {
                            "recipe": "wordpress-plugin-php-package-dependencies",
                            "manifest": "${components.package.path}/plugins/woocommerce/composer.json",
                            "lockfile": "${components.package.path}/plugins/woocommerce/composer.lock"
                        },
                        "expected_outputs": [
                            {
                                "path": "${components.package.path}/plugins/woocommerce/vendor/autoload_packages.php",
                                "kind": "file"
                            }
                        ],
                        "cache_key_inputs": [
                            "${components.package.path}/plugins/woocommerce/composer.json",
                            "${components.package.path}/plugins/woocommerce/composer.lock"
                        ],
                        "safety": "writes_working_tree"
                    }
                ]
            },
            "fuzz": {
                "default_component": "package"
            },
            "pipeline": {
                "fuzz_prepare": [
                    {
                        "kind": "requirement",
                        "id": "woocommerce-autoloader-ready",
                        "file": "${components.package.path}/plugins/woocommerce/vendor/autoload_packages.php",
                        "label": "WooCommerce Composer package autoloader exists or can be prepared"
                    }
                ]
            }
        })
        .to_string(),
    )
    .expect("rig config");
}

#[test]
fn fuzz_runner_env_includes_results_file_selected_workload_path_and_generic_contract() {
    with_isolated_home(|_| {
        let mut args = planner_args().run;
        args.workload_id = Some("parser".to_string());
        args.shared_state = Some(PathBuf::from("/tmp/fuzz-shared-state"));
        args.seed = Some("1234".to_string());
        args.inventory = Some(PathBuf::from("/tmp/fuzz-inventory.json"));
        args.max_duration = Some("60s".to_string());
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: "rig_workloads:generic:/tmp/fuzz/parser.json".to_string(),
            manifest_path: Some("/tmp/fuzz/parser.json".to_string()),
        };

        let run_dir = RunDir::create().expect("run dir");
        let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
        let execution_request_path =
            run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_EXECUTION_REQUEST);
        let artifacts_dir =
            run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_ARTIFACTS_DIR);

        let env = fuzz_runner_env(
            &args,
            None,
            Some(&workload),
            &results_path,
            &run_dir,
            Some(&execution_request_path),
            None,
        )
        .expect("fuzz runner env");

        for (key, value) in [
            ("HOMEBOY_FUZZ_RESULTS_FILE", results_path.to_string_lossy()),
            (
                "HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE",
                execution_request_path.to_string_lossy(),
            ),
            (
                "HOMEBOY_FUZZ_ARTIFACTS_DIR",
                artifacts_dir.to_string_lossy(),
            ),
            ("HOMEBOY_FUZZ_WORKLOAD_ID", "parser".into()),
            ("HOMEBOY_FUZZ_WORKLOAD_PATH", "/tmp/fuzz/parser.json".into()),
            ("HOMEBOY_FUZZ_RUN_ID", "proof-1".into()),
            ("HOMEBOY_FUZZ_SEED", "1234".into()),
            (
                "HOMEBOY_FUZZ_INVENTORY_FILE",
                "/tmp/fuzz-inventory.json".into(),
            ),
            ("HOMEBOY_FUZZ_SHARED_STATE", "/tmp/fuzz-shared-state".into()),
            ("HOMEBOY_FUZZ_MAX_DURATION", "60s".into()),
            ("HOMEBOY_FUZZ_GATE_PROFILE", "measurement".into()),
            ("HOMEBOY_FUZZ_ALLOW_DESTRUCTIVE", "false".into()),
            ("HOMEBOY_FUZZ_ISOLATION", "shared".into()),
        ] {
            assert!(
                env.contains(&(key.to_string(), value.to_string())),
                "missing {key}={value} in {env:?}"
            );
            assert!(
                default_runner_contract().env.iter().any(|name| name == key),
                "runner contract must advertise {key}"
            );
        }
        assert!(artifacts_dir.is_dir());
    });
}

#[test]
fn fuzz_runner_contract_includes_extension_declared_env_settings() {
    let config = FuzzConfig {
        extension_script: Some("fuzz.sh".to_string()),
        env: vec![
            "HOMEBOY_SETTINGS_JSON".to_string(),
            "HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN".to_string(),
            "HOMEBOY_SAMPLE_RUNTIME_BIN".to_string(),
            "SAMPLE_RUNTIME_BIN".to_string(),
            "HOMEBOY_FUZZ_WORKLOAD_ID".to_string(),
        ],
        ..FuzzConfig::default()
    };

    let contract = serde_json::to_value(fuzz_runner_contract(Some(&config)))
        .expect("serialize runner contract");
    let env = contract["env"].as_array().expect("runner contract env");

    for key in [
        "HOMEBOY_FUZZ_RESULTS_FILE",
        "HOMEBOY_FUZZ_WORKLOAD_ID",
        "HOMEBOY_SETTINGS_JSON",
        "HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN",
        "HOMEBOY_SAMPLE_RUNTIME_BIN",
        "SAMPLE_RUNTIME_BIN",
    ] {
        assert!(
            env.iter().any(|value| value == key),
            "runner contract env should include {key}"
        );
    }
    assert_eq!(
        env.iter()
            .filter(|value| **value == "HOMEBOY_FUZZ_WORKLOAD_ID")
            .count(),
        1
    );
}

fn rig_workload_env_fixture(
    package_root: &Path,
    workload_id: &str,
    workload_path: &Path,
) -> Vec<(String, String)> {
    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "package-fuzz",
        "components": {
            "package": {
                "path": "${package.root}/plugins/package",
                "branch": "main"
            }
        }
    }))
    .expect("parse rig spec");
    let context = FuzzRigContext {
        spec,
        package_root: Some(package_root.to_path_buf()),
    };
    let mut args = planner_args().run;
    args.comp.component = Some("package".to_string());
    args.rig = Some("package-fuzz".to_string());
    args.workload_id = Some(workload_id.to_string());
    let workload = FuzzWorkloadOutput {
        id: workload_id.to_string(),
        label: None,
        description: None,
        source: format!("rig_workloads:generic:{}", workload_path.display()),
        manifest_path: Some(workload_path.to_string_lossy().to_string()),
    };
    let run_dir = RunDir::create().expect("run dir");
    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);

    fuzz_runner_env(
        &args,
        Some(&context),
        Some(&workload),
        &results_path,
        &run_dir,
        None,
        None,
    )
    .expect("fuzz runner env")
}

fn expanded_workload(env: &[(String, String)]) -> serde_json::Value {
    let expanded_path = env
        .iter()
        .find_map(|(key, value)| (key == "HOMEBOY_FUZZ_WORKLOAD_PATH").then_some(value))
        .expect("expanded workload path");
    serde_json::from_str(&std::fs::read_to_string(expanded_path).expect("read expanded workload"))
        .expect("parse expanded workload")
}

#[test]
fn fuzz_runner_env_expands_rig_workload_and_injects_runtime_context() {
    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workload_path = temp.path().join("parser.json");
        std::fs::write(
            &workload_path,
            r#"{
          "schema": "homeboy/fuzz-workload/v1",
          "id": "parser",
          "target": { "component": "package" },
          "workload": { "path": "${package.root}/bench/parser.input" },
          "metadata": { "fixture": { "component": "package" } }
        }"#,
        )
        .expect("write workload");
        let override_env =
            homeboy::rig::expand::rig_component_path_override_env_name("package-fuzz", "package");
        let override_path = temp.path().join("runner/plugins/package");
        unsafe {
            std::env::set_var(&override_env, override_path.to_string_lossy().to_string());
        }

        let env = rig_workload_env_fixture(temp.path(), "parser", &workload_path);
        unsafe {
            std::env::remove_var(&override_env);
        }
        let expanded = expanded_workload(&env);

        assert_eq!(
            expanded["workload"]["path"].as_str(),
            Some(format!("{}/bench/parser.input", temp.path().display()).as_str())
        );
        assert_eq!(
            expanded["metadata"]["homeboy_runtime_context"]["components"]["package"]["path"]
                .as_str(),
            Some(override_path.to_string_lossy().as_ref())
        );
        assert!(env.iter().any(|(key, value)| {
            key == "HOMEBOY_FUZZ_WORKLOAD_ROOT" && value == &temp.path().to_string_lossy()
        }));
    });
}

/// File refs in workload step args are staged to the declared target root,
/// both directly and inside nested `workload-json=` payloads.
#[test]
fn fuzz_runner_env_stages_direct_and_nested_workload_file_refs() {
    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("bench")).expect("create bench dir");
        let php_path = temp.path().join("bench/rest-product-batch-import.php");
        std::fs::write(&php_path, "<?php return array( 'ok' => true );\n")
            .expect("write php workload");
        let workload_run = |arg: String| {
            serde_json::json!({
                "schema": "example/fuzz-workload-run/v1",
                "file_staging": {
                    "step_fields": ["steps"],
                    "path_arg_prefixes": ["path="],
                    "nested_json_arg_prefixes": ["workload-json="],
                    "target_root": "/tmp/homeboy-fuzz-workloads",
                    "file_extensions": ["php", "json", "mjs", "js"]
                },
                "steps": [{ "command": "example.run-workload", "args": [arg] }]
            })
        };
        let direct =
            workload_run("path=${package.root}/bench/rest-product-batch-import.php".to_string());
        let nested = workload_run(format!(
            "workload-json={}",
            workload_run(format!("path={}", php_path.display()))
        ));

        for (label, workload, is_nested) in [("direct", direct, false), ("nested", nested, true)] {
            let workload_path = temp.path().join(format!("{label}.workload.json"));
            std::fs::write(&workload_path, workload.to_string()).expect("write workload");

            let env =
                rig_workload_env_fixture(temp.path(), "rest-product-batch-import", &workload_path);
            let mut staged = expanded_workload(&env);
            if is_nested {
                let nested_arg = staged["steps"][0]["args"][0]
                    .as_str()
                    .expect("nested workload arg")
                    .strip_prefix("workload-json=")
                    .expect("workload-json prefix")
                    .to_string();
                staged = serde_json::from_str(&nested_arg).expect("nested json");
            }

            assert_eq!(
                staged["steps"][0]["args"][0].as_str(),
                Some("path=/tmp/homeboy-fuzz-workloads/rest-product-batch-import.php"),
                "{label}"
            );
            assert_eq!(
                staged["staged_files"][0]["source"].as_str(),
                Some(php_path.to_string_lossy().as_ref()),
                "{label}"
            );
            assert_eq!(
                staged["staged_files"][0]["target"].as_str(),
                Some("/tmp/homeboy-fuzz-workloads/rest-product-batch-import.php"),
                "{label}"
            );
        }
    });
}
