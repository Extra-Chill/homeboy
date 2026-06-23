use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::fuzz::{parse_fuzz_result_envelope_file, FuzzResultEnvelope};

use super::report::{
    evaluate_fuzz_result_envelope_gates, fuzz_coverage_completeness, gate_status,
    required_artifact_count,
};
use super::types::{
    FuzzCompareArgs, FuzzCompareDeltas, FuzzCompareOutput, FuzzCompareSnapshot,
    FuzzGateStatusChange,
};

const FUZZ_COMPARE_SCHEMA: &str = "homeboy/fuzz-compare/v1";

pub(super) fn run_compare(args: FuzzCompareArgs) -> homeboy::core::Result<FuzzCompareOutput> {
    let baseline_envelope = parse_fuzz_result_envelope_file(&args.baseline)?;
    let candidate_envelope = parse_fuzz_result_envelope_file(&args.candidate)?;
    let baseline = snapshot(&baseline_envelope);
    let candidate = snapshot(&candidate_envelope);
    let deltas = deltas(&baseline, &candidate);
    let regressions = regressions(&deltas);
    let improvements = improvements(&deltas);
    let status = if regressions.is_empty() {
        if improvements.is_empty() {
            "same"
        } else {
            "better"
        }
    } else {
        "worse"
    }
    .to_string();

    Ok(FuzzCompareOutput {
        schema: FUZZ_COMPARE_SCHEMA.to_string(),
        command: "fuzz.compare".to_string(),
        status: status.clone(),
        baseline_file: args.baseline.to_string_lossy().to_string(),
        candidate_file: args.candidate.to_string_lossy().to_string(),
        baseline,
        candidate,
        deltas,
        regressions,
        improvements,
        summary: vec![format!("fuzz compare status: {status}")],
    })
}

fn snapshot(envelope: &FuzzResultEnvelope) -> FuzzCompareSnapshot {
    let gates = evaluate_fuzz_result_envelope_gates(envelope);
    let gate_statuses = gates
        .iter()
        .map(|gate| (gate.gate_id.clone(), gate.status.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut gate_status_counts = BTreeMap::new();
    for gate in &gates {
        *gate_status_counts.entry(gate.status.clone()).or_default() += 1;
    }
    let coverage = envelope.campaign.as_ref().map(fuzz_coverage_completeness);
    let missing_required_artifacts = envelope
        .required_artifacts
        .iter()
        .filter(|artifact| {
            artifact.required && required_artifact_count(envelope, &artifact.kind) == 0
        })
        .map(|artifact| artifact.kind.clone())
        .collect::<Vec<_>>();

    let case_status_counts = case_status_counts(envelope);
    let case_count = envelope
        .campaign
        .as_ref()
        .map(|campaign| campaign.cases.len())
        .unwrap_or_default();
    let failure_count = case_status_counts
        .iter()
        .filter(|(status, _)| is_failure_status(status))
        .map(|(_, count)| *count)
        .sum::<usize>();
    let failure_rate = if case_count == 0 {
        0.0
    } else {
        failure_count as f64 / case_count as f64
    };

    FuzzCompareSnapshot {
        envelope_id: envelope.id.clone(),
        status: gate_status(&gates),
        campaign_id: envelope
            .campaign
            .as_ref()
            .map(|campaign| campaign.id.clone()),
        target_coverage_ratio: coverage
            .as_ref()
            .map(|coverage| coverage.target_coverage_ratio)
            .unwrap_or_default(),
        operation_coverage_ratio: coverage
            .as_ref()
            .map(|coverage| coverage.operation_coverage_ratio)
            .unwrap_or_default(),
        declared_targets: coverage
            .as_ref()
            .map(|coverage| coverage.declared_targets)
            .unwrap_or_default(),
        proven_targets: coverage
            .as_ref()
            .map(|coverage| coverage.proven_targets)
            .unwrap_or_default(),
        declared_operations: coverage
            .as_ref()
            .map(|coverage| coverage.declared_operations)
            .unwrap_or_default(),
        proven_operations: coverage
            .as_ref()
            .map(|coverage| coverage.proven_operations)
            .unwrap_or_default(),
        case_count,
        case_status_counts,
        failure_rate,
        finding_severity_counts: finding_severity_counts(envelope),
        critical_finding_keys: critical_finding_keys(envelope),
        missing_required_artifacts,
        gate_status_counts,
        gate_statuses,
    }
}

fn case_status_counts(envelope: &FuzzResultEnvelope) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    if let Some(campaign) = envelope.campaign.as_ref() {
        for case in &campaign.cases {
            let status = case
                .metadata
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .trim()
                .to_ascii_lowercase();
            *counts.entry(status).or_default() += 1;
        }
    }
    counts
}

fn finding_severity_counts(envelope: &FuzzResultEnvelope) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    if let Some(campaign) = envelope.campaign.as_ref() {
        for finding in &campaign.findings {
            *counts
                .entry(finding.severity.trim().to_ascii_lowercase())
                .or_default() += 1;
        }
    }
    counts
}

fn critical_finding_keys(envelope: &FuzzResultEnvelope) -> Vec<String> {
    let mut keys = envelope
        .campaign
        .as_ref()
        .map(|campaign| {
            campaign
                .findings
                .iter()
                .filter(|finding| finding.severity.eq_ignore_ascii_case("critical"))
                .map(|finding| finding.fingerprint.as_ref().unwrap_or(&finding.id).clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    keys.sort();
    keys.dedup();
    keys
}

fn deltas(baseline: &FuzzCompareSnapshot, candidate: &FuzzCompareSnapshot) -> FuzzCompareDeltas {
    let baseline_missing = baseline
        .missing_required_artifacts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let candidate_missing = candidate
        .missing_required_artifacts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let baseline_critical = baseline
        .critical_finding_keys
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let candidate_critical = candidate
        .critical_finding_keys
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    FuzzCompareDeltas {
        target_coverage_ratio: candidate.target_coverage_ratio - baseline.target_coverage_ratio,
        operation_coverage_ratio: candidate.operation_coverage_ratio
            - baseline.operation_coverage_ratio,
        declared_targets: candidate.declared_targets as i64 - baseline.declared_targets as i64,
        proven_targets: candidate.proven_targets as i64 - baseline.proven_targets as i64,
        declared_operations: candidate.declared_operations as i64
            - baseline.declared_operations as i64,
        proven_operations: candidate.proven_operations as i64 - baseline.proven_operations as i64,
        case_count: candidate.case_count as i64 - baseline.case_count as i64,
        case_status_counts: count_deltas(
            &baseline.case_status_counts,
            &candidate.case_status_counts,
        ),
        failure_rate: candidate.failure_rate - baseline.failure_rate,
        finding_severity_counts: count_deltas(
            &baseline.finding_severity_counts,
            &candidate.finding_severity_counts,
        ),
        missing_required_artifacts: candidate_missing
            .difference(&baseline_missing)
            .cloned()
            .collect(),
        resolved_required_artifacts: baseline_missing
            .difference(&candidate_missing)
            .cloned()
            .collect(),
        new_critical_findings: candidate_critical
            .difference(&baseline_critical)
            .cloned()
            .collect(),
        resolved_critical_findings: baseline_critical
            .difference(&candidate_critical)
            .cloned()
            .collect(),
        gate_status_changes: gate_status_changes(&baseline.gate_statuses, &candidate.gate_statuses),
    }
}

fn count_deltas(
    baseline: &BTreeMap<String, usize>,
    candidate: &BTreeMap<String, usize>,
) -> BTreeMap<String, i64> {
    baseline
        .keys()
        .chain(candidate.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|key| {
            let delta = candidate.get(&key).copied().unwrap_or_default() as i64
                - baseline.get(&key).copied().unwrap_or_default() as i64;
            (key, delta)
        })
        .collect()
}

fn gate_status_changes(
    baseline: &BTreeMap<String, String>,
    candidate: &BTreeMap<String, String>,
) -> Vec<FuzzGateStatusChange> {
    baseline
        .keys()
        .chain(candidate.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|gate_id| {
            let before = baseline.get(&gate_id).cloned();
            let after = candidate.get(&gate_id).cloned();
            (before != after).then_some(FuzzGateStatusChange {
                gate_id,
                baseline: before,
                candidate: after,
            })
        })
        .collect()
}

fn regressions(deltas: &FuzzCompareDeltas) -> Vec<String> {
    let mut regressions = Vec::new();
    if deltas.target_coverage_ratio < 0.0 {
        regressions.push("target coverage dropped".to_string());
    }
    if deltas.operation_coverage_ratio < 0.0 {
        regressions.push("operation coverage dropped".to_string());
    }
    if deltas.failure_rate > 0.0 {
        regressions.push("failure rate increased".to_string());
    }
    if !deltas.new_critical_findings.is_empty() {
        regressions.push("new critical findings".to_string());
    }
    if !deltas.missing_required_artifacts.is_empty() {
        regressions.push("new missing required artifacts".to_string());
    }
    if deltas.gate_status_changes.iter().any(|change| {
        change.baseline.as_deref() == Some("passed")
            && change.candidate.as_deref() == Some("failed")
    }) {
        regressions.push("gate changed from passed to failed".to_string());
    }
    regressions
}

fn improvements(deltas: &FuzzCompareDeltas) -> Vec<String> {
    let mut improvements = Vec::new();
    if deltas.target_coverage_ratio > 0.0 {
        improvements.push("target coverage improved".to_string());
    }
    if deltas.operation_coverage_ratio > 0.0 {
        improvements.push("operation coverage improved".to_string());
    }
    if deltas.failure_rate < 0.0 {
        improvements.push("failure rate decreased".to_string());
    }
    if !deltas.resolved_critical_findings.is_empty() {
        improvements.push("critical findings resolved".to_string());
    }
    if !deltas.resolved_required_artifacts.is_empty() {
        improvements.push("required artifacts restored".to_string());
    }
    if deltas.gate_status_changes.iter().any(|change| {
        change.baseline.as_deref() == Some("failed")
            && change.candidate.as_deref() == Some("passed")
    }) {
        improvements.push("gate changed from failed to passed".to_string());
    }
    improvements
}

fn is_failure_status(status: &str) -> bool {
    matches!(
        status,
        "failed" | "failure" | "error" | "crashed" | "crash" | "timeout" | "panic"
    )
}

#[cfg(test)]
mod tests {
    use homeboy::core::artifact_contract::ArtifactContract;
    use homeboy::core::fuzz::{
        default_fuzz_gates, default_fuzz_required_artifacts, FuzzArtifact, FuzzCampaign, FuzzCase,
        FuzzCoverageSummary, FuzzExecutionRequest, FuzzFinding, FuzzFindingStatus,
        FuzzResultEnvelope, FuzzSafetyClass, FUZZ_ARTIFACT_SCHEMA, FUZZ_CAMPAIGN_SCHEMA,
        FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA,
    };

    use super::*;

    #[test]
    fn fuzz_compare_reports_same_when_candidate_matches_baseline() {
        let baseline = snapshot(&envelope("baseline", 2, 2, 4, 4, &[], &[], true));
        let candidate = snapshot(&envelope("candidate", 2, 2, 4, 4, &[], &[], true));
        let deltas = deltas(&baseline, &candidate);

        assert!(regressions(&deltas).is_empty());
        assert!(improvements(&deltas).is_empty());
    }

    #[test]
    fn fuzz_compare_reports_better_candidate() {
        let baseline = snapshot(&envelope("baseline", 4, 3, 8, 6, &["failed"], &[], false));
        let candidate = snapshot(&envelope("candidate", 4, 4, 8, 8, &["passed"], &[], true));
        let deltas = deltas(&baseline, &candidate);

        assert!(regressions(&deltas).is_empty());
        assert!(improvements(&deltas).contains(&"target coverage improved".to_string()));
        assert!(improvements(&deltas).contains(&"operation coverage improved".to_string()));
        assert!(improvements(&deltas).contains(&"failure rate decreased".to_string()));
        assert!(improvements(&deltas).contains(&"required artifacts restored".to_string()));
    }

    #[test]
    fn fuzz_compare_reports_worse_candidate() {
        let baseline = snapshot(&envelope("baseline", 4, 4, 8, 8, &["passed"], &[], true));
        let candidate = snapshot(&envelope(
            "candidate",
            4,
            3,
            8,
            6,
            &["passed", "failed"],
            &["critical"],
            false,
        ));
        let deltas = deltas(&baseline, &candidate);
        let regressions = regressions(&deltas);

        assert!(regressions.contains(&"target coverage dropped".to_string()));
        assert!(regressions.contains(&"operation coverage dropped".to_string()));
        assert!(regressions.contains(&"failure rate increased".to_string()));
        assert!(regressions.contains(&"new critical findings".to_string()));
        assert!(regressions.contains(&"new missing required artifacts".to_string()));
        assert!(regressions.contains(&"gate changed from passed to failed".to_string()));
    }

    fn envelope(
        id: &str,
        declared_targets: u64,
        proven_targets: u64,
        declared_operations: u64,
        proven_operations: u64,
        case_statuses: &[&str],
        severities: &[&str],
        include_case_log: bool,
    ) -> FuzzResultEnvelope {
        let campaign = FuzzCampaign {
            schema: FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: format!("{id}-campaign"),
            title: None,
            safety_class: FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            cases: case_statuses
                .iter()
                .enumerate()
                .map(|(index, status)| FuzzCase {
                    schema: homeboy::core::fuzz::FUZZ_CASE_SCHEMA.to_string(),
                    id: format!("case-{index}"),
                    target_id: None,
                    operation_id: None,
                    workload_id: None,
                    seed_id: None,
                    replay_id: None,
                    input: serde_json::Value::Null,
                    expected: serde_json::Value::Null,
                    observed: serde_json::Value::Null,
                    metadata: serde_json::json!({ "status": status }),
                    extra: BTreeMap::new(),
                })
                .collect(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            coverage_summary: Some(FuzzCoverageSummary {
                schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets,
                executable_targets: declared_targets,
                proven_targets,
                declared_operations,
                executable_operations: declared_operations,
                proven_operations,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                surface_summaries: Vec::new(),
                kind_summaries: Vec::new(),
                artifact_ids: Vec::new(),
                metadata: serde_json::Value::Null,
                extra: BTreeMap::new(),
            }),
            findings: severities
                .iter()
                .enumerate()
                .map(|(index, severity)| FuzzFinding {
                    schema: homeboy::core::fuzz::FUZZ_FINDING_SCHEMA.to_string(),
                    id: format!("finding-{index}"),
                    title: format!("finding {index}"),
                    severity: severity.to_string(),
                    status: FuzzFindingStatus::Open,
                    surface_id: None,
                    target_id: None,
                    operation_id: None,
                    case_id: None,
                    workload_id: None,
                    seed_id: None,
                    fingerprint: Some(format!("fingerprint-{index}")),
                    artifact_ids: Vec::new(),
                    metadata: serde_json::Value::Null,
                    extra: BTreeMap::new(),
                })
                .collect(),
            artifacts: if include_case_log {
                vec![FuzzArtifact {
                    schema: FUZZ_ARTIFACT_SCHEMA.to_string(),
                    id: "case-log".to_string(),
                    kind: "case_log".to_string(),
                    artifact: Some(ArtifactContract {
                        schema: homeboy::core::artifact_contract::ARTIFACT_CONTRACT_SCHEMA
                            .to_string(),
                        kind: "case_log".to_string(),
                        artifact_type: "file".to_string(),
                        path: Some("case-log.json".to_string()),
                        url: None,
                        public_url: None,
                        role: None,
                        label: None,
                        semantic_key: None,
                        sha256: None,
                        size_bytes: None,
                        metadata: serde_json::Value::Null,
                        extra: BTreeMap::new(),
                    }),
                    metadata: serde_json::Value::Null,
                    extra: BTreeMap::new(),
                }]
            } else {
                Vec::new()
            },
            thresholds: Vec::new(),
            provenance: None,
            replay: None,
            lifecycle: None,
            metadata: serde_json::Value::Null,
            extra: BTreeMap::new(),
        };

        FuzzResultEnvelope {
            schema: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: id.to_string(),
            status: "pending".to_string(),
            request: FuzzExecutionRequest {
                schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
                version: FUZZ_CONTRACT_VERSION,
                id: format!("{id}-request"),
                component: "component".to_string(),
                rig_id: None,
                workload_id: None,
                case_ids: Vec::new(),
                seed: None,
                max_duration: None,
                args: Vec::new(),
                required_artifacts: default_fuzz_required_artifacts(),
                gates: default_fuzz_gates(),
                metadata: serde_json::Value::Null,
                extra: BTreeMap::new(),
            },
            campaign: Some(campaign.clone()),
            artifacts: campaign.artifacts.clone(),
            required_artifacts: default_fuzz_required_artifacts(),
            gates: default_fuzz_gates(),
            provenance: None,
            metadata: serde_json::Value::Null,
            extra: BTreeMap::new(),
        }
    }
}
