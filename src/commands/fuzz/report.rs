use std::collections::BTreeMap;
use std::path::Path;

use homeboy::core::artifact_ref::EvidenceRef;
use homeboy::core::fuzz::{
    fuzz_gate_profile_contract, parse_fuzz_case_log_file, parse_fuzz_observation_set_value,
    parse_fuzz_results_file, parse_fuzz_target_inventory_file, rank_fuzz_observation_set_hotspots,
    FuzzCampaign, FuzzExecutionRequest, FuzzGateProfile, FuzzHotspotSet, FuzzProvenance,
    FuzzResultEnvelope, FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA,
    FUZZ_RESULT_ENVELOPE_SCHEMA,
};
use homeboy::core::observation::{ArtifactRecord, ObservationStore};
use homeboy::core::performance_hotspots::{
    summarize_performance_hotspots, PerformanceHotspotSummary, PerformanceMetricPoint,
};

pub(super) const FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND: &str = "fuzz_result_envelope";

use super::types::{
    FuzzCoverageCompletenessOutput, FuzzCoverageSelectorSummaryOutput, FuzzGateEvaluation,
    FuzzReportArgs, FuzzReportOutput, FuzzRunArgs, FuzzValidateArgs, FuzzValidateOutput,
};

pub(super) fn run_validate(args: FuzzValidateArgs) -> homeboy::core::Result<FuzzValidateOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let mut case_log_entries = 0;
    for path in &args.case_logs {
        case_log_entries += parse_fuzz_case_log_file(path)?.len();
    }
    let gates = evaluate_fuzz_gates_for_profile(&campaign, args.gate_profile.as_core());
    let coverage_completeness = fuzz_coverage_completeness(&campaign);
    let performance_hotspots = fuzz_performance_hotspots(&campaign);
    let observation_hotspots = fuzz_observation_hotspots(&campaign);

    Ok(FuzzValidateOutput {
        command: "fuzz.validate".to_string(),
        status: gate_status(&gates),
        results_file: args.results_file.to_string_lossy().to_string(),
        campaign_id: campaign.id.clone(),
        case_log_files: args
            .case_logs
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        case_log_entries,
        open_findings: open_finding_count(&campaign),
        artifacts: campaign.artifacts.len(),
        coverage_completeness,
        performance_hotspots,
        observation_hotspots,
        gates,
    })
}

pub(super) fn run_report(args: FuzzReportArgs) -> homeboy::core::Result<FuzzReportOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let coverage_completeness = fuzz_coverage_completeness(&campaign);
    let performance_hotspots = fuzz_performance_hotspots(&campaign);
    let observation_hotspots = fuzz_observation_hotspots(&campaign);
    let component = args.run.comp.id().unwrap_or("unknown");
    let mut envelope = fuzz_result_envelope_from_campaign(
        &args.run,
        component,
        &campaign,
        args.envelope_id.as_deref(),
    )?;
    let gates = evaluate_fuzz_result_envelope_gates(&envelope);
    let status = gate_status(&gates);
    envelope.status = status.clone();

    if let Some(path) = args.output_envelope.as_ref() {
        let json = fuzz_result_envelope_json(&envelope)?;
        std::fs::write(path, json).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
    }
    let persisted_envelope = persist_fuzz_result_envelope(
        args.run.run_id.as_deref(),
        &envelope,
        args.output_envelope.as_deref(),
    )?;
    let evidence_refs = persisted_envelope
        .as_ref()
        .map(fuzz_result_envelope_evidence_ref)
        .into_iter()
        .collect();

    Ok(FuzzReportOutput {
        command: "fuzz.report".to_string(),
        status,
        results_file: args.results_file.to_string_lossy().to_string(),
        envelope_file: args
            .output_envelope
            .map(|path| path.to_string_lossy().to_string()),
        evidence_refs,
        envelope,
        coverage_completeness,
        performance_hotspots,
        observation_hotspots,
        gates,
    })
}

pub(super) fn fuzz_observation_hotspots(campaign: &FuzzCampaign) -> Option<FuzzHotspotSet> {
    parse_fuzz_observation_set_value(&campaign.metadata)
        .map(|set| rank_fuzz_observation_set_hotspots(&set))
}

pub(super) fn fuzz_result_envelope_from_campaign(
    run: &FuzzRunArgs,
    component: &str,
    campaign: &FuzzCampaign,
    envelope_id: Option<&str>,
) -> homeboy::core::Result<FuzzResultEnvelope> {
    let request_id = run
        .run_id
        .clone()
        .or_else(|| run.workload_id.clone())
        .unwrap_or_else(|| format!("{}-request", campaign.id));
    let envelope_id = envelope_id
        .map(str::to_string)
        .or_else(|| run.run_id.clone())
        .unwrap_or_else(|| campaign.id.clone());
    let (required_artifacts, gates) = report_gate_contract(run.gate_profile);
    let metadata = fuzz_result_metadata(run.inventory.as_deref())?;

    Ok(FuzzResultEnvelope {
        schema: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: envelope_id,
        status: "pending".to_string(),
        request: FuzzExecutionRequest {
            schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: request_id,
            component: component.to_string(),
            rig_id: run.rig.clone(),
            workload_id: run.workload_id.clone(),
            case_ids: campaign.cases.iter().map(|case| case.id.clone()).collect(),
            seed: run.seed.clone(),
            max_duration: run.max_duration.clone(),
            args: run.args.clone(),
            required_artifacts: required_artifacts.clone(),
            gates: gates.clone(),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        },
        campaign: Some(campaign.clone()),
        artifacts: campaign.artifacts.clone(),
        required_artifacts,
        gates,
        provenance: Some(fuzz_provenance(run.run_id.clone())),
        metadata,
        extra: std::collections::BTreeMap::new(),
    })
}

fn report_gate_contract(
    profile: super::types::FuzzGateProfileArg,
) -> (
    Vec<homeboy::core::fuzz::FuzzRequiredArtifact>,
    Vec<homeboy::core::fuzz::FuzzGate>,
) {
    fuzz_gate_profile_contract(profile.as_core())
}

pub(super) fn persist_fuzz_result_envelope(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
    envelope_path: Option<&Path>,
) -> homeboy::core::Result<Option<ArtifactRecord>> {
    persist_fuzz_result_envelope_with_source(run_id, envelope, envelope_path, "homeboy fuzz report")
}

pub(super) fn persist_fuzz_run_result_envelope(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
) -> homeboy::core::Result<Option<ArtifactRecord>> {
    persist_fuzz_result_envelope_with_source(run_id, envelope, None, "homeboy fuzz run")
}

fn persist_fuzz_result_envelope_with_source(
    run_id: Option<&str>,
    envelope: &FuzzResultEnvelope,
    envelope_path: Option<&Path>,
    source: &str,
) -> homeboy::core::Result<Option<ArtifactRecord>> {
    let Some(run_id) = run_id.filter(|run_id| !run_id.trim().is_empty()) else {
        return Ok(None);
    };
    let store = ObservationStore::open_initialized()?;
    if store.get_run(run_id)?.is_none() {
        return Ok(None);
    }

    if let Some(path) = envelope_path.filter(|path| path.is_file()) {
        return record_fuzz_result_envelope_artifact(&store, run_id, path, envelope, source);
    }

    let mut artifact_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("create temporary fuzz result envelope artifact".to_string()),
            )
        })?;
    serde_json::to_writer_pretty(&mut artifact_file, envelope).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode fuzz result envelope: {error}"
        ))
    })?;
    record_fuzz_result_envelope_artifact(&store, run_id, artifact_file.path(), envelope, source)
}

fn record_fuzz_result_envelope_artifact(
    store: &ObservationStore,
    run_id: &str,
    path: &Path,
    envelope: &FuzzResultEnvelope,
    source: &str,
) -> homeboy::core::Result<Option<ArtifactRecord>> {
    let metadata = serde_json::json!({
        "schema": envelope.schema.as_str(),
        "envelope_id": envelope.id.as_str(),
        "status": envelope.status.as_str(),
        "campaign_id": envelope.campaign.as_ref().map(|campaign| campaign.id.as_str()),
        "source": source,
        "evidence": {
            "role": "result",
            "semantic_key": "fuzz.result_envelope",
        },
    });
    store
        .record_artifact_with_metadata(run_id, FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND, path, metadata)
        .map(Some)
}

pub(super) fn fuzz_result_envelope_evidence_ref(artifact: &ArtifactRecord) -> EvidenceRef {
    EvidenceRef::for_artifact(
        artifact,
        "Fuzz result envelope",
        Some("result".to_string()),
        Some("fuzz.result_envelope".to_string()),
    )
}

fn fuzz_result_envelope_json(envelope: &FuzzResultEnvelope) -> homeboy::core::Result<String> {
    serde_json::to_string_pretty(envelope).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode fuzz result envelope: {error}"
        ))
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

#[cfg(test)]
pub(super) fn evaluate_fuzz_gates(campaign: &FuzzCampaign) -> Vec<FuzzGateEvaluation> {
    evaluate_fuzz_gates_for_profile(campaign, FuzzGateProfile::Strict)
}

pub(super) fn evaluate_fuzz_gates_for_profile(
    campaign: &FuzzCampaign,
    profile: FuzzGateProfile,
) -> Vec<FuzzGateEvaluation> {
    let evaluation_profile = FuzzGateEvaluationProfile::default();
    let metrics = FuzzGateMetrics::from_campaign(campaign, &evaluation_profile);
    let (_, gates) = fuzz_gate_profile_contract(profile);

    gates
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

pub(super) fn evaluate_expected_metric_gates(
    campaign: Option<&FuzzCampaign>,
    expectations: &[(String, String)],
) -> Vec<FuzzGateEvaluation> {
    expectations
        .iter()
        .map(|(metric, expected)| {
            let expected_value = expected.trim().parse::<f64>().unwrap_or(f64::NAN);
            let observed = campaign.and_then(|campaign| fuzz_campaign_metric(campaign, metric));
            let passed = expected_value.is_finite()
                && observed
                    .is_some_and(|observed| (observed - expected_value).abs() < f64::EPSILON);
            FuzzGateEvaluation {
                gate_id: format!("expected-metric-{metric}"),
                status: if passed { "passed" } else { "failed" }.to_string(),
                metric: metric.clone(),
                observed: observed.unwrap_or(0.0),
                expected: if expected_value.is_finite() {
                    expected_value
                } else {
                    0.0
                },
            }
        })
        .collect()
}

pub(super) fn fuzz_campaign_metric(campaign: &FuzzCampaign, metric: &str) -> Option<f64> {
    let mut metrics = BTreeMap::new();
    collect_value_metrics(None, &campaign.metadata, &mut metrics);
    for (key, value) in &campaign.extra {
        collect_value_metrics(Some(key), value, &mut metrics);
    }
    for artifact in &campaign.artifacts {
        collect_value_metrics(
            Some(&format!("artifact.{}.metadata", artifact.id)),
            &artifact.metadata,
            &mut metrics,
        );
        for (key, value) in &artifact.extra {
            collect_value_metrics(
                Some(&format!("artifact.{}.extra.{key}", artifact.id)),
                value,
                &mut metrics,
            );
        }
    }
    if let Some(summary) = campaign.coverage_summary.as_ref() {
        collect_value_metrics(
            Some("coverage_summary.metadata"),
            &summary.metadata,
            &mut metrics,
        );
        for (key, value) in &summary.extra {
            collect_value_metrics(
                Some(&format!("coverage_summary.extra.{key}")),
                value,
                &mut metrics,
            );
        }
    }

    metrics.get(metric).copied().or_else(|| {
        metrics
            .iter()
            .find_map(|(key, value)| key.ends_with(&format!(".{metric}")).then_some(*value))
    })
}

fn collect_value_metrics(
    prefix: Option<&str>,
    value: &serde_json::Value,
    metrics: &mut BTreeMap<String, f64>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let metric = prefix
                    .map(|prefix| format!("{prefix}.{key}"))
                    .unwrap_or_else(|| key.clone());
                collect_value_metrics(Some(&metric), value, metrics);
            }
        }
        serde_json::Value::Number(number) => {
            if let (Some(prefix), Some(value)) =
                (prefix, number.as_f64().filter(|value| value.is_finite()))
            {
                metrics.insert(prefix.to_string(), value);
            }
        }
        serde_json::Value::String(raw) => {
            if let (Some(prefix), Ok(value)) = (prefix, raw.parse::<f64>()) {
                if value.is_finite() {
                    metrics.insert(prefix.to_string(), value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                let metric = prefix
                    .map(|prefix| format!("{prefix}.{index}"))
                    .unwrap_or_else(|| index.to_string());
                collect_value_metrics(Some(&metric), value, metrics);
            }
        }
        _ => {}
    }
}

pub(super) fn evaluate_fuzz_result_envelope_gates(
    envelope: &FuzzResultEnvelope,
) -> Vec<FuzzGateEvaluation> {
    let metrics = envelope.campaign.as_ref().map(|campaign| {
        FuzzGateMetrics::from_campaign(campaign, &FuzzGateEvaluationProfile::default())
    });
    let mut gates = envelope
        .gates
        .iter()
        .map(|gate| {
            let observed = metrics
                .as_ref()
                .map(|metrics| observed_gate_metric(metrics, gate.metric.as_str()))
                .unwrap_or(0.0);
            let passed = threshold_passes(observed, gate.operator, gate.value);
            FuzzGateEvaluation {
                gate_id: gate.id.clone(),
                status: if passed { "passed" } else { "failed" }.to_string(),
                metric: gate.metric.clone(),
                observed,
                expected: gate.value,
            }
        })
        .collect::<Vec<_>>();

    gates.extend(
        envelope
            .required_artifacts
            .iter()
            .filter(|artifact| artifact.required)
            .map(|artifact| {
                let observed = required_artifact_count(envelope, artifact.kind.as_str()) as f64;
                FuzzGateEvaluation {
                    gate_id: format!("has-required-artifact-{}", artifact.id),
                    status: if observed >= 1.0 { "passed" } else { "failed" }.to_string(),
                    metric: format!("required_artifact:{}", artifact.kind),
                    observed,
                    expected: 1.0,
                }
            }),
    );

    gates
}

fn observed_gate_metric(metrics: &FuzzGateMetrics, metric: &str) -> f64 {
    match metric {
        "open_findings" => metrics.open_findings as f64,
        "case_log_artifacts" => metrics.case_evidence_artifacts as f64,
        "target_coverage_ratio" => metrics.target_coverage_ratio,
        "operation_coverage_ratio" => metrics.operation_coverage_ratio,
        _ => 0.0,
    }
}

pub(super) fn required_artifact_count(envelope: &FuzzResultEnvelope, kind: &str) -> usize {
    match kind {
        "result_envelope" => 1,
        "case_log" => envelope
            .campaign
            .as_ref()
            .map(|campaign| {
                case_evidence_artifact_count(campaign, &FuzzGateEvaluationProfile::default())
            })
            .unwrap_or(0),
        "coverage_summary" => {
            usize::from(
                envelope
                    .campaign
                    .as_ref()
                    .and_then(|campaign| campaign.coverage_summary.as_ref())
                    .is_some(),
            ) + artifact_ref_count(envelope, kind)
        }
        "replay_data" => replay_data_count(envelope) + artifact_ref_count(envelope, kind),
        _ => artifact_ref_count(envelope, kind),
    }
}

fn replay_data_count(envelope: &FuzzResultEnvelope) -> usize {
    let Some(campaign) = envelope.campaign.as_ref() else {
        return 0;
    };

    usize::from(campaign.replay.is_some())
        + campaign
            .cases
            .iter()
            .filter(|case| case.replay_id.is_some() || !case.input.is_null())
            .count()
        + campaign
            .seeds
            .iter()
            .filter(|seed| seed.value.is_some() || seed.artifact.is_some())
            .count()
}

fn artifact_ref_count(envelope: &FuzzResultEnvelope, kind: &str) -> usize {
    let envelope_artifacts = envelope
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == kind)
        .count();
    let campaign_artifacts = envelope
        .campaign
        .as_ref()
        .map(|campaign| {
            campaign
                .artifacts
                .iter()
                .filter(|artifact| artifact.kind == kind)
                .count()
                + metadata_artifact_kind_count(&campaign.metadata, kind)
        })
        .unwrap_or(0);

    envelope_artifacts + campaign_artifacts + metadata_artifact_kind_count(&envelope.metadata, kind)
}

fn metadata_artifact_kind_count(metadata: &serde_json::Value, kind: &str) -> usize {
    ["artifact_refs", "artifactRefs"]
        .into_iter()
        .map(|key| {
            metadata
                .get(key)
                .and_then(|value| value.as_array())
                .map(|artifacts| {
                    artifacts
                        .iter()
                        .filter(|artifact| metadata_artifact_has_kind(artifact, kind))
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

fn metadata_artifact_has_kind(artifact: &serde_json::Value, kind: &str) -> bool {
    artifact
        .get("kind")
        .or_else(|| artifact.get("role"))
        .and_then(|value| value.as_str())
        == Some(kind)
}

struct FuzzGateEvaluationProfile {
    case_evidence_kinds: &'static [&'static str],
    metadata_artifact_ref_keys: &'static [&'static str],
}

impl Default for FuzzGateEvaluationProfile {
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
    fn from_campaign(campaign: &FuzzCampaign, profile: &FuzzGateEvaluationProfile) -> Self {
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
        return 0.0;
    };
    let total = total(summary);
    if total == 0 {
        1.0
    } else {
        covered(summary) as f64 / total as f64
    }
}

fn case_evidence_artifact_count(
    campaign: &FuzzCampaign,
    profile: &FuzzGateEvaluationProfile,
) -> usize {
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
    profile: &FuzzGateEvaluationProfile,
) -> usize {
    profile
        .metadata_artifact_ref_keys
        .iter()
        .map(|key| metadata_artifact_refs(metadata.get(*key), profile))
        .sum()
}

fn metadata_artifact_refs(
    value: Option<&serde_json::Value>,
    profile: &FuzzGateEvaluationProfile,
) -> usize {
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
    profile: &FuzzGateEvaluationProfile,
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
            target_coverage_ratio: 0.0,
            declared_operations: 0,
            executable_operations: 0,
            proven_operations: 0,
            operation_coverage_ratio: 0.0,
            skipped_targets: 0,
            skipped_operations: 0,
            skipped_reason_counts: BTreeMap::new(),
            surface_summaries: Vec::new(),
            kind_summaries: Vec::new(),
            artifact_ids: Vec::new(),
        },
    }
}

pub(super) fn fuzz_performance_hotspots(campaign: &FuzzCampaign) -> PerformanceHotspotSummary {
    let mut points = Vec::new();

    collect_metadata_metric_points(&campaign.id, &campaign.metadata, &mut points);
    collect_extra_metric_points(&campaign.id, &campaign.extra, &mut points);

    if let Some(summary) = campaign.coverage_summary.as_ref() {
        collect_metadata_metric_points("coverage_summary", &summary.metadata, &mut points);
        collect_extra_metric_points("coverage_summary", &summary.extra, &mut points);

        for group in summary
            .surface_summaries
            .iter()
            .chain(summary.kind_summaries.iter())
        {
            let subject_id = format!("coverage_summary:{}", group.id);
            collect_metadata_metric_points(&subject_id, &group.metadata, &mut points);
            collect_extra_metric_points(&subject_id, &group.extra, &mut points);
        }
    }

    for coverage in &campaign.coverage {
        let subject_id = format!("coverage:{}", coverage.id);
        collect_metadata_metric_points(&subject_id, &coverage.metadata, &mut points);
        collect_extra_metric_points(&subject_id, &coverage.extra, &mut points);
    }

    for artifact in &campaign.artifacts {
        let subject_id = format!("artifact:{}", artifact.id);
        collect_metadata_metric_points(&subject_id, &artifact.metadata, &mut points);
        collect_extra_metric_points(&subject_id, &artifact.extra, &mut points);
    }

    summarize_performance_hotspots(&points, 5, 5)
}

fn collect_metadata_metric_points(
    subject_id: &str,
    value: &serde_json::Value,
    points: &mut Vec<PerformanceMetricPoint>,
) {
    collect_value_metric_points(subject_id, None, value, points);
}

fn collect_extra_metric_points(
    subject_id: &str,
    extra: &BTreeMap<String, serde_json::Value>,
    points: &mut Vec<PerformanceMetricPoint>,
) {
    for (key, value) in extra {
        collect_value_metric_points(subject_id, Some(key), value, points);
    }
}

fn collect_value_metric_points(
    subject_id: &str,
    prefix: Option<&str>,
    value: &serde_json::Value,
    points: &mut Vec<PerformanceMetricPoint>,
) {
    let Some(object) = value.as_object() else {
        return;
    };

    for (key, value) in object {
        let metric = prefix
            .map(|prefix| format!("{prefix}.{key}"))
            .unwrap_or_else(|| key.clone());
        if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
            points.push(PerformanceMetricPoint {
                subject_id: subject_id.to_string(),
                metric,
                value: number,
            });
        } else {
            collect_value_metric_points(subject_id, Some(&metric), value, points);
        }
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
