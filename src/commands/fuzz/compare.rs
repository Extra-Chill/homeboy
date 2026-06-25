use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::fuzz::{
    parse_fuzz_hotspot_set_value, parse_fuzz_observation_set_value,
    parse_fuzz_result_envelope_file, FuzzHotspot, FuzzObservationFamily, FuzzResultEnvelope,
};

use super::report::{
    evaluate_fuzz_result_envelope_gates, fuzz_coverage_completeness, gate_status,
    required_artifact_count,
};
use super::types::{
    FuzzCompareArgs, FuzzCompareDeltas, FuzzCompareHotspotDelta, FuzzCompareHotspotPolicy,
    FuzzCompareHotspotSnapshot, FuzzCompareHotspotSummary, FuzzCompareOutput, FuzzCompareSnapshot,
    FuzzGateStatusChange,
};

const FUZZ_COMPARE_SCHEMA: &str = "homeboy/fuzz-compare/v1";

pub(super) fn run_compare(args: FuzzCompareArgs) -> homeboy::core::Result<FuzzCompareOutput> {
    let baseline_envelope = parse_fuzz_result_envelope_file(&args.baseline)?;
    let candidate_envelope = parse_fuzz_result_envelope_file(&args.candidate)?;
    compare_envelopes(
        baseline_envelope,
        candidate_envelope,
        args.hotspot_policy,
        args.baseline.to_string_lossy().to_string(),
        args.candidate.to_string_lossy().to_string(),
        "fuzz.compare",
    )
}

pub(crate) fn compare_envelopes(
    baseline_envelope: FuzzResultEnvelope,
    candidate_envelope: FuzzResultEnvelope,
    hotspot_policy: FuzzCompareHotspotPolicy,
    baseline_ref: String,
    candidate_ref: String,
    command: &str,
) -> homeboy::core::Result<FuzzCompareOutput> {
    let baseline = snapshot(&baseline_envelope);
    let candidate = snapshot(&candidate_envelope);
    let deltas = deltas(&baseline, &candidate, hotspot_policy);
    let hotspot_summary = hotspot_summary(&deltas, hotspot_policy);
    let regressions = regressions(&deltas);
    let advisories = advisories(&deltas);
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
        command: command.to_string(),
        status: status.clone(),
        advisory_status: advisory_status(&status, &advisories),
        baseline_file: baseline_ref,
        candidate_file: candidate_ref,
        baseline,
        candidate,
        deltas,
        hotspot_summary,
        regressions,
        advisories,
        improvements,
        summary: vec![
            format!("fuzz compare status: {status}"),
            format!("hotspot policy: {}", hotspot_policy.as_str()),
        ],
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
        hotspots: hotspot_snapshots(envelope),
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

fn deltas(
    baseline: &FuzzCompareSnapshot,
    candidate: &FuzzCompareSnapshot,
    hotspot_policy: FuzzCompareHotspotPolicy,
) -> FuzzCompareDeltas {
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
    let hotspot_deltas = hotspot_deltas(&baseline.hotspots, &candidate.hotspots, hotspot_policy);
    let new_hotspots = hotspot_deltas
        .iter()
        .filter(|delta| delta.status == "new")
        .map(|delta| delta.id.clone())
        .collect();
    let resolved_hotspots = hotspot_deltas
        .iter()
        .filter(|delta| delta.status == "resolved")
        .map(|delta| delta.id.clone())
        .collect();

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
        hotspot_deltas,
        new_hotspots,
        resolved_hotspots,
        gate_status_changes: gate_status_changes(&baseline.gate_statuses, &candidate.gate_statuses),
    }
}

fn hotspot_snapshots(envelope: &FuzzResultEnvelope) -> Vec<FuzzCompareHotspotSnapshot> {
    let mut hotspots = Vec::new();
    collect_hotspots_from_value(
        &serde_json::to_value(envelope).unwrap_or_default(),
        &mut hotspots,
    );
    hotspots.sort_by(|a, b| {
        a.rank
            .unwrap_or(u64::MAX)
            .cmp(&b.rank.unwrap_or(u64::MAX))
            .then_with(|| {
                b.relative_score
                    .unwrap_or(b.value)
                    .total_cmp(&a.relative_score.unwrap_or(a.value))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    hotspots
}

fn collect_hotspots_from_value(
    value: &serde_json::Value,
    hotspots: &mut Vec<FuzzCompareHotspotSnapshot>,
) {
    if let Some(set) = parse_fuzz_hotspot_set_value(value) {
        hotspots.extend(set.items.into_iter().map(hotspot_snapshot));
        return;
    }
    if let Some(set) = parse_fuzz_observation_set_value(value) {
        hotspots.extend(set.observations.into_iter().map(|observation| {
            let dimension = match observation.family {
                FuzzObservationFamily::Action => "action",
                FuzzObservationFamily::Query => "query",
                FuzzObservationFamily::Resource => "resource",
                FuzzObservationFamily::Timing => "timing",
                FuzzObservationFamily::Counter => "counter",
            }
            .to_string();
            let id = observation.fingerprint.clone().unwrap_or_else(|| {
                [
                    Some(dimension.as_str()),
                    Some(observation.subject.as_str()),
                    Some(observation.metric.as_str()),
                    observation.operation_id.as_deref(),
                    observation.case_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(":")
            });
            FuzzCompareHotspotSnapshot {
                id,
                dimension,
                kind: Some("observation".to_string()),
                metric: observation.metric,
                value: observation.value,
                unit: observation.unit,
                basis: Some("fuzz_observation_set".to_string()),
                sample_count: observation.sample_count,
                rank: None,
                relative_score: Some(observation.value.abs()),
                label: Some(observation.subject),
            }
        }));
        return;
    }

    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_hotspots_from_value(item, hotspots);
            }
        }
        serde_json::Value::Object(object) => {
            for (key, item) in object {
                if key != "prior_observations" {
                    collect_hotspots_from_value(item, hotspots);
                }
            }
        }
        _ => {}
    }
}

fn hotspot_snapshot(hotspot: FuzzHotspot) -> FuzzCompareHotspotSnapshot {
    FuzzCompareHotspotSnapshot {
        id: hotspot.id,
        dimension: hotspot.dimension,
        kind: hotspot.kind,
        metric: hotspot.metric,
        value: hotspot.value,
        unit: hotspot.unit,
        basis: hotspot.basis,
        sample_count: hotspot.sample_count,
        rank: hotspot.rank,
        relative_score: hotspot.relative_score,
        label: hotspot.label,
    }
}

fn hotspot_deltas(
    baseline: &[FuzzCompareHotspotSnapshot],
    candidate: &[FuzzCompareHotspotSnapshot],
    policy: FuzzCompareHotspotPolicy,
) -> Vec<FuzzCompareHotspotDelta> {
    let baseline_by_id = baseline
        .iter()
        .map(|hotspot| (hotspot.id.clone(), hotspot))
        .collect::<BTreeMap<_, _>>();
    let candidate_by_id = candidate
        .iter()
        .map(|hotspot| (hotspot.id.clone(), hotspot))
        .collect::<BTreeMap<_, _>>();

    baseline_by_id
        .keys()
        .chain(candidate_by_id.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            let before = baseline_by_id.get(&id).copied();
            let after = candidate_by_id.get(&id).copied();
            let status = match (before, after) {
                (None, Some(_)) => "new",
                (Some(_), None) => "resolved",
                (Some(_), Some(_)) => "changed",
                (None, None) => "absent",
            }
            .to_string();
            let mut delta = FuzzCompareHotspotDelta {
                id: id.clone(),
                dimension: after
                    .or(before)
                    .map(|hotspot| hotspot.dimension.clone())
                    .unwrap_or_default(),
                metric: after
                    .or(before)
                    .map(|hotspot| hotspot.metric.clone())
                    .unwrap_or_default(),
                baseline_value: before.map(|hotspot| hotspot.value),
                candidate_value: after.map(|hotspot| hotspot.value),
                value_delta: before
                    .zip(after)
                    .map(|(before, after)| after.value - before.value),
                baseline_rank: before.and_then(|hotspot| hotspot.rank),
                candidate_rank: after.and_then(|hotspot| hotspot.rank),
                rank_delta: before
                    .and_then(|hotspot| hotspot.rank)
                    .zip(after.and_then(|hotspot| hotspot.rank))
                    .map(|(before, after)| after as i64 - before as i64),
                baseline_relative_score: before.and_then(|hotspot| hotspot.relative_score),
                candidate_relative_score: after.and_then(|hotspot| hotspot.relative_score),
                relative_score_delta: before
                    .and_then(|hotspot| hotspot.relative_score)
                    .zip(after.and_then(|hotspot| hotspot.relative_score))
                    .map(|(before, after)| after - before),
                status,
                classification: String::new(),
            };
            delta.classification = hotspot_classification(&delta, policy);
            delta
        })
        .collect()
}

fn hotspot_classification(
    delta: &FuzzCompareHotspotDelta,
    policy: FuzzCompareHotspotPolicy,
) -> String {
    match hotspot_direction(delta) {
        HotspotDirection::Regression => match policy {
            FuzzCompareHotspotPolicy::Advisory => "advisory_regression",
            FuzzCompareHotspotPolicy::Blocking => "blocking_regression",
            FuzzCompareHotspotPolicy::Off => "measured_regression",
        },
        HotspotDirection::Improvement => match policy {
            FuzzCompareHotspotPolicy::Advisory => "advisory_improvement",
            FuzzCompareHotspotPolicy::Blocking => "blocking_improvement",
            FuzzCompareHotspotPolicy::Off => "measured_improvement",
        },
        HotspotDirection::Unchanged => "unchanged",
    }
    .to_string()
}

#[derive(PartialEq, Eq)]
enum HotspotDirection {
    Regression,
    Improvement,
    Unchanged,
}

fn hotspot_direction(delta: &FuzzCompareHotspotDelta) -> HotspotDirection {
    if delta.status == "new" {
        return HotspotDirection::Regression;
    }
    if delta.status == "resolved" {
        return HotspotDirection::Improvement;
    }

    let rank_regressed = delta.rank_delta.is_some_and(|change| change < 0);
    let rank_improved = delta.rank_delta.is_some_and(|change| change > 0);
    let score_regressed = delta
        .relative_score_delta
        .is_some_and(|change| change > 0.0);
    let score_improved = delta
        .relative_score_delta
        .is_some_and(|change| change < 0.0);
    let value_regressed = delta.value_delta.is_some_and(|change| change > 0.0);
    let value_improved = delta.value_delta.is_some_and(|change| change < 0.0);

    if rank_regressed || score_regressed || value_regressed {
        HotspotDirection::Regression
    } else if rank_improved || score_improved || value_improved {
        HotspotDirection::Improvement
    } else {
        HotspotDirection::Unchanged
    }
}

fn hotspot_summary(
    deltas: &FuzzCompareDeltas,
    policy: FuzzCompareHotspotPolicy,
) -> FuzzCompareHotspotSummary {
    let advisory_regressions = deltas
        .hotspot_deltas
        .iter()
        .filter(|delta| delta.classification == "advisory_regression")
        .count();
    let blocking_regressions = deltas
        .hotspot_deltas
        .iter()
        .filter(|delta| delta.classification == "blocking_regression")
        .count();
    let measured_regressions = deltas
        .hotspot_deltas
        .iter()
        .filter(|delta| delta.classification == "measured_regression")
        .count();
    let improvements = deltas
        .hotspot_deltas
        .iter()
        .filter(|delta| delta.classification.ends_with("_improvement"))
        .count();
    let regressions = advisory_regressions + blocking_regressions + measured_regressions;

    FuzzCompareHotspotSummary {
        policy: policy.as_str().to_string(),
        status: if blocking_regressions > 0 {
            "blocking_regression"
        } else if advisory_regressions > 0 {
            "advisory_regression"
        } else if measured_regressions > 0 {
            "measured_regression"
        } else if improvements > 0 {
            "improved"
        } else {
            "same"
        }
        .to_string(),
        total: deltas.hotspot_deltas.len(),
        regressions,
        advisory_regressions,
        blocking_regressions,
        improvements,
        new_hotspots: deltas.new_hotspots.len(),
        resolved_hotspots: deltas.resolved_hotspots.len(),
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
    if deltas
        .hotspot_deltas
        .iter()
        .any(|delta| delta.classification == "blocking_regression")
    {
        regressions.push("hotspot regressions".to_string());
    }
    regressions
}

fn advisories(deltas: &FuzzCompareDeltas) -> Vec<String> {
    let mut advisories = Vec::new();
    let hotspot_regressions = deltas
        .hotspot_deltas
        .iter()
        .filter(|delta| delta.classification == "advisory_regression")
        .count();
    if hotspot_regressions > 0 {
        advisories.push(format!("hotspot regressions: {hotspot_regressions}"));
    }
    advisories
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
    if deltas
        .hotspot_deltas
        .iter()
        .any(|delta| delta.classification == "blocking_improvement")
    {
        improvements.push("hotspots improved".to_string());
    }
    improvements
}

fn advisory_status(status: &str, advisories: &[String]) -> String {
    if status == "worse" || !advisories.is_empty() {
        "worse"
    } else {
        status
    }
    .to_string()
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
        let deltas = deltas(&baseline, &candidate, FuzzCompareHotspotPolicy::Advisory);

        assert!(regressions(&deltas).is_empty());
        assert!(improvements(&deltas).is_empty());
    }

    #[test]
    fn fuzz_compare_reports_better_candidate() {
        let baseline = snapshot(&envelope("baseline", 4, 3, 8, 6, &["failed"], &[], false));
        let candidate = snapshot(&envelope("candidate", 4, 4, 8, 8, &["passed"], &[], true));
        let deltas = deltas(&baseline, &candidate, FuzzCompareHotspotPolicy::Advisory);

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
        let deltas = deltas(&baseline, &candidate, FuzzCompareHotspotPolicy::Advisory);
        let regressions = regressions(&deltas);

        assert!(regressions.contains(&"target coverage dropped".to_string()));
        assert!(regressions.contains(&"operation coverage dropped".to_string()));
        assert!(regressions.contains(&"failure rate increased".to_string()));
        assert!(regressions.contains(&"new critical findings".to_string()));
        assert!(regressions.contains(&"new missing required artifacts".to_string()));
        assert!(regressions.contains(&"gate changed from passed to failed".to_string()));
    }

    #[test]
    fn fuzz_compare_reports_relative_hotspot_movement_as_advisory_by_default() {
        let mut baseline = envelope("baseline", 0, 0, 0, 0, &[], &[], true);
        baseline.metadata = serde_json::json!({
            "hotspots": {
                "schema": "homeboy/fuzz-hotspot-set/v1",
                "id": "baseline-hotspots",
                "items": [
                    {
                        "id": "feature:alpha",
                        "dimension": "feature",
                        "kind": "request",
                        "metric": "duration",
                        "value": 100.0,
                        "unit": "ms",
                        "basis": "per_case",
                        "sample_count": 12,
                        "rank": 1,
                        "relative_score": 0.9
                    },
                    {
                        "id": "query:old",
                        "dimension": "query",
                        "metric": "count",
                        "value": 9.0,
                        "unit": "queries",
                        "rank": 2
                    }
                ]
            }
        });
        baseline.gates.clear();
        baseline.required_artifacts.clear();

        let mut candidate = envelope("candidate", 0, 0, 0, 0, &[], &[], true);
        candidate.metadata = serde_json::json!({
            "hotspots": {
                "schema": "homeboy/fuzz-hotspot-set/v1",
                "id": "candidate-hotspots",
                "items": [
                    {
                        "id": "feature:alpha",
                        "dimension": "feature",
                        "kind": "request",
                        "metric": "duration",
                        "value": 125.0,
                        "unit": "ms",
                        "basis": "per_case",
                        "sample_count": 14,
                        "rank": 3,
                        "relative_score": 0.7
                    },
                    {
                        "id": "page:new",
                        "dimension": "page",
                        "metric": "duration",
                        "value": 88.0,
                        "unit": "ms",
                        "rank": 1
                    }
                ]
            }
        });
        candidate.gates.clear();
        candidate.required_artifacts.clear();

        let baseline = snapshot(&baseline);
        let candidate = snapshot(&candidate);
        let deltas = deltas(&baseline, &candidate, FuzzCompareHotspotPolicy::Advisory);
        let summary = hotspot_summary(&deltas, FuzzCompareHotspotPolicy::Advisory);

        assert!(regressions(&deltas).is_empty());
        assert_eq!(advisories(&deltas), vec!["hotspot regressions: 2"]);
        assert!(improvements(&deltas).is_empty());
        assert_eq!(summary.policy, "advisory");
        assert_eq!(summary.status, "advisory_regression");
        assert_eq!(summary.advisory_regressions, 2);
        assert_eq!(summary.blocking_regressions, 0);
        assert_eq!(summary.improvements, 1);
        assert_eq!(deltas.new_hotspots, vec!["page:new"]);
        assert_eq!(deltas.resolved_hotspots, vec!["query:old"]);
        let alpha = deltas
            .hotspot_deltas
            .iter()
            .find(|delta| delta.id == "feature:alpha")
            .expect("alpha delta");
        assert_eq!(alpha.value_delta, Some(25.0));
        assert_eq!(alpha.rank_delta, Some(2));
        assert_eq!(alpha.classification, "advisory_regression");
        assert!(
            (alpha.relative_score_delta.expect("relative score delta") + 0.2).abs() < f64::EPSILON
        );
    }

    #[test]
    fn fuzz_compare_promotes_observation_sets_to_hotspots() {
        let mut envelope = envelope("candidate", 0, 0, 0, 0, &[], &[], true);
        envelope.metadata = serde_json::json!({
            "observations": {
                "schema": "homeboy/fuzz-observation-set/v1",
                "version": 1,
                "id": "candidate-observations",
                "observations": [
                    {
                        "id": "observation-1",
                        "family": "timing",
                        "subject": "page-load",
                        "metric": "duration",
                        "value": 123.0,
                        "unit": "ms",
                        "sample_count": 4,
                        "fingerprint": "page-load:duration"
                    }
                ]
            }
        });

        let snapshot = snapshot(&envelope);

        assert_eq!(snapshot.hotspots.len(), 1);
        assert_eq!(snapshot.hotspots[0].id, "page-load:duration");
        assert_eq!(snapshot.hotspots[0].dimension, "timing");
        assert_eq!(snapshot.hotspots[0].kind.as_deref(), Some("observation"));
        assert_eq!(snapshot.hotspots[0].relative_score, Some(123.0));
    }

    #[test]
    fn fuzz_compare_blocking_hotspot_policy_affects_compare_status_data() {
        let mut baseline = envelope("baseline", 0, 0, 0, 0, &[], &[], true);
        baseline.metadata = serde_json::json!({
            "hotspots": {
                "schema": "homeboy/fuzz-hotspot-set/v1",
                "id": "baseline-hotspots",
                "items": [
                    { "id": "path:alpha", "dimension": "path", "metric": "duration", "value": 100.0, "unit": "ms", "rank": 2, "relative_score": 0.5 }
                ]
            }
        });
        baseline.gates.clear();
        baseline.required_artifacts.clear();

        let mut candidate = envelope("candidate", 0, 0, 0, 0, &[], &[], true);
        candidate.metadata = serde_json::json!({
            "hotspots": {
                "schema": "homeboy/fuzz-hotspot-set/v1",
                "id": "candidate-hotspots",
                "items": [
                    { "id": "path:alpha", "dimension": "path", "metric": "duration", "value": 140.0, "unit": "ms", "rank": 1, "relative_score": 0.9 }
                ]
            }
        });
        candidate.gates.clear();
        candidate.required_artifacts.clear();

        let baseline = snapshot(&baseline);
        let candidate = snapshot(&candidate);
        let deltas = deltas(&baseline, &candidate, FuzzCompareHotspotPolicy::Blocking);
        let regressions = regressions(&deltas);
        let summary = hotspot_summary(&deltas, FuzzCompareHotspotPolicy::Blocking);

        assert!(regressions.contains(&"hotspot regressions".to_string()));
        assert_eq!(summary.policy, "blocking");
        assert_eq!(summary.status, "blocking_regression");
        assert_eq!(summary.blocking_regressions, 1);
        assert_eq!(
            deltas.hotspot_deltas[0].classification,
            "blocking_regression"
        );
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
                    source_refs: Vec::new(),
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
