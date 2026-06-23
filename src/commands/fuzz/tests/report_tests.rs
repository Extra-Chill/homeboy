use super::*;

#[test]
fn fuzz_output_contract_includes_results_file_and_parsed_campaign() {
    let results = FuzzCampaign {
        schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
        version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
        id: "campaign-1".to_string(),
        title: None,
        safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
        surfaces: Vec::new(),
        targets: Vec::new(),
        workloads: Vec::new(),
        cases: Vec::new(),
        seeds: Vec::new(),
        coverage: Vec::new(),
        coverage_summary: None,
        findings: Vec::new(),
        artifacts: Vec::new(),
        thresholds: Vec::new(),
        provenance: None,
        replay: None,
        lifecycle: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };
    let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
        kind: "fuzz".to_string(),
        command: "fuzz.run".to_string(),
        component: "component-a".to_string(),
        rig_id: None,
        status: "passed".to_string(),
        workload_id: None,
        workload_path: None,
        run_id: None,
        seed: None,
        inventory_file: None,
        max_duration: None,
        passthrough_args: Vec::new(),
        target_inventory: None,
        execution: Some(FuzzExecutionOutput {
            kind: "fuzz".to_string(),
            extension_id: "generic".to_string(),
            exit_code: 0,
            success: true,
            run_dir: "/tmp/homeboy-run".to_string(),
            results_file: "/tmp/homeboy-run/fuzz-results.json".to_string(),
            stdout: String::new(),
            stderr: String::new(),
        }),
        results: Some(results),
        campaign_contract: fuzz_campaign_contract(None, Some("seed-1")),
        runner_contract: FuzzRunnerContract {
            capability: "fuzz".to_string(),
            extension_script_required: true,
            env: vec!["HOMEBOY_FUZZ_RESULTS_FILE"],
        },
        evidence_followups: Vec::new(),
    }))
    .unwrap();

    assert_eq!(
        run["execution"]["results_file"],
        "/tmp/homeboy-run/fuzz-results.json"
    );
    assert_eq!(
        run["results"]["schema"],
        homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
    );
    assert_eq!(run["results"]["id"], "campaign-1");
    assert_eq!(
        run["runner_contract"]["env"][0],
        "HOMEBOY_FUZZ_RESULTS_FILE"
    );
    assert_eq!(run["campaign_contract"]["seed"], "seed-1");
    assert_eq!(
        run["campaign_contract"]["result_schema"],
        homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
    );
    assert!(run["campaign_contract"]["unsupported"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field == "replay_command"));
}

#[test]
fn fuzz_campaign_contract_surfaces_extension_metadata() {
    let config = FuzzConfig {
        extension_script: Some("fuzz.sh".to_string()),
        workloads: Vec::new(),
        case_artifact: Some("failing-case".to_string()),
        corpus_artifacts: vec!["corpus".to_string()],
        seed: Some("manifest-seed".to_string()),
        replay_command: Some("runner replay {case}".to_string()),
        minimize_command: Some("runner minimize {case}".to_string()),
        result_schema: Some("custom/fuzz-result/v1".to_string()),
        artifact_retention: Some("persisted-run-artifacts".to_string()),
    };

    let contract =
        serde_json::to_value(fuzz_campaign_contract(Some(&config), Some("cli-seed"))).unwrap();

    assert_eq!(contract["case_artifact"], "failing-case");
    assert_eq!(contract["corpus_artifacts"][0], "corpus");
    assert_eq!(contract["seed"], "cli-seed");
    assert_eq!(contract["replay_command"], "runner replay {case}");
    assert_eq!(contract["minimize_command"], "runner minimize {case}");
    assert_eq!(contract["result_schema"], "custom/fuzz-result/v1");
    assert_eq!(contract["artifact_retention"], "persisted-run-artifacts");
    assert!(contract["unsupported"].as_array().unwrap().is_empty());
}

#[test]
fn select_workload_requires_explicit_id_for_ambiguous_fuzz_workloads() {
    let workloads = vec![
        FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: "extension:generic".to_string(),
            manifest_path: None,
        },
        FuzzWorkloadOutput {
            id: "serializer".to_string(),
            label: None,
            description: None,
            source: "extension:generic".to_string(),
            manifest_path: None,
        },
    ];

    let err = select_workload(&workloads, None).expect_err("ambiguous workload");

    assert!(err.message.contains("Multiple fuzz workloads"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("parser, serializer")));
}

#[test]
fn select_workload_rejects_empty_fuzz_selection() {
    let err = select_workload(&[], None).expect_err("empty workload selection");

    assert!(err.message.contains("No fuzz workloads"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("fuzz list")));
}

#[test]
fn fuzz_command_tests_keep_core_fixtures_product_neutral() {
    let source = [
        include_str!("../mod.rs"),
        include_str!("../dispatch.rs"),
        include_str!("../planning.rs"),
        include_str!("../types.rs"),
        include_str!("../types_extra.rs"),
        include_str!("../replay.rs"),
        include_str!("../report.rs"),
        include_str!("../execution.rs"),
        include_str!("../workloads.rs"),
        include_str!("../compare.rs"),
    ]
    .concat()
    .to_ascii_lowercase();
    let forbidden = ["word", "press"].concat();
    assert!(!source.contains(&forbidden));
}
