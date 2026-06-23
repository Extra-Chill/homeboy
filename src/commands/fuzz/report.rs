use std::collections::BTreeMap;
use std::path::Path;

use homeboy::core::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, parse_fuzz_results_file,
    parse_fuzz_target_inventory_file, FuzzCampaign, FuzzExecutionRequest, FuzzProvenance,
    FuzzResultEnvelope, FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA,
};

use super::types::{
    FuzzCoverageCompletenessOutput, FuzzCoverageSelectorSummaryOutput, FuzzGateEvaluation,
    FuzzReportArgs, FuzzReportOutput, FuzzValidateArgs, FuzzValidateOutput,
};

pub(super) fn run_validate(args: FuzzValidateArgs) -> homeboy::core::Result<FuzzValidateOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let gates = evaluate_fuzz_gates(&campaign);
    let coverage_completeness = fuzz_coverage_completeness(&campaign);

    Ok(FuzzValidateOutput {
        command: "fuzz.validate".to_string(),
        status: gate_status(&gates),
        results_file: args.results_file.to_string_lossy().to_string(),
        campaign_id: campaign.id.clone(),
        open_findings: open_finding_count(&campaign),
        artifacts: campaign.artifacts.len(),
        coverage_completeness,
        gates,
    })
}

pub(super) fn run_report(args: FuzzReportArgs) -> homeboy::core::Result<FuzzReportOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let gates = evaluate_fuzz_gates(&campaign);
    let coverage_completeness = fuzz_coverage_completeness(&campaign);
    let status = gate_status(&gates);
    let run_id = args.run.run_id.clone();
    let component = args.run.comp.id().unwrap_or("unknown").to_string();
    let request_id = args
        .run
        .run_id
        .clone()
        .or_else(|| args.run.workload_id.clone())
        .unwrap_or_else(|| format!("{}-request", campaign.id));
    let envelope_id = args
        .envelope_id
        .clone()
        .or_else(|| args.run.run_id.clone())
        .unwrap_or_else(|| campaign.id.clone());
    let metadata = fuzz_result_metadata(args.run.inventory.as_deref())?;
    let envelope = FuzzResultEnvelope {
        schema: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: envelope_id,
        status: status.clone(),
        request: FuzzExecutionRequest {
            schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: request_id,
            component,
            rig_id: args.run.rig,
            workload_id: args.run.workload_id,
            case_ids: campaign.cases.iter().map(|case| case.id.clone()).collect(),
            seed: args.run.seed,
            max_duration: args.run.max_duration,
            args: args.run.args,
            required_artifacts: default_fuzz_required_artifacts(),
            gates: default_fuzz_gates(),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        },
        campaign: Some(campaign.clone()),
        artifacts: campaign.artifacts.clone(),
        required_artifacts: default_fuzz_required_artifacts(),
        gates: default_fuzz_gates(),
        provenance: Some(fuzz_provenance(run_id)),
        metadata,
        extra: std::collections::BTreeMap::new(),
    };

    if let Some(path) = args.output_envelope.as_ref() {
        let json = serde_json::to_string_pretty(&envelope).map_err(|error| {
            homeboy::core::Error::internal_unexpected(format!(
                "failed to encode fuzz result envelope: {error}"
            ))
        })?;
        std::fs::write(path, json).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
    }

    Ok(FuzzReportOutput {
        command: "fuzz.report".to_string(),
        status,
        results_file: args.results_file.to_string_lossy().to_string(),
        envelope_file: args
            .output_envelope
            .map(|path| path.to_string_lossy().to_string()),
        envelope,
        coverage_completeness,
        gates,
    })
}

pub(super) fn fuzz_result_metadata(
    inventory_path: Option<&Path>,
) -> homeboy::core::Result<serde_json::Value> {
    let Some(path) = inventory_path else {
        return Ok(serde_json::Value::Null);
    };
    let inventory = parse_fuzz_target_inventory_file(path)?;
    Ok(serde_json::json!({
        "inventory_file": path.to_string_lossy(),
        "target_inventory": inventory,
    }))
}

pub(super) fn fuzz_provenance(run_id: Option<String>) -> FuzzProvenance {
    FuzzProvenance {
        schema: homeboy::core::fuzz::FUZZ_PROVENANCE_SCHEMA.to_string(),
        producer: "homeboy fuzz".to_string(),
        producer_version: None,
        invocation: None,
        run_id,
        source_ref: None,
        created_at: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }
}

pub(super) fn evaluate_fuzz_gates(campaign: &FuzzCampaign) -> Vec<FuzzGateEvaluation> {
    let profile = FuzzGateProfile::default();
    let metrics = FuzzGateMetrics::from_campaign(campaign, &profile);

    default_fuzz_gates()
        .into_iter()
        .map(|gate| {
            let observed = match gate.metric.as_str() {
                "open_findings" => metrics.open_findings as f64,
                "case_log_artifacts" => metrics.case_evidence_artifacts as f64,
                "target_coverage_ratio" => metrics.target_coverage_ratio,
                "operation_coverage_ratio" => metrics.operation_coverage_ratio,
                _ => 0.0,
            };
            let passed = threshold_passes(observed, gate.operator, gate.value);
            FuzzGateEvaluation {
                gate_id: gate.id,
                status: if passed { "passed" } else { "failed" }.to_string(),
                metric: gate.metric,
                observed,
                expected: gate.value,
            }
        })
        .collect()
}

struct FuzzGateProfile {
    case_evidence_kinds: &'static [&'static str],
    metadata_artifact_ref_keys: &'static [&'static str],
}

impl Default for FuzzGateProfile {
    fn default() -> Self {
        Self {
            case_evidence_kinds: &[
                "case_log",
                "fuzz_report",
                "fuzz_case",
                "case_artifact",
                "failing_case",
                "repro_case",
            ],
            metadata_artifact_ref_keys: &["artifact_refs", "artifactRefs"],
        }
    }
}

struct FuzzGateMetrics {
    open_findings: usize,
    case_evidence_artifacts: usize,
    target_coverage_ratio: f64,
    operation_coverage_ratio: f64,
}

impl FuzzGateMetrics {
    fn from_campaign(campaign: &FuzzCampaign, profile: &FuzzGateProfile) -> Self {
        Self {
            open_findings: open_finding_count(campaign),
            case_evidence_artifacts: case_evidence_artifact_count(campaign, profile),
            target_coverage_ratio: coverage_ratio(
                campaign.coverage_summary.as_ref(),
                |summary| summary.proven_targets,
                |summary| summary.declared_targets,
            ),
            operation_coverage_ratio: coverage_ratio(
                campaign.coverage_summary.as_ref(),
                |summary| summary.proven_operations,
                |summary| summary.declared_operations,
            ),
        }
    }
}

fn coverage_ratio(
    summary: Option<&homeboy::core::fuzz::FuzzCoverageSummary>,
    covered: impl Fn(&homeboy::core::fuzz::FuzzCoverageSummary) -> u64,
    total: impl Fn(&homeboy::core::fuzz::FuzzCoverageSummary) -> u64,
) -> f64 {
    let Some(summary) = summary else {
        return 1.0;
    };
    let total = total(summary);
    if total == 0 {
        1.0
    } else {
        covered(summary) as f64 / total as f64
    }
}

fn case_evidence_artifact_count(campaign: &FuzzCampaign, profile: &FuzzGateProfile) -> usize {
    let campaign_artifacts = campaign
        .artifacts
        .iter()
        .filter(|artifact| {
            profile
                .case_evidence_kinds
                .contains(&artifact.kind.as_str())
        })
        .count();
    campaign_artifacts + metadata_case_evidence_artifact_count(&campaign.metadata, profile)
}

fn metadata_case_evidence_artifact_count(
    metadata: &serde_json::Value,
    profile: &FuzzGateProfile,
) -> usize {
    profile
        .metadata_artifact_ref_keys
        .iter()
        .map(|key| metadata_artifact_refs(metadata.get(*key), profile))
        .sum()
}

fn metadata_artifact_refs(value: Option<&serde_json::Value>, profile: &FuzzGateProfile) -> usize {
    value
        .and_then(|value| value.as_array())
        .map(|artifacts| {
            artifacts
                .iter()
                .filter(|artifact| metadata_artifact_has_case_evidence_kind(artifact, profile))
                .count()
        })
        .unwrap_or(0)
}

fn metadata_artifact_has_case_evidence_kind(
    artifact: &serde_json::Value,
    profile: &FuzzGateProfile,
) -> bool {
    let kind = artifact
        .get("kind")
        .or_else(|| artifact.get("role"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    profile.case_evidence_kinds.contains(&kind)
}

pub(super) fn fuzz_coverage_completeness(
    campaign: &FuzzCampaign,
) -> FuzzCoverageCompletenessOutput {
    match campaign.coverage_summary.as_ref() {
        Some(summary) => {
            let mut skipped_reason_counts = BTreeMap::new();
            accumulate_skip_reason_counts(&mut skipped_reason_counts, &summary.skipped_targets);
            accumulate_skip_reason_counts(&mut skipped_reason_counts, &summary.skipped_operations);

            FuzzCoverageCompletenessOutput {
                has_summary: true,
                declared_targets: summary.declared_targets,
                executable_targets: summary.executable_targets,
                proven_targets: summary.proven_targets,
                target_coverage_ratio: coverage_ratio(
                    Some(summary),
                    |summary| summary.proven_targets,
                    |summary| summary.declared_targets,
                ),
                declared_operations: summary.declared_operations,
                executable_operations: summary.executable_operations,
                proven_operations: summary.proven_operations,
                operation_coverage_ratio: coverage_ratio(
                    Some(summary),
                    |summary| summary.proven_operations,
                    |summary| summary.declared_operations,
                ),
                skipped_targets: summary.skipped_targets.len(),
                skipped_operations: summary.skipped_operations.len(),
                skipped_reason_counts,
                surface_summaries: summary
                    .surface_summaries
                    .iter()
                    .map(fuzz_coverage_selector_summary)
                    .collect(),
                kind_summaries: summary
                    .kind_summaries
                    .iter()
                    .map(fuzz_coverage_selector_summary)
                    .collect(),
                artifact_ids: summary.artifact_ids.clone(),
            }
        }
        None => FuzzCoverageCompletenessOutput {
            has_summary: false,
            declared_targets: 0,
            executable_targets: 0,
            proven_targets: 0,
            target_coverage_ratio: 1.0,
            declared_operations: 0,
            executable_operations: 0,
            proven_operations: 0,
            operation_coverage_ratio: 1.0,
            skipped_targets: 0,
            skipped_operations: 0,
            skipped_reason_counts: BTreeMap::new(),
            surface_summaries: Vec::new(),
            kind_summaries: Vec::new(),
            artifact_ids: Vec::new(),
        },
    }
}

fn fuzz_coverage_selector_summary(
    summary: &homeboy::core::fuzz::FuzzCoverageGroupSummary,
) -> FuzzCoverageSelectorSummaryOutput {
    let mut skipped_reason_counts = BTreeMap::new();
    accumulate_skip_reason_counts(&mut skipped_reason_counts, &summary.skipped_targets);
    accumulate_skip_reason_counts(&mut skipped_reason_counts, &summary.skipped_operations);

    FuzzCoverageSelectorSummaryOutput {
        id: summary.id.clone(),
        kind: summary.kind.clone(),
        label: summary.label.clone(),
        declared_targets: summary.declared_targets,
        executable_targets: summary.executable_targets,
        proven_targets: summary.proven_targets,
        target_coverage_ratio: count_ratio(summary.proven_targets, summary.declared_targets),
        declared_operations: summary.declared_operations,
        executable_operations: summary.executable_operations,
        proven_operations: summary.proven_operations,
        operation_coverage_ratio: count_ratio(
            summary.proven_operations,
            summary.declared_operations,
        ),
        skipped_targets: summary.skipped_targets.len(),
        skipped_operations: summary.skipped_operations.len(),
        skipped_reason_counts,
    }
}

fn count_ratio(covered: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        covered as f64 / total as f64
    }
}

fn accumulate_skip_reason_counts(
    counts: &mut BTreeMap<String, usize>,
    skips: &[homeboy::core::fuzz::FuzzCoverageSkip],
) {
    for skip in skips {
        let reason = skip.reason.trim();
        if !reason.is_empty() {
            *counts.entry(reason.to_string()).or_default() += 1;
        }
    }
}

fn open_finding_count(campaign: &FuzzCampaign) -> usize {
    campaign
        .findings
        .iter()
        .filter(|finding| finding.status == homeboy::core::fuzz::FuzzFindingStatus::Open)
        .count()
}

pub(super) fn gate_status(gates: &[FuzzGateEvaluation]) -> String {
    if gates.iter().all(|gate| gate.status == "passed") {
        "passed".to_string()
    } else {
        "failed".to_string()
    }
}

fn threshold_passes(
    observed: f64,
    operator: homeboy::core::fuzz::FuzzThresholdOperator,
    expected: f64,
) -> bool {
    match operator {
        homeboy::core::fuzz::FuzzThresholdOperator::GreaterThan => observed > expected,
        homeboy::core::fuzz::FuzzThresholdOperator::GreaterThanOrEqual => observed >= expected,
        homeboy::core::fuzz::FuzzThresholdOperator::LessThan => observed < expected,
        homeboy::core::fuzz::FuzzThresholdOperator::LessThanOrEqual => observed <= expected,
        homeboy::core::fuzz::FuzzThresholdOperator::Equal => {
            (observed - expected).abs() < f64::EPSILON
        }
    }
}
