use super::*;

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
fn fuzz_workload_parses_artifact_postprocess_metadata() {
    let spec: RigSpec = serde_json::from_value(serde_json::json!({
        "id": "package-fuzz",
        "fuzz_workloads": {
            "generic": [
                {
                    "path": "fuzz/parser.json",
                    "artifact_postprocess": [
                        {
                            "id": "coverage-summary",
                            "helper": "fuzz-helper",
                            "action": "summarize-coverage",
                            "input": "${run.fuzz_results}",
                            "output": "coverage/summary.json",
                            "parameters": {
                                "format": "json",
                                "threshold": 1
                            }
                        }
                    ]
                }
            ]
        }
    }))
    .expect("parse rig spec");

    let postprocess = spec.fuzz_workloads["generic"][0].artifact_postprocess();

    assert_eq!(postprocess.len(), 1);
    assert_eq!(postprocess[0].id.as_deref(), Some("coverage-summary"));
    assert_eq!(postprocess[0].helper, "fuzz-helper");
    assert_eq!(postprocess[0].action, "summarize-coverage");
    assert_eq!(postprocess[0].input.as_deref(), Some("${run.fuzz_results}"));
    assert_eq!(postprocess[0].output, "coverage/summary.json");
    assert!(postprocess[0].required);
    assert_eq!(postprocess[0].parameters["format"], "json");
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
fn fuzz_runner_env_includes_results_file_selected_workload_path_and_generic_contract() {
    let args = FuzzRunArgs {
        comp: PositionalComponentArgs {
            component: Some("component-a".to_string()),
            path: None,
        },
        rig: None,
        extension_override: ExtensionOverrideArgs { extensions: vec![] },
        setting_args: SettingArgs {
            setting: vec![],
            setting_json: vec![],
        },
        workload_id: Some("parser".to_string()),
        run_id: Some("proof-1".to_string()),
        tracker_refs: vec![],
        seed: Some("1234".to_string()),
        inventory: Some(PathBuf::from("/tmp/fuzz-inventory.json")),
        require_case_log: false,
        require_coverage_summary: false,
        require_result_envelope: false,
        max_duration: Some("60s".to_string()),
        gate_profile: FuzzGateProfileArg::Measurement,
        allow_destructive: false,
        isolation: FuzzIsolationArg::Shared,
        expect_metric: vec![],
        args: vec![],
    };
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

    let env = fuzz_runner_env(
        &args,
        None,
        Some(&workload),
        &results_path,
        &run_dir,
        Some(&execution_request_path),
    )
    .expect("fuzz runner env");

    assert!(env.contains(&(
        "HOMEBOY_FUZZ_RESULTS_FILE".to_string(),
        results_path.to_string_lossy().to_string()
    )));
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE".to_string(),
        execution_request_path.to_string_lossy().to_string()
    )));
    let artifacts_dir =
        run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_ARTIFACTS_DIR);
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_ARTIFACTS_DIR".to_string(),
        artifacts_dir.to_string_lossy().to_string()
    )));
    assert!(artifacts_dir.is_dir());
    assert!(env.contains(&("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), "parser".to_string())));
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(),
        "/tmp/fuzz/parser.json".to_string()
    )));
    assert!(env.contains(&("HOMEBOY_FUZZ_RUN_ID".to_string(), "proof-1".to_string())));
    assert!(env.contains(&("HOMEBOY_FUZZ_SEED".to_string(), "1234".to_string())));
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_INVENTORY_FILE".to_string(),
        "/tmp/fuzz-inventory.json".to_string()
    )));
    assert!(env.contains(&("HOMEBOY_FUZZ_MAX_DURATION".to_string(), "60s".to_string())));
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_GATE_PROFILE".to_string(),
        "measurement".to_string()
    )));
    assert!(env.contains(&(
        "HOMEBOY_FUZZ_ALLOW_DESTRUCTIVE".to_string(),
        "false".to_string()
    )));
    assert!(env.contains(&("HOMEBOY_FUZZ_ISOLATION".to_string(), "shared".to_string())));
    let contract = default_runner_contract();
    assert!(contract
        .env
        .contains(&"HOMEBOY_FUZZ_WORKLOAD_ROOT".to_string()));
    assert!(contract
        .env
        .contains(&"HOMEBOY_FUZZ_ARTIFACTS_DIR".to_string()));
    assert!(contract
        .env
        .contains(&"HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE".to_string()));
    assert!(contract
        .env
        .contains(&"HOMEBOY_FUZZ_GATE_PROFILE".to_string()));
    assert!(contract
        .env
        .contains(&"HOMEBOY_FUZZ_ALLOW_DESTRUCTIVE".to_string()));
    assert!(contract.env.contains(&"HOMEBOY_FUZZ_ISOLATION".to_string()));
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

#[test]
fn fuzz_runner_env_expands_rig_workload_and_injects_runtime_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workload_path = temp.path().join("parser.json");
    std::fs::write(
        &workload_path,
        r#"{
          "schema": "homeboy/fuzz-workload/v1",
          "id": "parser",
          "target": { "component": "package" },
          "workload": { "path": "${package.root}/bench/parser.php" },
          "metadata": { "fixture": { "component": "package" } }
        }"#,
    )
    .expect("write workload");
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
        package_root: Some(temp.path().to_path_buf()),
    };
    let args = FuzzRunArgs {
        comp: PositionalComponentArgs {
            component: Some("package".to_string()),
            path: None,
        },
        rig: Some("package-fuzz".to_string()),
        extension_override: ExtensionOverrideArgs { extensions: vec![] },
        setting_args: SettingArgs {
            setting: vec![],
            setting_json: vec![],
        },
        workload_id: Some("parser".to_string()),
        run_id: Some("proof-1".to_string()),
        tracker_refs: vec![],
        seed: None,
        inventory: None,
        require_case_log: false,
        require_coverage_summary: false,
        require_result_envelope: false,
        max_duration: None,
        gate_profile: FuzzGateProfileArg::Measurement,
        allow_destructive: false,
        isolation: FuzzIsolationArg::Shared,
        expect_metric: vec![],
        args: vec![],
    };
    let workload = FuzzWorkloadOutput {
        id: "parser".to_string(),
        label: None,
        description: None,
        source: format!("rig_workloads:generic:{}", workload_path.display()),
        manifest_path: Some(workload_path.to_string_lossy().to_string()),
    };
    let run_dir = RunDir::create().expect("run dir");
    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let override_env =
        homeboy::core::rig::expand::rig_component_path_override_env_name("package-fuzz", "package");
    let override_path = temp.path().join("runner/plugins/package");
    unsafe {
        std::env::set_var(&override_env, override_path.to_string_lossy().to_string());
    }

    let env = fuzz_runner_env(
        &args,
        Some(&context),
        Some(&workload),
        &results_path,
        &run_dir,
        None,
    )
    .expect("fuzz runner env");
    unsafe {
        std::env::remove_var(&override_env);
    }
    let expanded_path = env
        .iter()
        .find_map(|(key, value)| (key == "HOMEBOY_FUZZ_WORKLOAD_PATH").then_some(value))
        .expect("expanded workload path");
    let expanded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(expanded_path).expect("read expanded workload"),
    )
    .expect("parse expanded workload");

    assert_eq!(
        expanded["workload"]["path"].as_str(),
        Some(format!("{}/bench/parser.php", temp.path().display()).as_str())
    );
    assert_eq!(
        expanded["metadata"]["homeboy_runtime_context"]["components"]["package"]["path"].as_str(),
        Some(override_path.to_string_lossy().as_ref())
    );
    assert!(env.iter().any(|(key, value)| {
        key == "HOMEBOY_FUZZ_WORKLOAD_ROOT" && value == &temp.path().to_string_lossy()
    }));
}
