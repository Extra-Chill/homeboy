use super::*;

fn gate<'a>(
    gates: &'a [super::super::types::FuzzGateEvaluation],
    id: &str,
) -> &'a super::super::types::FuzzGateEvaluation {
    gates
        .iter()
        .find(|gate| gate.gate_id == id)
        .unwrap_or_else(|| panic!("missing gate {id}"))
}

fn coverage_summary(
    declared_targets: u64,
    proven_targets: u64,
    declared_operations: u64,
    proven_operations: u64,
) -> FuzzCoverageSummary {
    FuzzCoverageSummary {
        declared_targets,
        executable_targets: declared_targets,
        proven_targets,
        declared_operations,
        executable_operations: declared_operations,
        proven_operations,
        ..zero_coverage_summary()
    }
}

/// Without case evidence or a coverage summary, every evidence gate fails
/// closed rather than passing vacuously.
#[test]
fn fuzz_gates_fail_closed_without_case_evidence_or_coverage_summary() {
    let empty = empty_fuzz_campaign();
    let gates = evaluate_fuzz_gates(&empty);
    assert_eq!(gate_status(&gates), "failed");
    for id in [
        "has-case-evidence",
        "target-coverage-complete",
        "operation-coverage-complete",
    ] {
        assert_eq!(gate(&gates, id).status, "failed", "{id}");
        assert_eq!(gate(&gates, id).observed, 0.0, "{id}");
    }

    let mut no_summary = artifact_complete_fuzz_campaign();
    no_summary.coverage_summary = None;
    let gates = evaluate_fuzz_gates(&no_summary);
    let summary = fuzz_coverage_completeness(&no_summary);
    assert_eq!(gate_status(&gates), "failed");
    assert!(!summary.has_summary);
    assert_eq!(summary.target_coverage_ratio, 0.0);
    assert_eq!(summary.operation_coverage_ratio, 0.0);
    assert_eq!(gate(&gates, "has-case-evidence").status, "passed");
    assert_eq!(gate(&gates, "target-coverage-complete").status, "failed");
}

/// Case evidence may come from a case log, a `fuzz_report` artifact, or a
/// metadata `artifact_refs` role; a zero-declared coverage summary is
/// complete coverage.
#[test]
fn fuzz_gates_accept_each_case_evidence_channel_with_zero_declared_coverage() {
    let case_log = artifact_complete_fuzz_campaign();
    let mut fuzz_report = empty_fuzz_campaign();
    fuzz_report.coverage_summary = Some(zero_coverage_summary());
    fuzz_report.artifacts = vec![homeboy::fuzz::FuzzArtifact {
        schema: homeboy::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
        id: "case-evidence-report".to_string(),
        kind: "fuzz_report".to_string(),
        artifact: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }];
    let mut metadata_refs = empty_fuzz_campaign();
    metadata_refs.coverage_summary = Some(zero_coverage_summary());
    metadata_refs.metadata = serde_json::json!({
        "artifact_refs": [{
            "name": "case_evidence_report",
            "path": "case-evidence/report.json",
            "role": "fuzz_report"
        }]
    });

    for (label, campaign) in [
        ("case log", case_log),
        ("fuzz report artifact", fuzz_report),
        ("metadata artifact ref", metadata_refs),
    ] {
        let gates = evaluate_fuzz_gates(&campaign);
        let summary = fuzz_coverage_completeness(&campaign);

        assert_eq!(gate_status(&gates), "passed", "{label}");
        for id in [
            "has-case-evidence",
            "target-coverage-complete",
            "operation-coverage-complete",
        ] {
            assert_eq!(gate(&gates, id).status, "passed", "{label}: {id}");
            assert_eq!(gate(&gates, id).observed, 1.0, "{label}: {id}");
        }
        assert!(summary.has_summary, "{label}");
        assert_eq!(summary.target_coverage_ratio, 1.0, "{label}");
        assert_eq!(summary.operation_coverage_ratio, 1.0, "{label}");
    }
}

#[test]
fn fuzz_gate_evaluation_requires_complete_target_and_operation_coverage() {
    let mut campaign = artifact_complete_fuzz_campaign();
    campaign.coverage_summary = Some(coverage_summary(2, 1, 4, 4));

    let gates = evaluate_fuzz_gates(&campaign);

    assert_eq!(gate(&gates, "target-coverage-complete").status, "failed");
    assert_eq!(gate(&gates, "target-coverage-complete").observed, 0.5);
    assert_eq!(gate(&gates, "operation-coverage-complete").status, "passed");
    assert_eq!(gate(&gates, "operation-coverage-complete").observed, 1.0);

    campaign.coverage_summary = Some(coverage_summary(2, 2, 4, 4));
    assert_eq!(gate_status(&evaluate_fuzz_gates(&campaign)), "passed");
}

#[test]
fn fuzz_coverage_completeness_reports_summary_counts() {
    let mut campaign = empty_fuzz_campaign();
    campaign.coverage_summary = Some(homeboy::fuzz::FuzzCoverageSummary {
        schema: homeboy::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
        declared_targets: 2,
        executable_targets: 1,
        proven_targets: 1,
        declared_operations: 0,
        executable_operations: 0,
        proven_operations: 0,
        skipped_targets: vec![homeboy::fuzz::FuzzCoverageSkip {
            id: "target-2".to_string(),
            reason: "auth_required".to_string(),
            label: None,
        }],
        skipped_operations: vec![homeboy::fuzz::FuzzCoverageSkip {
            id: "operation-2".to_string(),
            reason: "config_required".to_string(),
            label: None,
        }],
        surface_summaries: vec![homeboy::fuzz::FuzzCoverageGroupSummary {
            id: "surface-a".to_string(),
            kind: "api".to_string(),
            label: Some("Surface A".to_string()),
            declared_targets: 2,
            executable_targets: 1,
            proven_targets: 1,
            declared_operations: 2,
            executable_operations: 1,
            proven_operations: 1,
            skipped_targets: vec![homeboy::fuzz::FuzzCoverageSkip {
                id: "target-2".to_string(),
                reason: "auth_required".to_string(),
                label: None,
            }],
            skipped_operations: vec![homeboy::fuzz::FuzzCoverageSkip {
                id: "operation-2".to_string(),
                reason: "config_required".to_string(),
                label: None,
            }],
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }],
        kind_summaries: vec![homeboy::fuzz::FuzzCoverageGroupSummary {
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
    });

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
