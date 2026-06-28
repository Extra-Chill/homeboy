use super::*;

#[test]
fn fuzz_discover_merges_inventory_artifacts_with_provenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let first = temp.path().join("first.json");
    let second = temp.path().join("second.json");
    let mut second_inventory = planner_inventory();
    second_inventory.id = "second-inventory".to_string();
    second_inventory.targets[0].id = "api.orders".to_string();
    second_inventory.workloads.clear();
    second_inventory.seeds.clear();
    write_inventory(&first, &planner_inventory());
    write_inventory(&second, &second_inventory);

    let output = super::run_discover(FuzzDiscoverArgs {
        inventories: vec![first.clone(), second.clone()],
        inventory_id: Some("merged-inventory".to_string()),
        source_label: "unit-test".to_string(),
    })
    .expect("discover inventory");

    assert_eq!(output.command, "fuzz.discover");
    assert_eq!(output.status, "ok");
    assert_eq!(output.target_inventory.id, "merged-inventory");
    assert_eq!(output.summary.targets, 2);
    assert_eq!(output.summary.workloads, 1);
    assert_eq!(output.summary.seeds, 1);
    assert_eq!(
        output.inventory_files,
        vec![first.display().to_string(), second.display().to_string()]
    );
    let provenance = output.target_inventory.provenance.expect("provenance");
    assert_eq!(provenance.producer, "homeboy fuzz discover");
    assert_eq!(provenance.source_ref.as_deref(), Some("unit-test"));
    assert_eq!(
        provenance.metadata["discovery_mode"],
        serde_json::json!("artifact")
    );
}

#[test]
fn fuzz_discover_requires_inventory_artifacts() {
    let error = super::run_discover(FuzzDiscoverArgs {
        inventories: Vec::new(),
        inventory_id: None,
        source_label: "artifact".to_string(),
    })
    .expect_err("empty discovery should fail");

    assert!(error
        .to_string()
        .contains("at least one --inventory artifact is required"));
}

#[test]
fn fuzz_plan_selects_inventory_targets_operations_seeds_and_budgets() {
    let args = planner_args();
    let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

    assert_eq!(
        metadata["selection"]["target_ids"].as_array().unwrap(),
        &vec![serde_json::json!("api.users")]
    );
    assert_eq!(
        metadata["selection"]["operation_families"]
            .as_array()
            .unwrap(),
        &vec![serde_json::json!("create"), serde_json::json!("read")]
    );
    assert_eq!(
        metadata["selection"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        metadata["selection"]["seed_ids"].as_array().unwrap(),
        &vec![serde_json::json!("seed-a")]
    );
    assert_eq!(metadata["budgets"]["case_budget"], serde_json::json!(25));
    assert_eq!(
        metadata["budgets"]["duration_budget_seconds"],
        serde_json::json!(120)
    );
    assert_eq!(
        metadata["sampling"]["schema"],
        serde_json::json!(homeboy::core::fuzz::FUZZ_SAMPLING_REQUEST_SCHEMA)
    );
    assert_eq!(metadata["sampling"]["strategy"], serde_json::json!("all"));
    assert_eq!(metadata["sampling"]["case_budget"], serde_json::json!(25));
    assert_eq!(
        metadata["sampling"]["duration_budget_seconds"],
        serde_json::json!(120)
    );
    assert_eq!(
        metadata["sampling"]["target_strata"][0]["values"],
        serde_json::json!(["api.users"])
    );
    assert_eq!(
        metadata["sampling"]["operation_strata"][0]["values"],
        serde_json::json!(["create", "read"])
    );
    assert_eq!(
        metadata["sampling"]["corpus_refs"][0]["id"],
        serde_json::json!("seed-a")
    );
    assert_eq!(
        metadata["sampling"]["replay"],
        serde_json::json!({
            "deterministic": true,
            "seed_source": "corpus",
            "replay_batch_id": "proof-1-sampling"
        })
    );
    assert_eq!(metadata["isolation"]["required"], serde_json::json!(true));
    assert_eq!(
        metadata["planner"]["gate_profile"],
        serde_json::json!("measurement")
    );
    assert!(metadata["required_artifact_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(metadata["gate_ids"].as_array().unwrap().is_empty());
}

#[test]
fn fuzz_plan_strict_profile_requests_required_artifacts_and_gates() {
    let mut args = planner_args();
    args.run.gate_profile = FuzzGateProfileArg::Strict;

    let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

    assert_eq!(
        metadata["planner"]["gate_profile"],
        serde_json::json!("strict")
    );
    assert!(metadata["required_artifact_ids"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("case-log")));
    assert!(metadata["gate_ids"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("target-coverage-complete")));
}

#[test]
fn fuzz_plan_operation_filter_limits_inventory_selection() {
    let mut args = planner_args();
    args.operations = vec!["read".to_string()];

    let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

    assert_eq!(
        metadata["selection"]["operation_families"]
            .as_array()
            .unwrap(),
        &vec![serde_json::json!("read")]
    );
    assert_eq!(
        metadata["selection"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        metadata["skipped"]["operations"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn fuzz_plan_skips_destructive_operations_by_default() {
    let _guard = test_isolation_env_guard(None);
    let metadata =
        super::plan_inventory_selection(&planner_args(), &destructive_inventory()).unwrap();

    assert!(metadata["selection"]["operations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        metadata["skipped"]["operations"][0]["reason"],
        serde_json::json!("destructive")
    );
    assert_eq!(metadata["isolation"]["mode"], serde_json::json!("shared"));
    assert_eq!(
        metadata["isolation"]["destructive_allowed"],
        serde_json::json!(false)
    );
}

#[test]
fn fuzz_plan_skips_destructive_operations_without_verified_isolation() {
    let _guard = test_isolation_env_guard(None);
    let mut args = planner_args();
    args.run.allow_destructive = true;
    args.run.isolation = FuzzIsolationArg::Isolated;

    let metadata = super::plan_inventory_selection(&args, &destructive_inventory()).unwrap();

    assert!(metadata["selection"]["operations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        metadata["skipped"]["operations"][0]["reason"],
        serde_json::json!("destructive")
    );
    assert_eq!(
        metadata["skipped"]["operations"][0]["detail"],
        serde_json::json!(
            "destructive fuzz requires verified generic isolation proof from Lab/offloaded runner metadata"
        )
    );
    assert_eq!(metadata["isolation"]["mode"], serde_json::json!("isolated"));
    assert_eq!(
        metadata["isolation"]["destructive_allowed"],
        serde_json::json!(false)
    );
    assert_eq!(metadata["isolation"]["verified"], serde_json::json!(false));
}

#[test]
fn fuzz_plan_includes_destructive_operations_with_verified_isolation_opt_in() {
    let _guard = test_isolation_env_guard(Some("1"));
    let mut args = planner_args();
    args.run.allow_destructive = true;
    args.run.isolation = FuzzIsolationArg::Isolated;

    let metadata = super::plan_inventory_selection(&args, &destructive_inventory()).unwrap();

    assert_eq!(
        metadata["selection"]["operations"][0]["operation_id"],
        serde_json::json!("api.users.delete")
    );
    assert!(metadata["skipped"]["operations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(metadata["isolation"]["mode"], serde_json::json!("isolated"));
    assert_eq!(
        metadata["isolation"]["allow_destructive"],
        serde_json::json!(true)
    );
    assert_eq!(
        metadata["isolation"]["destructive_allowed"],
        serde_json::json!(true)
    );
    assert_eq!(metadata["isolation"]["verified"], serde_json::json!(true));
    assert_eq!(
        metadata["isolation"]["proof_source"],
        serde_json::json!("test_env")
    );
    assert_eq!(
        metadata["sampling"]["metadata"]["destructive_allowed"],
        serde_json::json!(true)
    );
}

#[test]
fn fuzz_plan_workload_safety_participates_in_destructive_skip() {
    let _guard = test_isolation_env_guard(None);
    let mut inventory = planner_inventory();
    inventory.workloads[0].safety_class = homeboy::core::fuzz::FuzzSafetyClass::Destructive;
    let mut args = planner_args();
    args.run.workload_id = Some("api-fuzz".to_string());

    let metadata = super::plan_inventory_selection(&args, &inventory).unwrap();

    assert!(metadata["selection"]["operations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        metadata["skipped"]["operations"][0]["reason"],
        serde_json::json!("destructive")
    );
}

#[test]
fn fuzz_plan_budget_flags_override_inventory_workload_budgets() {
    let mut args = planner_args();
    args.case_budget = Some(5);
    args.duration_budget_seconds = Some(10);
    args.run.max_duration = Some("30s".to_string());

    let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

    assert_eq!(metadata["budgets"]["case_budget"], serde_json::json!(5));
    assert_eq!(
        metadata["budgets"]["duration_budget_seconds"],
        serde_json::json!(10)
    );
    assert_eq!(
        metadata["budgets"]["max_duration"],
        serde_json::json!("30s")
    );
    assert_eq!(metadata["sampling"]["case_budget"], serde_json::json!(5));
    assert_eq!(
        metadata["sampling"]["duration_budget_seconds"],
        serde_json::json!(10)
    );
}

#[test]
fn fuzz_plan_sampling_metadata_records_caller_seed_deterministically() {
    let mut args = planner_args();
    args.run.seed = Some("seed-123".to_string());

    let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

    assert_eq!(metadata["sampling"]["seed"], serde_json::json!("seed-123"));
    assert_eq!(
        metadata["sampling"]["replay"],
        serde_json::json!({
            "deterministic": true,
            "seed_source": "caller",
            "replay_batch_id": "proof-1-sampling"
        })
    );
}

#[test]
fn fuzz_plan_skips_unsupported_inventory_operations() {
    let inventory = FuzzTargetInventory::from_value(serde_json::json!({
        "schema": "homeboy/fuzz-target-inventory/v1",
        "version": 1,
        "id": "component-a-inventory",
        "targets": [
            {
                "schema": "homeboy/fuzz-target/v1",
                "id": "custom.surface",
                "kind": "api",
                "operations": [
                    { "id": "custom.surface.invoke", "kind": "domain-specific" }
                ]
            }
        ]
    }))
    .expect("inventory parses");

    let metadata = super::plan_inventory_selection(&planner_args(), &inventory).unwrap();

    assert!(metadata["selection"]["operations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        metadata["skipped"]["operations"][0]["reason"],
        serde_json::json!("unsupported")
    );
    assert_eq!(metadata["skipped"]["targets"].as_array().unwrap().len(), 1);
}

#[test]
fn fuzz_run_parses_generic_contract_flags() {
    let cli = FuzzCli::parse_from([
        "fuzz",
        "run",
        "component-a",
        "--rig",
        "package-fuzz",
        "--workload",
        "parser",
        "--run-id",
        "proof-1",
        "--tracker-ref",
        "github_issue:Extra-Chill/homeboy#123",
        "--tracker-ref",
        "linear:HB-42",
        "--seed",
        "1234",
        "--inventory",
        "/tmp/fuzz-inventory.json",
        "--require-case-log",
        "--require-coverage-summary",
        "--require-result-envelope",
        "--max-duration",
        "60s",
        "--allow-destructive",
        "--isolation",
        "isolated",
        "--expect-metric",
        "side_effect_grouped_created_count=2",
        "--",
        "--engine",
        "libfuzzer",
    ]);

    match cli.args.command {
        Some(FuzzCommand::Run(run)) => {
            assert_eq!(run.comp.component.as_deref(), Some("component-a"));
            assert_eq!(run.rig.as_deref(), Some("package-fuzz"));
            assert_eq!(run.workload_id.as_deref(), Some("parser"));
            assert_eq!(run.run_id.as_deref(), Some("proof-1"));
            assert_eq!(run.tracker_refs.len(), 2);
            assert_eq!(run.tracker_refs[0].kind, "github_issue");
            assert_eq!(run.tracker_refs[0].id, "Extra-Chill/homeboy#123");
            assert_eq!(run.tracker_refs[1].kind, "linear");
            assert_eq!(run.tracker_refs[1].id, "HB-42");
            assert_eq!(run.seed.as_deref(), Some("1234"));
            assert_eq!(
                run.inventory.as_deref(),
                Some(Path::new("/tmp/fuzz-inventory.json"))
            );
            assert!(run.require_case_log);
            assert!(run.require_coverage_summary);
            assert!(run.require_result_envelope);
            assert_eq!(run.max_duration.as_deref(), Some("60s"));
            assert!(run.allow_destructive);
            assert_eq!(run.isolation, FuzzIsolationArg::Isolated);
            assert_eq!(
                run.expect_metric,
                vec![(
                    "side_effect_grouped_created_count".to_string(),
                    "2".to_string()
                )]
            );
            assert_eq!(run.args, vec!["--engine", "libfuzzer"]);
        }
        _ => panic!("expected fuzz run command"),
    }
}

#[test]
fn fuzz_doctor_parses_extension_id() {
    let cli = FuzzCli::parse_from(["fuzz", "doctor", "--extension", "runtime-a"]);

    match cli.args.command {
        Some(FuzzCommand::Doctor(doctor)) => {
            assert_eq!(doctor.extension_id, "runtime-a");
        }
        _ => panic!("expected fuzz doctor command"),
    }
}

#[test]
fn fuzz_validate_accepts_case_log_artifact() {
    let dir = tempfile::tempdir().expect("temp dir");
    let results_file = dir.path().join("fuzz-results.json");
    let case_log = dir.path().join("case-log.jsonl");
    std::fs::write(
        &results_file,
        serde_json::json!({
            "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
            "id": "campaign-1",
            "safety_class": "read_only",
            "coverage_summary": {
                "declared_targets": 0,
                "executable_targets": 0,
                "proven_targets": 0,
                "declared_operations": 0,
                "executable_operations": 0,
                "proven_operations": 0
            },
            "artifacts": [{
                "id": "case-log",
                "kind": "case_log"
            }]
        })
        .to_string(),
    )
    .expect("write campaign");
    std::fs::write(
        &case_log,
        r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"passed","duration_ms":5}"#,
    )
    .expect("write case log");

    let output = run_validate(FuzzValidateArgs {
        results_file,
        gate_profile: FuzzGateProfileArg::Measurement,
        case_logs: vec![case_log.clone()],
    })
    .expect("validate fuzz campaign and case log");

    assert_eq!(output.status, "passed");
    assert_eq!(output.case_log_entries, 1);
    assert_eq!(
        output.case_log_files,
        vec![case_log.to_string_lossy().to_string()]
    );
}

#[test]
fn fuzz_validate_rejects_invalid_case_log_artifact() {
    let dir = tempfile::tempdir().expect("temp dir");
    let results_file = dir.path().join("fuzz-results.json");
    let case_log = dir.path().join("case-log.jsonl");
    std::fs::write(
        &results_file,
        serde_json::json!({
            "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
            "id": "campaign-1",
            "safety_class": "read_only"
        })
        .to_string(),
    )
    .expect("write campaign");
    std::fs::write(
        &case_log,
        r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"skipped","duration_ms":5}"#,
    )
    .expect("write invalid case log");

    let err = match run_validate(FuzzValidateArgs {
        results_file,
        gate_profile: FuzzGateProfileArg::Measurement,
        case_logs: vec![case_log],
    }) {
        Ok(_) => panic!("invalid case log should fail validation"),
        Err(err) => err,
    };

    assert!(err.message.contains("skip_reason"));
}

#[test]
fn fuzz_validate_measurement_profile_is_non_blocking_but_strict_profile_blocks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let results_file = dir.path().join("fuzz-results.json");
    std::fs::write(
        &results_file,
        serde_json::json!({
            "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
            "id": "campaign-1",
            "safety_class": "read_only",
            "findings": [{
                "id": "finding-1",
                "title": "open finding",
                "severity": "high",
                "status": "open"
            }],
            "metadata": {
                "status": "failed",
                "case_counts": { "passed": 1, "failed": 1 }
            }
        })
        .to_string(),
    )
    .expect("write campaign");

    let measurement = run_validate(FuzzValidateArgs {
        results_file: results_file.clone(),
        gate_profile: FuzzGateProfileArg::Measurement,
        case_logs: Vec::new(),
    })
    .expect("validate measurement campaign");
    let strict = run_validate(FuzzValidateArgs {
        results_file,
        gate_profile: FuzzGateProfileArg::Strict,
        case_logs: Vec::new(),
    })
    .expect("validate strict campaign");

    assert_eq!(measurement.status, "passed");
    assert_eq!(measurement.open_findings, 1);
    assert!(measurement.gates.is_empty());
    assert_eq!(strict.status, "failed");
    assert!(strict.gates.iter().any(|gate| {
        gate.gate_id == "no-open-findings" && gate.status == "failed" && gate.observed == 1.0
    }));
}

#[test]
fn fuzz_compare_parses_envelope_paths() {
    let cli = FuzzCli::parse_from([
        "fuzz",
        "compare",
        "baseline-envelope.json",
        "candidate-envelope.json",
    ]);

    match cli.args.command {
        Some(FuzzCommand::Compare(compare)) => {
            assert_eq!(compare.baseline, PathBuf::from("baseline-envelope.json"));
            assert_eq!(compare.candidate, PathBuf::from("candidate-envelope.json"));
        }
        _ => panic!("expected fuzz compare command"),
    }
}

#[test]
fn fuzz_output_contract_has_stable_variant_discriminators() {
    let contract = serde_json::to_value(FuzzOutput::Contract(run_contract())).unwrap();
    assert_eq!(contract["variant"], "contract");
    assert_eq!(
        contract["contract"]["schemas"]["result_envelope"],
        homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA
    );

    let list = serde_json::to_value(FuzzOutput::List(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: "component-a".to_string(),
        rig_id: None,
        workloads: Vec::new(),
        count: 0,
        run_hint: "hint".to_string(),
    }))
    .unwrap();
    assert_eq!(list["variant"], "list");

    let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
        kind: "fuzz".to_string(),
        command: "fuzz.run".to_string(),
        component: "component-a".to_string(),
        rig_id: Some("package-fuzz".to_string()),
        status: "passed".to_string(),
        workload_id: Some("parser".to_string()),
        workload_path: None,
        run_id: None,
        seed: None,
        inventory_file: None,
        max_duration: None,
        passthrough_args: Vec::new(),
        requested_settings: serde_json::Value::Null,
        gates: Vec::new(),
        target_inventory: None,
        execution: None,
        postprocess: Vec::new(),
        results: None,
        campaign_contract: fuzz_campaign_contract(None, None),
        runner_contract: FuzzRunnerContract {
            capability: "fuzz".to_string(),
            extension_script_required: true,
            env: Vec::new(),
        },
        evidence_refs: Vec::new(),
        evidence_followups: Vec::new(),
    }))
    .unwrap();
    assert_eq!(run["variant"], "run");
    assert_eq!(run["kind"], "fuzz");
    assert_eq!(run["rig_id"], "package-fuzz");
}
