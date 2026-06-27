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
            tracker_refs: vec![homeboy::core::evidence_manifest::TrackerRef {
                kind: "github_issue".to_string(),
                id: "Extra-Chill/homeboy#123".to_string(),
                url: None,
                title: None,
                state: None,
            }],
            seed: Some("1234".to_string()),
            inventory: None,
            require_case_log: false,
            require_coverage_summary: false,
            require_result_envelope: false,
            max_duration: None,
            gate_profile: FuzzGateProfileArg::Measurement,
            expect_metric: vec![],
            args: vec![],
        };
        let results_path = home.path().join("fuzz-results.json");
        let artifacts_dir = home.path().join("fuzz-artifacts");
        std::fs::write(&results_path, "{}").expect("results file");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");

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
            artifacts_dir: &artifacts_dir,
            results: None,
            expected_metric_gates: &[],
            results_error: None,
            missing_artifact_refs: &[],
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
        assert_eq!(run.metadata_json["tracker_refs"][0]["kind"], "github_issue");
        assert_eq!(
            run.metadata_json["tracker_refs"][0]["id"],
            "Extra-Chill/homeboy#123"
        );
        assert!(run
            .command
            .as_deref()
            .unwrap_or_default()
            .contains("--tracker-ref github_issue:Extra-Chill/homeboy#123"));
        let artifacts = store.list_artifacts("proof-1").expect("artifacts");
        assert_eq!(artifacts.len(), 2);
        let results_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "fuzz_results")
            .expect("fuzz results artifact");
        assert_eq!(results_artifact.artifact_type, "file");
        assert!(std::path::Path::new(&results_artifact.path).is_file());
        let dir_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "fuzz_artifacts")
            .expect("fuzz artifacts directory");
        assert_eq!(dir_artifact.artifact_type, "directory");
        assert_eq!(
            dir_artifact.metadata_json["source"],
            "HOMEBOY_FUZZ_ARTIFACTS_DIR"
        );
    });
}

#[test]
fn fuzz_run_persistence_generates_run_id_when_omitted() {
    with_isolated_home(|home| {
        let mut args = fuzz_run_args_with_run_id("ignored");
        args.run_id = None;
        let results_path = home.path().join("fuzz-results.json");
        let artifacts_dir = home.path().join("fuzz-artifacts");
        std::fs::write(&results_path, "{}").expect("results file");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");

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
            artifacts_dir: &artifacts_dir,
            results: None,
            expected_metric_gates: &[],
            results_error: None,
            missing_artifact_refs: &[],
        })
        .expect("persist fuzz run")
        .expect("run record");

        assert!(persisted.id.starts_with("fuzz-"));
        let store = ObservationStore::open_initialized().expect("store");
        assert!(store.get_run(&persisted.id).expect("get run").is_some());
        assert_eq!(
            store
                .list_artifacts(&persisted.id)
                .expect("artifacts")
                .len(),
            2
        );
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

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

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
        source_refs: Vec::new(),
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }];

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_expected_metric_gate_fails_when_observed_metric_differs() {
    let mut campaign = empty_fuzz_campaign();
    campaign.metadata = serde_json::json!({
        "metrics": {
            "side_effect_grouped_created_count": 25,
            "simple_created": 25,
            "grouped_created": 25,
            "variation_created": 25
        }
    });
    let expectations = vec![(
        "side_effect_grouped_created_count".to_string(),
        "2".to_string(),
    )];

    let gates = evaluate_expected_metric_gates(Some(&campaign), &expectations);
    let error = fuzz_expected_metric_error(&gates).expect("expected metric failure");
    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), Some(&error));

    assert_eq!(gate_status(&gates), "failed");
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "expected-metric-side_effect_grouped_created_count"
            && gate.status == "failed"
            && gate.observed == 25.0
            && gate.expected == 2.0
    }));
    assert!(error.contains("side_effect_grouped_created_count expected 2 observed 25"));
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

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

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

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

    assert_eq!(outcome.status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_outcome_reports_timeout_as_non_pass() {
    let outcome = fuzz_run_outcome(0, true, true, None, None);

    assert_eq!(outcome.status, "timeout");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 124);
}

#[test]
fn fuzz_run_outcome_reports_skipped_lifecycle_as_non_proof() {
    let mut campaign = empty_fuzz_campaign();
    campaign.lifecycle = Some(LifecycleResultMetadata {
        schema: LIFECYCLE_RESULT_SCHEMA.to_string(),
        version: LIFECYCLE_CONTRACT_VERSION,
        phases: vec![LifecyclePhaseResult {
            id: "execute".to_string(),
            phase: LifecyclePhaseKind::Snapshot,
            status: LifecyclePhaseStatus::Skipped,
            snapshot_ref: None,
            started_at: None,
            finished_at: None,
            message: Some("runner skipped unsupported workload".to_string()),
        }],
        snapshot_refs: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    });

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

    assert_eq!(outcome.status, "skipped");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_run_outcome_reports_unsupported_metadata_as_non_proof() {
    let mut campaign = empty_fuzz_campaign();
    campaign.metadata = serde_json::json!({
        "wordpress_fuzz_result": {
            "status": "unsupported",
            "success": true
        }
    });

    let outcome = fuzz_run_outcome(0, true, false, Some(&campaign), None);

    assert_eq!(outcome.status, "unsupported");
    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn fuzz_max_duration_accepts_supported_units() {
    assert_eq!(
        fuzz_max_duration(Some("250ms")).expect("duration"),
        Some(std::time::Duration::from_millis(250))
    );
    assert_eq!(
        fuzz_max_duration(Some("60s")).expect("duration"),
        Some(std::time::Duration::from_secs(60))
    );
    assert_eq!(
        fuzz_max_duration(Some("5m")).expect("duration"),
        Some(std::time::Duration::from_secs(300))
    );
    assert_eq!(
        fuzz_max_duration(Some("1h")).expect("duration"),
        Some(std::time::Duration::from_secs(3600))
    );
}

#[test]
fn fuzz_max_duration_rejects_zero_and_unknown_units() {
    assert!(fuzz_max_duration(Some("0s")).is_err());
    assert!(fuzz_max_duration(Some("10x")).is_err());
}

#[test]
fn strict_fuzz_run_artifact_validation_reports_missing_campaign() {
    let mut args = fuzz_run_args_with_run_id("strict-proof");
    args.require_case_log = true;

    let error = fuzz_run_artifact_validation_error(&args, None).expect("strict error");

    assert!(error.contains("runner did not emit a fuzz campaign"));
}

#[test]
fn strict_fuzz_run_artifact_validation_reports_missing_required_artifacts() {
    let mut args = fuzz_run_args_with_run_id("strict-proof");
    args.require_case_log = true;
    args.require_coverage_summary = true;
    args.require_result_envelope = true;
    let campaign = artifact_complete_fuzz_campaign();

    let error = fuzz_run_artifact_validation_error(&args, Some(&campaign)).expect("strict error");

    assert!(!error.contains("case log"));
    assert!(!error.contains("coverage summary"));
    assert!(error.contains("result envelope"));
}

#[test]
fn strict_fuzz_run_artifact_validation_passes_with_required_artifacts() {
    let mut args = fuzz_run_args_with_run_id("strict-proof");
    args.require_case_log = true;
    args.require_coverage_summary = true;
    args.require_result_envelope = true;
    let mut campaign = artifact_complete_fuzz_campaign();
    campaign.artifacts.push(homeboy::core::fuzz::FuzzArtifact {
        schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
        id: "result-envelope".to_string(),
        kind: "result_envelope".to_string(),
        artifact: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    });

    assert!(fuzz_run_artifact_validation_error(&args, Some(&campaign)).is_none());
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
            tracker_refs: vec![],
            seed: None,
            inventory: None,
            require_case_log: false,
            require_coverage_summary: false,
            require_result_envelope: false,
            max_duration: None,
            gate_profile: FuzzGateProfileArg::Measurement,
            expect_metric: vec![],
            args: vec![],
        };
        let results_path = home.path().join("fuzz-results.json");
        let artifacts_dir = home.path().join("fuzz-artifacts");
        std::fs::write(
            &results_path,
            r#"{"schema":"unsupported/fuzz-result/v1","id":"raw-output"}"#,
        )
        .expect("results file");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");

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
            artifacts_dir: &artifacts_dir,
            results: None,
            expected_metric_gates: &[],
            results_error: Some(
                "fuzz results schema must be homeboy/fuzz-campaign/v1, got unsupported/fuzz-result/v1",
            ),
            missing_artifact_refs: &[],
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
        let results_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "fuzz_results")
            .expect("fuzz results artifact");
        assert_eq!(results_artifact.mime.as_deref(), Some("application/json"));
        assert!(std::path::Path::new(&results_artifact.path).is_file());
        let raw = std::fs::read_to_string(&results_artifact.path).expect("raw artifact");
        assert!(raw.contains("unsupported/fuzz-result/v1"));
    });
}

#[test]
fn fuzz_artifact_ref_validation_reports_missing_local_refs() {
    with_isolated_home(|home| {
        let artifacts_dir = home.path().join("fuzz-artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        std::fs::write(artifacts_dir.join("present.json"), "{}").expect("present artifact");
        let mut campaign = empty_fuzz_campaign();
        campaign.artifacts.push(homeboy::core::fuzz::FuzzArtifact {
            schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            artifact: Some(homeboy::core::artifact_contract::ArtifactContract {
                schema: homeboy::core::artifact_contract::ARTIFACT_CONTRACT_SCHEMA.to_string(),
                kind: "case_log".to_string(),
                artifact_type: "file".to_string(),
                path: Some("missing.json".to_string()),
                url: None,
                public_url: None,
                role: None,
                label: None,
                semantic_key: None,
                size_bytes: None,
                sha256: None,
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        });
        campaign.metadata = serde_json::json!({
            "artifact_refs": ["present.json", "also-missing.json", "https://example.test/remote.json"]
        });

        let validation = fuzz_artifact_ref_validation(Some(&campaign), &artifacts_dir);

        assert_eq!(
            validation.missing_refs,
            vec!["also-missing.json".to_string(), "missing.json".to_string()]
        );
        assert!(validation
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("HOMEBOY_FUZZ_ARTIFACTS_DIR"));
    });
}

#[test]
fn fuzz_artifact_postprocess_collects_declared_output_under_artifact_root() {
    with_isolated_home(|home| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workload_path = temp.path().join("parser.json");
        std::fs::write(&workload_path, "{}").expect("workload");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "fuzz_workloads": {
                "generic": [
                    {
                        "path": "${package.root}/parser.json",
                        "artifact_postprocess": [
                            {
                                "id": "coverage-summary",
                                "helper": "sh",
                                "action": "-c",
                                "input": "${run.fuzz_results}",
                                "output": "coverage/summary.json",
                                "parameters": {
                                    "args": ["cp \"$HOMEBOY_ARTIFACT_POSTPROCESS_INPUT\" \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\""]
                                }
                            }
                        ]
                    }
                ]
            }
        }))
        .expect("parse rig spec");
        let context = FuzzRigContext {
            spec,
            package_root: Some(temp.path().to_path_buf()),
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: format!("rig_workloads:generic:{}", workload_path.display()),
            manifest_path: Some(workload_path.to_string_lossy().to_string()),
        };
        let results_path = home.path().join("fuzz-results.json");
        std::fs::write(&results_path, r#"{"status":"ok"}"#).expect("results");
        let artifacts_dir = home.path().join("fuzz-artifacts");

        let outputs = run_fuzz_artifact_postprocess(
            Some(&context),
            Some("generic"),
            Some(&workload),
            &results_path,
            &artifacts_dir,
        )
        .expect("postprocess");

        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].success);
        let collected = artifacts_dir.join("coverage/summary.json");
        assert!(collected.is_file());
        assert_eq!(
            std::fs::read_to_string(collected).expect("collected"),
            r#"{"status":"ok"}"#
        );
    });
}

#[test]
fn required_fuzz_artifact_postprocess_fails_when_output_is_missing() {
    with_isolated_home(|home| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workload_path = temp.path().join("parser.json");
        std::fs::write(&workload_path, "{}").expect("workload");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "fuzz_workloads": {
                "generic": [
                    {
                        "path": "${package.root}/parser.json",
                        "artifact_postprocess": [
                            {
                                "id": "gap-report",
                                "helper": "sh",
                                "action": "-c",
                                "output": "gaps/report.json",
                                "parameters": { "args": ["true"] }
                            }
                        ]
                    }
                ]
            }
        }))
        .expect("parse rig spec");
        let context = FuzzRigContext {
            spec,
            package_root: Some(temp.path().to_path_buf()),
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: format!("rig_workloads:generic:{}", workload_path.display()),
            manifest_path: Some(workload_path.to_string_lossy().to_string()),
        };
        let results_path = home.path().join("fuzz-results.json");
        std::fs::write(&results_path, "{}").expect("results");
        let artifacts_dir = home.path().join("fuzz-artifacts");

        let outputs = run_fuzz_artifact_postprocess(
            Some(&context),
            Some("generic"),
            Some(&workload),
            &results_path,
            &artifacts_dir,
        )
        .expect("postprocess");

        assert_eq!(outputs.len(), 1);
        assert!(!outputs[0].success);
        let error = fuzz_postprocess_error(&outputs).expect("required failure");
        assert!(error.contains("gap-report"));
        assert!(error.contains("did not create required output"));
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

        let mut run_args = fuzz_run_args_with_run_id("report-run-1");
        run_args.gate_profile = FuzzGateProfileArg::Measurement;

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: run_args,
            output_envelope: None,
            envelope_id: None,
        })
        .expect("fuzz report");

        assert_eq!(output.envelope_file, None);
        assert_eq!(output.envelope.id, "report-run-1");
        assert!(output
            .performance_hotspots
            .slowest_timing_metrics
            .is_empty());
        assert!(output
            .performance_hotspots
            .hottest_metric_families
            .is_empty());
        assert_eq!(output.status, "passed");
        assert!(output.gates.is_empty());
        assert!(output.envelope.gates.is_empty());
        assert!(output.envelope.required_artifacts.is_empty());
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

        let mut run_args = fuzz_run_args_with_run_id("report-run-missing-replay");
        run_args.gate_profile = FuzzGateProfileArg::Strict;

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: run_args,
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

        let mut run_args = fuzz_run_args_with_run_id("report-run-with-replay");
        run_args.gate_profile = FuzzGateProfileArg::Strict;

        let output = run_report(FuzzReportArgs {
            results_file: results_path,
            run: run_args,
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

        let mut run_args = fuzz_run_args_with_run_id("report-run-output");
        run_args.gate_profile = FuzzGateProfileArg::Measurement;

        run_report(FuzzReportArgs {
            results_file: results_path,
            run: run_args,
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
