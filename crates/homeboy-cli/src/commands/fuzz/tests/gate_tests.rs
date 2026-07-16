use super::*;

#[test]
fn fuzz_gate_evaluation_requires_case_log_evidence() {
    let campaign = FuzzCampaign {
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
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);

    assert_eq!(gate_status(&gates), "failed");
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "has-case-evidence" && gate.status == "failed" && gate.observed == 0.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "target-coverage-complete"
            && gate.status == "failed"
            && gate.observed == 0.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "operation-coverage-complete"
            && gate.status == "failed"
            && gate.observed == 0.0
    }));
}

#[test]
fn fuzz_gate_evaluation_accepts_case_level_fuzz_report_evidence() {
    let campaign = FuzzCampaign {
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
        coverage_summary: Some(zero_coverage_summary()),
        findings: Vec::new(),
        artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
            schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "case-evidence-report".to_string(),
            kind: "fuzz_report".to_string(),
            artifact: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }],
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);

    assert_eq!(gate_status(&gates), "passed");
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "has-case-evidence" && gate.status == "passed" && gate.observed == 1.0
    }));
}

#[test]
fn fuzz_gate_evaluation_accepts_metadata_artifact_refs() {
    let campaign = FuzzCampaign {
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
        coverage_summary: Some(zero_coverage_summary()),
        findings: Vec::new(),
        artifacts: Vec::new(),
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::json!({
            "artifact_refs": [{
                "name": "case_evidence_report",
                "path": "case-evidence/report.json",
                "role": "fuzz_report"
            }]
        }),
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);
    let summary = fuzz_coverage_completeness(&campaign);

    assert_eq!(gate_status(&gates), "passed");
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "has-case-evidence" && gate.status == "passed" && gate.observed == 1.0
    }));
    assert!(summary.has_summary);
    assert_eq!(summary.target_coverage_ratio, 1.0);
    assert_eq!(summary.operation_coverage_ratio, 1.0);
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "target-coverage-complete"
            && gate.status == "passed"
            && gate.observed == 1.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "operation-coverage-complete"
            && gate.status == "passed"
            && gate.observed == 1.0
    }));
}

#[test]
fn fuzz_gate_evaluation_accepts_zero_declared_coverage_summary() {
    let campaign = FuzzCampaign {
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
        coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
            schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
            declared_targets: 0,
            executable_targets: 0,
            proven_targets: 0,
            declared_operations: 0,
            executable_operations: 0,
            proven_operations: 0,
            skipped_targets: Vec::new(),
            skipped_operations: Vec::new(),
            surface_summaries: Vec::new(),
            kind_summaries: Vec::new(),
            artifact_ids: vec!["coverage-report".to_string()],
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }),
        findings: Vec::new(),
        artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
            schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            artifact: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }],
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);
    let summary = fuzz_coverage_completeness(&campaign);

    assert_eq!(gate_status(&gates), "passed");
    assert!(summary.has_summary);
    assert_eq!(summary.declared_targets, 0);
    assert_eq!(summary.target_coverage_ratio, 1.0);
    assert_eq!(summary.declared_operations, 0);
    assert_eq!(summary.operation_coverage_ratio, 1.0);
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "target-coverage-complete"
            && gate.status == "passed"
            && gate.observed == 1.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "operation-coverage-complete"
            && gate.status == "passed"
            && gate.observed == 1.0
    }));
}

#[test]
fn fuzz_coverage_completeness_fails_closed_without_summary() {
    let campaign = FuzzCampaign {
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
        artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
            schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            artifact: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }],
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);
    let summary = fuzz_coverage_completeness(&campaign);

    assert_eq!(gate_status(&gates), "failed");
    assert!(!summary.has_summary);
    assert_eq!(summary.target_coverage_ratio, 0.0);
    assert_eq!(summary.operation_coverage_ratio, 0.0);
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "target-coverage-complete"
            && gate.status == "failed"
            && gate.observed == 0.0
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "operation-coverage-complete"
            && gate.status == "failed"
            && gate.observed == 0.0
    }));
}

#[test]
fn fuzz_gate_evaluation_requires_complete_target_and_operation_coverage() {
    let mut campaign = FuzzCampaign {
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
        coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
            schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
            declared_targets: 2,
            executable_targets: 2,
            proven_targets: 1,
            declared_operations: 4,
            executable_operations: 4,
            proven_operations: 4,
            skipped_targets: Vec::new(),
            skipped_operations: Vec::new(),
            surface_summaries: Vec::new(),
            kind_summaries: Vec::new(),
            artifact_ids: vec!["coverage-report".to_string()],
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }),
        findings: Vec::new(),
        artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
            schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            artifact: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }],
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let gates = evaluate_fuzz_gates(&campaign);

    assert!(gates.iter().any(|gate| {
        gate.gate_id == "target-coverage-complete"
            && gate.status == "failed"
            && gate.observed == 0.5
    }));
    assert!(gates.iter().any(|gate| {
        gate.gate_id == "operation-coverage-complete"
            && gate.status == "passed"
            && gate.observed == 1.0
    }));

    campaign.coverage_summary.as_mut().unwrap().proven_targets = 2;
    assert_eq!(gate_status(&evaluate_fuzz_gates(&campaign)), "passed");
}

#[test]
fn fuzz_coverage_completeness_reports_summary_counts() {
    let campaign = FuzzCampaign {
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
        coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
            schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
            declared_targets: 2,
            executable_targets: 1,
            proven_targets: 1,
            declared_operations: 0,
            executable_operations: 0,
            proven_operations: 0,
            skipped_targets: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                id: "target-2".to_string(),
                reason: "auth_required".to_string(),
                label: None,
            }],
            skipped_operations: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                id: "operation-2".to_string(),
                reason: "config_required".to_string(),
                label: None,
            }],
            surface_summaries: vec![homeboy::core::fuzz::FuzzCoverageGroupSummary {
                id: "surface-a".to_string(),
                kind: "api".to_string(),
                label: Some("Surface A".to_string()),
                declared_targets: 2,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 2,
                executable_operations: 1,
                proven_operations: 1,
                skipped_targets: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                    id: "target-2".to_string(),
                    reason: "auth_required".to_string(),
                    label: None,
                }],
                skipped_operations: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                    id: "operation-2".to_string(),
                    reason: "config_required".to_string(),
                    label: None,
                }],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            kind_summaries: vec![homeboy::core::fuzz::FuzzCoverageGroupSummary {
                id: "read".to_string(),
                kind: "operation_kind".to_string(),
                label: None,
                declared_targets: 1,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 1,
                executable_operations: 1,
                proven_operations: 1,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            artifact_ids: vec!["coverage-report".to_string()],
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }),
        findings: Vec::new(),
        artifacts: Vec::new(),
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    };

    let summary = fuzz_coverage_completeness(&campaign);

    assert!(summary.has_summary);
    assert_eq!(summary.declared_targets, 2);
    assert_eq!(summary.target_coverage_ratio, 0.5);
    assert_eq!(summary.operation_coverage_ratio, 1.0);
    assert_eq!(summary.skipped_targets, 1);
    assert_eq!(summary.skipped_operations, 1);
    assert_eq!(summary.skipped_reason_counts["auth_required"], 1);
    assert_eq!(summary.skipped_reason_counts["config_required"], 1);
    assert_eq!(summary.surface_summaries.len(), 1);
    assert_eq!(summary.surface_summaries[0].id, "surface-a");
    assert_eq!(summary.surface_summaries[0].target_coverage_ratio, 0.5);
    assert_eq!(summary.surface_summaries[0].operation_coverage_ratio, 0.5);
    assert_eq!(
        summary.surface_summaries[0].skipped_reason_counts["auth_required"],
        1
    );
    assert_eq!(summary.kind_summaries[0].id, "read");
    assert_eq!(summary.artifact_ids, vec!["coverage-report"]);
}
