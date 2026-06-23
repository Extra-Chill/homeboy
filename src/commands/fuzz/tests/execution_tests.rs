use super::*;

#[test]
fn fuzz_run_persists_requested_run_id_and_results_artifact() {
    with_isolated_home(|home| {
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
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
            seed: Some("1234".to_string()),
            inventory: None,
            max_duration: None,
            args: vec![],
        };
        let results_path = home.path().join("fuzz-results.json");
        std::fs::write(&results_path, "{}").expect("results file");

        let persisted = persist_fuzz_run_evidence(FuzzRunEvidenceInput {
            run_id: args.run_id.as_deref(),
            component_id: "component-a",
            rig_id: args.rig.as_deref(),
            workload_id: args.workload_id.as_deref(),
            workload_path: Some("/tmp/fuzz/parser.json"),
            status: "passed",
            exit_code: 0,
            success: true,
            args: &args,
            results_path: &results_path,
            results: None,
            results_error: None,
        })
        .expect("persist fuzz run")
        .expect("run record");

        assert_eq!(persisted.id, "proof-1");
        assert_eq!(persisted.kind, "fuzz");
        assert_eq!(persisted.status, "pass");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .get_run("proof-1")
            .expect("get run")
            .expect("persisted run");
        assert_eq!(run.component_id.as_deref(), Some("component-a"));
        assert_eq!(run.rig_id.as_deref(), Some("package-fuzz"));
        assert_eq!(run.metadata_json["workload_id"], "parser");
        assert_eq!(run.metadata_json["seed"], "1234");
        assert!(run
            .command
            .as_deref()
            .unwrap_or_default()
            .contains("homeboy fuzz run component-a"));
        let artifacts = store.list_artifacts("proof-1").expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "fuzz_results");
        assert_eq!(artifacts[0].artifact_type, "file");
        assert!(std::path::Path::new(&artifacts[0].path).is_file());
    });
}

#[test]
fn fuzz_run_outcome_fails_when_successful_command_reports_failed_campaign() {
    let mut campaign = empty_fuzz_campaign();
    campaign.metadata = serde_json::json!({
        "status": "failed",
        "success": false,
        "case_counts": { "passed": 2, "failed": 1, "errored": 0 }
    });

    let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_outcome_fails_when_successful_command_reports_open_finding() {
    let mut campaign = empty_fuzz_campaign();
    campaign.findings = vec![FuzzFinding {
        schema: homeboy::core::fuzz::FUZZ_FINDING_SCHEMA.to_string(),
        id: "finding-1".to_string(),
        title: "runner surfaced a failing case".to_string(),
        severity: "high".to_string(),
        status: FuzzFindingStatus::Open,
        surface_id: None,
        target_id: None,
        operation_id: None,
        case_id: Some("case-1".to_string()),
        workload_id: None,
        seed_id: None,
        fingerprint: None,
        artifact_ids: Vec::new(),
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }];

    let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_outcome_fails_when_successful_command_reports_failed_lifecycle_phase() {
    let mut campaign = empty_fuzz_campaign();
    campaign.lifecycle = Some(LifecycleResultMetadata {
        schema: LIFECYCLE_RESULT_SCHEMA.to_string(),
        version: LIFECYCLE_CONTRACT_VERSION,
        phases: vec![LifecyclePhaseResult {
            id: "prepare".to_string(),
            phase: LifecyclePhaseKind::Prepare,
            status: LifecyclePhaseStatus::Failed,
            snapshot_ref: None,
            started_at: None,
            finished_at: None,
            message: Some("runtime prepare failed".to_string()),
        }],
        snapshot_refs: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    });

    let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_outcome_fails_when_workload_reports_invariant_failure_count() {
    let mut campaign = empty_fuzz_campaign();
    campaign.metadata = serde_json::json!({
        "wordpress_fuzz_result": {
            "status": "passed",
            "success": true,
            "cases": [
                {
                    "status": "passed",
                    "metadata": {
                        "observations": [
                            {
                                "payload": {
                                    "metrics": {
                                        "side_effect_invariant_failure_count": 1
                                    }
                                }
                            }
                        ]
                    }
                }
            ]
        }
    });

    let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_persists_raw_results_artifact_when_results_parse_fails() {
    with_isolated_home(|home| {
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
                path: None,
            },
            rig: Some("package-fuzz".to_string()),
            extension_override: ExtensionOverrideArgs { extensions: vec![] },
            setting_args: SettingArgs {
                setting: vec![],
                setting_json: vec![],
            },
            workload_id: Some("parser".to_string()),
            run_id: Some("proof-bad-results".to_string()),
            seed: None,
            inventory: None,
            max_duration: None,
            args: vec![],
        };
        let results_path = home.path().join("fuzz-results.json");
        std::fs::write(
            &results_path,
            r#"{"schema":"unsupported/fuzz-result/v1","id":"raw-output"}"#,
        )
        .expect("results file");

        let persisted = persist_fuzz_run_evidence(FuzzRunEvidenceInput {
            run_id: args.run_id.as_deref(),
            component_id: "component-a",
            rig_id: args.rig.as_deref(),
            workload_id: args.workload_id.as_deref(),
            workload_path: Some("/tmp/fuzz/parser.json"),
            status: "failed",
            exit_code: 1,
            success: false,
            args: &args,
            results_path: &results_path,
            results: None,
            results_error: Some(
                "fuzz results schema must be homeboy/fuzz-campaign/v1, got unsupported/fuzz-result/v1",
            ),
        })
        .expect("persist fuzz run")
        .expect("run record");

        assert_eq!(persisted.id, "proof-bad-results");
        assert_eq!(persisted.status, "fail");
        assert_eq!(
            persisted.metadata_json["campaign_id"],
            serde_json::Value::Null
        );
        assert!(persisted.metadata_json["results_error"]
            .as_str()
            .unwrap()
            .contains("unsupported/fuzz-result/v1"));

        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store
            .list_artifacts("proof-bad-results")
            .expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "fuzz_results");
        assert_eq!(artifacts[0].mime.as_deref(), Some("application/json"));
        assert!(std::path::Path::new(&artifacts[0].path).is_file());
        let raw = std::fs::read_to_string(&artifacts[0].path).expect("raw artifact");
        assert!(raw.contains("unsupported/fuzz-result/v1"));
    });
}

#[test]
fn fuzz_report_persists_result_envelope_artifact_for_run_id() {
    with_isolated_home(|home| {
        let artifact_root = home.path().join("agent-readable-artifacts");
        homeboy::core::set_artifact_root_override(Some(artifact_root));
        seed_fuzz_run("report-run-1");
        let results_path = home.path().join("fuzz-campaign.json");
        std::fs::write(
            &results_path,
            serde_json::to_string(&empty_fuzz_campaign()).expect("serialize campaign"),
        )
        .expect("results file");

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: fuzz_run_args_with_run_id("report-run-1"),
            output_envelope: None,
            envelope_id: None,
        })
        .expect("fuzz report");

        assert_eq!(output.envelope_file, None);
        assert_eq!(output.envelope.id, "report-run-1");
        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts("report-run-1").expect("artifacts");
        let envelope_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND)
            .expect("fuzz result envelope artifact");
        assert_eq!(envelope_artifact.artifact_type, "file");
        assert_eq!(envelope_artifact.mime.as_deref(), Some("application/json"));
        assert_eq!(
            envelope_artifact.metadata_json["envelope_id"],
            "report-run-1"
        );
        let persisted = std::fs::read_to_string(&envelope_artifact.path).expect("artifact file");
        assert!(persisted.contains(homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA));

        let artifact_index =
            homeboy::core::observation::evidence_report::evidence_artifact_index(&artifacts);
        assert_eq!(artifact_index.count, 1);
        assert_eq!(artifact_index.file_count, 1);
        assert_eq!(
            artifact_index.artifacts[0].kind,
            FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND
        );
        assert!(artifact_index.artifacts[0].fetch_command.is_some());
        homeboy::core::set_artifact_root_override(None);
    });
}

#[test]
fn fuzz_report_fails_required_artifact_gate_when_replay_data_is_missing() {
    with_isolated_home(|home| {
        let results_path = home.path().join("fuzz-campaign.json");
        std::fs::write(
            &results_path,
            serde_json::to_string(&artifact_complete_fuzz_campaign()).expect("serialize campaign"),
        )
        .expect("results file");

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: fuzz_run_args_with_run_id("report-run-missing-replay"),
            output_envelope: None,
            envelope_id: None,
        })
        .expect("fuzz report");

        assert_eq!(output.status, "failed");
        assert!(output.gates.iter().any(|gate| {
            gate.gate_id == "has-required-artifact-replay-data"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
        assert!(output.gates.iter().any(|gate| {
            gate.gate_id == "has-required-artifact-result-envelope" && gate.status == "passed"
        }));
        assert_eq!(output.envelope.status, output.status);
    });
}

#[test]
fn fuzz_report_passes_required_artifact_gates_with_seed_replay_data() {
    with_isolated_home(|home| {
        let mut campaign = artifact_complete_fuzz_campaign();
        campaign.seeds = vec![homeboy::core::fuzz::FuzzSeed {
            schema: homeboy::core::fuzz::FUZZ_SEED_SCHEMA.to_string(),
            id: "seed-1".to_string(),
            kind: "literal".to_string(),
            label: None,
            value: Some("seed-value".to_string()),
            artifact: None,
            tags: Vec::new(),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }];
        let results_path = home.path().join("fuzz-campaign.json");
        std::fs::write(
            &results_path,
            serde_json::to_string(&campaign).expect("serialize campaign"),
        )
        .expect("results file");

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: fuzz_run_args_with_run_id("report-run-with-replay"),
            output_envelope: None,
            envelope_id: None,
        })
        .expect("fuzz report");

        assert_eq!(output.status, "passed");
        assert!(output.gates.iter().any(|gate| {
            gate.gate_id == "has-required-artifact-replay-data"
                && gate.status == "passed"
                && gate.observed == 1.0
        }));
    });
}

#[test]
fn fuzz_report_records_existing_output_envelope_path_as_artifact() {
    with_isolated_home(|home| {
        seed_fuzz_run("report-run-output");
        let results_path = home.path().join("fuzz-campaign.json");
        let envelope_path = home.path().join("fuzz-envelope.json");
        std::fs::write(
            &results_path,
            serde_json::to_string(&empty_fuzz_campaign()).expect("serialize campaign"),
        )
        .expect("results file");

        run_report(FuzzReportArgs {
            results_file: results_path,
            run: fuzz_run_args_with_run_id("report-run-output"),
            output_envelope: Some(envelope_path.clone()),
            envelope_id: Some("custom-envelope".to_string()),
        })
        .expect("fuzz report");

        assert!(envelope_path.is_file());
        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store
            .list_artifacts("report-run-output")
            .expect("artifacts");
        let envelope_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND)
            .expect("fuzz result envelope artifact");
        assert_eq!(
            envelope_artifact.metadata_json["envelope_id"],
            "custom-envelope"
        );
        assert!(std::path::Path::new(&envelope_artifact.path).is_file());
    });
}

#[test]
fn fuzz_evidence_followups_point_to_raw_results_when_parse_fails() {
    let results_path = Path::new("/tmp/homeboy-run/fuzz-results.json");
    let normalization_error =
        ["Unsupported ", "Word", "Press", " fuzz case status: error"].concat();

    let followups = fuzz_evidence_followups(
        Some("proof-bad-results"),
        Some(&normalization_error),
        results_path,
    );

    assert!(followups
        .iter()
        .any(|followup| followup == "homeboy runs artifacts proof-bad-results"));
    assert!(followups.iter().any(|followup| {
        followup.contains("/tmp/homeboy-run/fuzz-results.json")
            && followup.contains("normalization failed")
            && followup.contains(&normalization_error)
    }));
}
