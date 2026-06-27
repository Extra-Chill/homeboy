use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::bundle::{write_trace_experiment_bundle, TraceExperimentBundleRequest};
use super::TraceArgs;
use crate::commands::CmdResult;

mod markdown;

#[cfg(test)]
pub(super) use markdown::render_aggregate_markdown;
pub(super) use markdown::{
    fmt_delta_avg_ms, fmt_delta_ms, fmt_ms, render_compare_markdown, render_matrix_markdown,
    render_scenario_matrix_markdown, render_trace_aggregate_evidence_markdown,
    render_trace_compare_evidence_markdown, render_trace_run_evidence_markdown,
};

#[derive(Deserialize)]
pub(super) struct TraceAggregateInput {
    pub(super) component: Option<String>,
    pub(super) scenario_id: Option<String>,
    #[serde(default)]
    pub(super) phase_preset: Option<String>,
    #[serde(default)]
    pub(super) repeat: Option<usize>,
    #[serde(default)]
    pub(super) rig_state: Option<Value>,
    #[serde(default)]
    pub(super) overlays: Vec<TraceOverlayInput>,
    #[serde(default)]
    pub(super) runs: Vec<TraceAggregateRunInput>,
    pub(super) spans: Vec<TraceAggregateSpanInput>,
    #[serde(default)]
    pub(super) metrics: Vec<TraceAggregateMetricInput>,
    #[serde(default)]
    pub(super) guardrails: Vec<extension_trace::TraceGuardrailOutput>,
    #[serde(default)]
    pub(super) guardrail_failure_count: usize,
}

#[derive(Deserialize)]
struct TraceAggregateEnvelopeInput {
    data: TraceAggregateInput,
}

/// Shared identity fields (`id`, `n`) carried by trace aggregate span and
/// metric inputs. Flattened into parents so the on-wire JSON keeps the `id`
/// and `n` keys inline.
#[derive(Deserialize, Clone)]
pub(super) struct TraceAggregateIdentity {
    pub(super) id: String,
    pub(super) n: usize,
}

#[derive(Deserialize)]
pub(super) struct TraceAggregateSpanInput {
    #[serde(flatten)]
    pub(super) identity: TraceAggregateIdentity,
    pub(super) median_ms: Option<u64>,
    pub(super) avg_ms: Option<f64>,
    #[serde(default)]
    pub(super) max_ms: Option<u64>,
    #[serde(default)]
    pub(super) max_run_index: Option<usize>,
    #[serde(default)]
    pub(super) max_artifact_path: Option<String>,
    pub(super) failures: usize,
    #[serde(default)]
    pub(super) metadata: Option<extension_trace::TraceSpanMetadata>,
}

#[derive(Deserialize, Clone)]
pub(super) struct TraceAggregateMetricInput {
    #[serde(flatten)]
    pub(super) identity: TraceAggregateIdentity,
    #[serde(default)]
    pub(super) min: Option<f64>,
    #[serde(default)]
    pub(super) median: Option<f64>,
    #[serde(default)]
    pub(super) max: Option<f64>,
    #[serde(default)]
    pub(super) samples: Vec<extension_trace::TraceAggregateMetricSampleOutput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceMetricGuardrailSpec {
    pub metric: String,
    pub statistic: TraceMetricStatistic,
    pub policy: TraceMetricGuardrailPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMetricStatistic {
    Min,
    Median,
    Max,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceMetricGuardrailPolicy {
    Required,
    Equal,
    CandidateLteBaseline,
    AbsoluteDelta { max: f64 },
    PercentDelta { max_percent: f64 },
}

#[derive(Deserialize)]
pub(super) struct TraceOverlayInput {
    pub(super) path: String,
    pub(super) component_path: String,
    #[serde(default)]
    pub(super) touched_files: Vec<String>,
    pub(super) kept: bool,
}

#[derive(Deserialize)]
pub(super) struct TraceAggregateRunInput {
    pub(super) index: usize,
    pub(super) status: String,
    pub(super) exit_code: i32,
    pub(super) artifact_path: String,
    #[serde(default)]
    pub(super) failure: Option<String>,
}

pub(super) fn run_compare(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    let before = required_compare_path_arg(args.scenario.as_deref(), "BEFORE_JSON")?;
    let before_path = PathBuf::from(before);
    let after_path = required_compare_path_arg(args.compare_after, "AFTER_JSON")?;

    let before_json = read_trace_aggregate_json(&before_path)?;
    let after_json = read_trace_aggregate_json(&after_path)?;
    let before = parse_trace_aggregate_for_path(&before_json, &before_path)?;
    let after = parse_trace_aggregate_for_path(&after_json, &after_path)?;
    let output = compare_trace_aggregates_with_focus(
        &before_path,
        before,
        &after_path,
        after,
        &args.focus_spans,
        args.regression_threshold,
        args.regression_min_delta_ms,
        &args.metric_guardrails,
    );
    let exit_code = if output.focus_status.as_deref() == Some("fail")
        || output.guardrail_status.as_deref() == Some("fail")
        || output.metric_guardrail_status.as_deref() == Some("fail")
    {
        1
    } else {
        0
    };
    if let Some(experiment) = args.experiment.as_deref() {
        let before = parse_trace_aggregate_for_path(&before_json, &before_path)?;
        let after = parse_trace_aggregate_for_path(&after_json, &after_path)?;
        write_trace_experiment_bundle(TraceExperimentBundleRequest {
            name: experiment,
            bundle_root: None,
            command: std::env::args().collect::<Vec<_>>().join(" "),
            before_path: &before_path,
            before_json: &before_json,
            before: &before,
            after_path: &after_path,
            after_json: &after_json,
            after: &after,
            compare: &output,
        })?;
    }
    Ok((TraceCommandOutput::Compare(output), exit_code))
}

fn required_compare_path_arg<T>(value: Option<T>, field: &'static str) -> homeboy::core::Result<T> {
    value.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            field,
            "trace compare requires before and after aggregate JSON files",
            None,
            None,
        )
    })
}

fn read_trace_aggregate_json(path: &Path) -> homeboy::core::Result<String> {
    fs::read_to_string(path).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!("Failed to read trace aggregate {}: {}", path.display(), err),
            Some("trace.compare.read".to_string()),
        )
    })
}

fn parse_trace_aggregate_for_path(
    content: &str,
    path: &Path,
) -> homeboy::core::Result<TraceAggregateInput> {
    parse_trace_aggregate_input(content).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some(format!("parse trace aggregate {}", path.display())),
        )
    })
}

pub(super) fn parse_trace_aggregate_input(
    content: &str,
) -> serde_json::Result<TraceAggregateInput> {
    match serde_json::from_str::<TraceAggregateInput>(content) {
        Ok(input) => Ok(input),
        Err(direct_error) => serde_json::from_str::<TraceAggregateEnvelopeInput>(content)
            .map(|envelope| envelope.data)
            .map_err(|_| direct_error),
    }
}

#[cfg(test)]
pub(super) fn compare_trace_aggregates(
    before_path: &Path,
    before: TraceAggregateInput,
    after_path: &Path,
    after: TraceAggregateInput,
) -> extension_trace::TraceCompareOutput {
    compare_trace_aggregates_with_focus(
        before_path,
        before,
        after_path,
        after,
        &[],
        extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        &[],
    )
}

pub(super) fn compare_trace_aggregates_with_focus(
    before_path: &Path,
    before: TraceAggregateInput,
    after_path: &Path,
    after: TraceAggregateInput,
    focus_span_ids: &[String],
    regression_threshold_percent: f64,
    regression_min_delta_ms: u64,
    metric_guardrail_specs: &[TraceMetricGuardrailSpec],
) -> extension_trace::TraceCompareOutput {
    let before_spans = before
        .spans
        .into_iter()
        .map(|span| (span.identity.id.clone(), span))
        .collect::<BTreeMap<_, _>>();
    let after_spans = after
        .spans
        .into_iter()
        .map(|span| (span.identity.id.clone(), span))
        .collect::<BTreeMap<_, _>>();
    let span_ids = before_spans
        .keys()
        .chain(after_spans.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut spans = span_ids
        .into_iter()
        .map(|id| {
            let before_span = before_spans.get(&id);
            let after_span = after_spans.get(&id);
            let before_median = before_span.and_then(|span| span.median_ms);
            let after_median = after_span.and_then(|span| span.median_ms);
            let before_avg = before_span.and_then(|span| span.avg_ms);
            let after_avg = after_span.and_then(|span| span.avg_ms);

            extension_trace::TraceCompareSpanOutput {
                id,
                before_n: before_span.map(|span| span.identity.n),
                after_n: after_span.map(|span| span.identity.n),
                before_median_ms: before_median,
                after_median_ms: after_median,
                median_delta_ms: option_delta_i64(before_median, after_median),
                median_delta_percent: option_percent_delta(
                    before_median.map(|value| value as f64),
                    after_median.map(|value| value as f64),
                ),
                before_avg_ms: before_avg,
                after_avg_ms: after_avg,
                avg_delta_ms: option_delta_f64(before_avg, after_avg),
                avg_delta_percent: option_percent_delta(before_avg, after_avg),
                before_failures: before_span.map(|span| span.failures),
                after_failures: after_span.map(|span| span.failures),
            }
        })
        .collect::<Vec<_>>();
    spans.sort_by(compare_trace_span_impact);
    let classification_summaries = compare_classification_summaries(&before_spans, &after_spans);
    let before_metrics = before
        .metrics
        .into_iter()
        .map(|metric| (metric.identity.id.clone(), metric))
        .collect::<BTreeMap<_, _>>();
    let after_metrics = after
        .metrics
        .into_iter()
        .map(|metric| (metric.identity.id.clone(), metric))
        .collect::<BTreeMap<_, _>>();
    let metrics = compare_metrics(&before_metrics, &after_metrics);
    let metric_guardrails = metric_guardrail_specs
        .iter()
        .map(|spec| evaluate_metric_guardrail(spec, &before_metrics, &after_metrics))
        .collect::<Vec<_>>();
    let metric_guardrail_failure_count = metric_guardrails
        .iter()
        .filter(|guardrail| !guardrail.passed)
        .count();
    let metric_guardrail_status = if metric_guardrail_specs.is_empty() {
        None
    } else if metric_guardrail_failure_count > 0 {
        Some("fail".to_string())
    } else {
        Some("pass".to_string())
    };

    let focus_spans = focus_compare_spans(&spans, focus_span_ids);
    let focus_regression_count = focus_spans
        .iter()
        .filter(|span| {
            is_focused_span_regression(span, regression_threshold_percent, regression_min_delta_ms)
        })
        .count();
    let focus_failure_count = focus_spans
        .iter()
        .filter(|span| span.after_failures.unwrap_or(0) > span.before_failures.unwrap_or(0))
        .count();
    let focus_status = if focus_span_ids.is_empty() {
        None
    } else if focus_regression_count > 0 || focus_failure_count > 0 {
        Some("fail".to_string())
    } else {
        Some("pass".to_string())
    };
    let before_guardrails = before.guardrails;
    let after_guardrails = after.guardrails;
    let guardrail_failure_count = before.guardrail_failure_count + after.guardrail_failure_count;
    let guardrail_status = if before_guardrails.is_empty() && after_guardrails.is_empty() {
        None
    } else if guardrail_failure_count > 0 {
        Some("fail".to_string())
    } else {
        Some("pass".to_string())
    };

    extension_trace::TraceCompareOutput {
        command: "trace.compare.spans",
        before_path: before_path.display().to_string(),
        after_path: after_path.display().to_string(),
        before_target: None,
        after_target: None,
        before_git_sha: None,
        after_git_sha: None,
        before_status: None,
        after_status: None,
        before_exit_code: None,
        after_exit_code: None,
        output_dir: None,
        summary_path: None,
        before_component: before.component,
        after_component: after.component,
        before_scenario_id: before.scenario_id,
        after_scenario_id: after.scenario_id,
        span_count: spans.len(),
        spans,
        metrics,
        metric_guardrails,
        metric_guardrail_failure_count,
        metric_guardrail_status,
        focus_span_ids: focus_span_ids.to_vec(),
        focus_spans,
        focus_regression_count,
        focus_failure_count,
        focus_status,
        before_guardrails,
        after_guardrails,
        guardrail_failure_count,
        guardrail_status,
        classification_summaries,
        proof_run_order: Vec::new(),
        caveats: Vec::new(),
        browser_proof: None,
    }
}

pub(super) fn parse_metric_guardrail(value: &str) -> Result<TraceMetricGuardrailSpec, String> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return Err(
            "metric guardrail must use METRIC[.min|.median|.max]:POLICY[:VALUE]".to_string(),
        );
    }
    let (metric, statistic) = parse_metric_and_statistic(parts[0])?;
    let policy = match parts[1] {
        "required" | "present" => TraceMetricGuardrailPolicy::Required,
        "equal" | "eq" => TraceMetricGuardrailPolicy::Equal,
        "lte" | "candidate_lte_baseline" | "candidate<=baseline" => {
            TraceMetricGuardrailPolicy::CandidateLteBaseline
        }
        "delta" | "absolute_delta" | "max_delta" => TraceMetricGuardrailPolicy::AbsoluteDelta {
            max: parse_required_threshold(parts.get(2), parts[1])?,
        },
        "percent" | "percent_delta" | "max_percent" => TraceMetricGuardrailPolicy::PercentDelta {
            max_percent: parse_required_threshold(parts.get(2), parts[1])?,
        },
        other => {
            return Err(format!(
                "unsupported metric guardrail policy '{}'; use required, equal, lte, delta, or percent",
                other
            ));
        }
    };
    if matches!(
        policy,
        TraceMetricGuardrailPolicy::Required
            | TraceMetricGuardrailPolicy::Equal
            | TraceMetricGuardrailPolicy::CandidateLteBaseline
    ) && parts.len() == 3
    {
        return Err(format!("policy '{}' does not take a threshold", parts[1]));
    }
    Ok(TraceMetricGuardrailSpec {
        metric,
        statistic,
        policy,
    })
}

fn parse_metric_and_statistic(value: &str) -> Result<(String, TraceMetricStatistic), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("metric name must not be empty".to_string());
    }
    for (suffix, statistic) in [
        (".min", TraceMetricStatistic::Min),
        (".median", TraceMetricStatistic::Median),
        (".max", TraceMetricStatistic::Max),
    ] {
        if let Some(metric) = value.strip_suffix(suffix) {
            if metric.is_empty() {
                return Err("metric name must not be empty".to_string());
            }
            return Ok((metric.to_string(), statistic));
        }
    }
    Ok((value.to_string(), TraceMetricStatistic::Median))
}

fn parse_required_threshold(value: Option<&&str>, policy: &str) -> Result<f64, String> {
    let value = value.ok_or_else(|| format!("policy '{}' requires a numeric threshold", policy))?;
    value
        .parse::<f64>()
        .map_err(|_| format!("policy '{}' threshold must be numeric", policy))
}

fn compare_metrics(
    before_metrics: &BTreeMap<String, TraceAggregateMetricInput>,
    after_metrics: &BTreeMap<String, TraceAggregateMetricInput>,
) -> Vec<extension_trace::TraceCompareMetricOutput> {
    let metric_ids = before_metrics
        .keys()
        .chain(after_metrics.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    metric_ids
        .into_iter()
        .map(|id| {
            let before = before_metrics.get(&id);
            let after = after_metrics.get(&id);
            let before_median = before.and_then(|metric| metric.median);
            let after_median = after.and_then(|metric| metric.median);
            extension_trace::TraceCompareMetricOutput {
                id,
                before_n: before.map(|metric| metric.identity.n),
                after_n: after.map(|metric| metric.identity.n),
                before_min: before.and_then(|metric| metric.min),
                after_min: after.and_then(|metric| metric.min),
                before_median,
                after_median,
                median_delta: option_delta_f64(before_median, after_median),
                median_delta_percent: option_percent_delta(before_median, after_median),
                before_max: before.and_then(|metric| metric.max),
                after_max: after.and_then(|metric| metric.max),
                before_samples: before
                    .map(|metric| metric.samples.clone())
                    .unwrap_or_default(),
                after_samples: after
                    .map(|metric| metric.samples.clone())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn evaluate_metric_guardrail(
    spec: &TraceMetricGuardrailSpec,
    before_metrics: &BTreeMap<String, TraceAggregateMetricInput>,
    after_metrics: &BTreeMap<String, TraceAggregateMetricInput>,
) -> extension_trace::TraceMetricGuardrailOutput {
    let before_value = before_metrics
        .get(&spec.metric)
        .and_then(|metric| metric_value(metric, spec.statistic));
    let after_value = after_metrics
        .get(&spec.metric)
        .and_then(|metric| metric_value(metric, spec.statistic));
    let delta = option_delta_f64(before_value, after_value);
    let delta_percent = option_percent_delta(before_value, after_value);
    let failure = match spec.policy {
        TraceMetricGuardrailPolicy::Required => {
            if before_value.is_some() && after_value.is_some() {
                None
            } else {
                Some("metric is required in both baseline and candidate aggregates".to_string())
            }
        }
        TraceMetricGuardrailPolicy::Equal => match (before_value, after_value) {
            (Some(before), Some(after)) if (after - before).abs() < f64::EPSILON => None,
            (Some(_), Some(_)) => Some("candidate value differs from baseline".to_string()),
            _ => Some("metric is missing from baseline or candidate aggregate".to_string()),
        },
        TraceMetricGuardrailPolicy::CandidateLteBaseline => match (before_value, after_value) {
            (Some(before), Some(after)) if after <= before => None,
            (Some(_), Some(_)) => Some("candidate value exceeds baseline".to_string()),
            _ => Some("metric is missing from baseline or candidate aggregate".to_string()),
        },
        TraceMetricGuardrailPolicy::AbsoluteDelta { max } => match delta {
            Some(delta) if delta.abs() <= max => None,
            Some(_) => Some(format!("absolute delta exceeds {}", max)),
            None => Some("metric is missing from baseline or candidate aggregate".to_string()),
        },
        TraceMetricGuardrailPolicy::PercentDelta { max_percent } => match delta_percent {
            Some(delta_percent) if delta_percent.abs() <= max_percent => None,
            Some(_) => Some(format!("percent delta exceeds {}%", max_percent)),
            None => {
                Some("percent delta unavailable for missing or zero baseline metric".to_string())
            }
        },
    };
    extension_trace::TraceMetricGuardrailOutput {
        metric: spec.metric.clone(),
        policy: metric_policy_label(&spec.policy).to_string(),
        statistic: metric_statistic_label(spec.statistic).to_string(),
        passed: failure.is_none(),
        status: if failure.is_none() { "pass" } else { "fail" }.to_string(),
        threshold: metric_policy_threshold(&spec.policy),
        before_value,
        after_value,
        delta,
        delta_percent,
        failure,
    }
}

fn metric_value(
    metric: &TraceAggregateMetricInput,
    statistic: TraceMetricStatistic,
) -> Option<f64> {
    match statistic {
        TraceMetricStatistic::Min => metric.min,
        TraceMetricStatistic::Median => metric.median,
        TraceMetricStatistic::Max => metric.max,
    }
}

fn metric_statistic_label(statistic: TraceMetricStatistic) -> &'static str {
    match statistic {
        TraceMetricStatistic::Min => "min",
        TraceMetricStatistic::Median => "median",
        TraceMetricStatistic::Max => "max",
    }
}

fn metric_policy_label(policy: &TraceMetricGuardrailPolicy) -> &'static str {
    match policy {
        TraceMetricGuardrailPolicy::Required => "required",
        TraceMetricGuardrailPolicy::Equal => "equal",
        TraceMetricGuardrailPolicy::CandidateLteBaseline => "candidate_lte_baseline",
        TraceMetricGuardrailPolicy::AbsoluteDelta { .. } => "absolute_delta",
        TraceMetricGuardrailPolicy::PercentDelta { .. } => "percent_delta",
    }
}

fn metric_policy_threshold(policy: &TraceMetricGuardrailPolicy) -> Option<f64> {
    match policy {
        TraceMetricGuardrailPolicy::AbsoluteDelta { max } => Some(*max),
        TraceMetricGuardrailPolicy::PercentDelta { max_percent } => Some(*max_percent),
        _ => None,
    }
}

fn compare_classification_summaries(
    before_spans: &BTreeMap<String, TraceAggregateSpanInput>,
    after_spans: &BTreeMap<String, TraceAggregateSpanInput>,
) -> Vec<extension_trace::TraceCompareClassificationSummaryOutput> {
    let mut totals: BTreeMap<String, (usize, Option<u64>, Option<u64>)> = BTreeMap::new();
    for (id, before_span) in before_spans {
        let metadata = after_spans
            .get(id)
            .and_then(|span| span.metadata.as_ref())
            .or(before_span.metadata.as_ref());
        add_compare_classification_totals(
            &mut totals,
            metadata,
            before_span.median_ms,
            after_spans.get(id).and_then(|span| span.median_ms),
        );
    }
    for (id, after_span) in after_spans {
        if before_spans.contains_key(id) {
            continue;
        }
        add_compare_classification_totals(
            &mut totals,
            after_span.metadata.as_ref(),
            None,
            after_span.median_ms,
        );
    }
    totals
        .into_iter()
        .map(
            |(classification, (span_count, before_total_median_ms, after_total_median_ms))| {
                extension_trace::TraceCompareClassificationSummaryOutput {
                    classification,
                    span_count,
                    before_total_median_ms,
                    after_total_median_ms,
                    median_delta_ms: option_delta_i64(
                        before_total_median_ms,
                        after_total_median_ms,
                    ),
                }
            },
        )
        .collect()
}

fn add_compare_classification_totals(
    totals: &mut BTreeMap<String, (usize, Option<u64>, Option<u64>)>,
    metadata: Option<&extension_trace::TraceSpanMetadata>,
    before_median_ms: Option<u64>,
    after_median_ms: Option<u64>,
) {
    let Some(metadata) = metadata else {
        return;
    };
    for classification in span_classifications(metadata) {
        let entry = totals
            .entry(classification)
            .or_insert((0, Some(0), Some(0)));
        entry.0 += 1;
        entry.1 = option_sum(entry.1, before_median_ms);
        entry.2 = option_sum(entry.2, after_median_ms);
    }
}

fn focus_compare_spans(
    spans: &[extension_trace::TraceCompareSpanOutput],
    focus_span_ids: &[String],
) -> Vec<extension_trace::TraceCompareSpanOutput> {
    if focus_span_ids.is_empty() {
        return Vec::new();
    }
    let focus = focus_span_ids.iter().collect::<BTreeSet<_>>();
    spans
        .iter()
        .filter(|span| focus.contains(&span.id))
        .cloned()
        .collect()
}

fn is_focused_span_regression(
    span: &extension_trace::TraceCompareSpanOutput,
    regression_threshold_percent: f64,
    regression_min_delta_ms: u64,
) -> bool {
    let Some(delta_ms) = span.median_delta_ms else {
        return false;
    };
    if delta_ms <= 0 || delta_ms < regression_min_delta_ms as i64 {
        return false;
    }
    span.median_delta_percent
        .is_some_and(|percent| percent >= regression_threshold_percent)
}

fn compare_trace_span_impact(
    left: &extension_trace::TraceCompareSpanOutput,
    right: &extension_trace::TraceCompareSpanOutput,
) -> std::cmp::Ordering {
    right
        .median_delta_ms
        .map(i64::abs)
        .cmp(&left.median_delta_ms.map(i64::abs))
        .then_with(|| {
            right
                .avg_delta_ms
                .map(f64::abs)
                .partial_cmp(&left.avg_delta_ms.map(f64::abs))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| left.id.cmp(&right.id))
}

fn option_delta_i64(before: Option<u64>, after: Option<u64>) -> Option<i64> {
    Some(after? as i64 - before? as i64)
}

fn option_delta_f64(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    Some(after? - before?)
}

fn option_percent_delta(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    let before = before?;
    let after = after?;
    if before.abs() < f64::EPSILON {
        if after.abs() < f64::EPSILON {
            Some(0.0)
        } else {
            None
        }
    } else {
        Some(((after - before) / before) * 100.0)
    }
}

pub(super) fn attach_span_metadata(
    spans: &mut [extension_trace::TraceAggregateSpanOutput],
    span_metadata: &BTreeMap<String, extension_trace::TraceSpanMetadata>,
) -> Vec<String> {
    if span_metadata.is_empty() {
        return Vec::new();
    }
    let mut matched = BTreeSet::new();
    for span in spans {
        if let Some(metadata) = span_metadata.get(&span.id) {
            span.metadata = Some(metadata.clone());
            matched.insert(span.id.clone());
        }
    }
    span_metadata
        .keys()
        .filter(|id| !matched.contains(*id))
        .cloned()
        .collect()
}

pub(super) fn classification_summaries(
    spans: &[extension_trace::TraceAggregateSpanOutput],
) -> Vec<extension_trace::TraceClassificationSummaryOutput> {
    let mut totals: BTreeMap<String, (usize, Option<u64>, Option<f64>)> = BTreeMap::new();
    for span in spans {
        let Some(metadata) = span.metadata.as_ref() else {
            continue;
        };
        for classification in span_classifications(metadata) {
            let entry = totals
                .entry(classification)
                .or_insert((0, Some(0), Some(0.0)));
            entry.0 += 1;
            entry.1 = option_sum(entry.1, span.median_ms);
            entry.2 = option_sum_f64(entry.2, span.avg_ms);
        }
    }
    totals
        .into_iter()
        .map(
            |(classification, (span_count, total_median_ms, total_avg_ms))| {
                extension_trace::TraceClassificationSummaryOutput {
                    classification,
                    span_count,
                    total_median_ms,
                    total_avg_ms,
                }
            },
        )
        .collect()
}

fn span_classifications(metadata: &extension_trace::TraceSpanMetadata) -> Vec<String> {
    let mut classifications = Vec::new();
    if metadata.critical {
        classifications.push("critical".to_string());
    }
    if metadata.blocking {
        classifications.push("blocking".to_string());
    }
    if metadata.cacheable {
        classifications.push("cacheable".to_string());
        if metadata.critical {
            classifications.push("cacheable_critical".to_string());
        }
    }
    if metadata.prewarmable {
        classifications.push("prewarmable".to_string());
        if metadata.critical {
            classifications.push("prewarmable_critical".to_string());
        }
    }
    if metadata.deferrable {
        classifications.push("deferrable".to_string());
        if metadata.critical {
            classifications.push("deferrable_critical".to_string());
        }
    }
    if let Some(category) = metadata.category.as_deref() {
        classifications.push(format!("category:{category}"));
    }
    if let Some(blocks) = metadata.blocks.as_deref() {
        classifications.push(format!("blocks:{blocks}"));
    }
    classifications
}

fn option_sum<T>(left: Option<T>, right: Option<T>) -> Option<T>
where
    T: std::ops::Add<Output = T>,
{
    Some(left? + right?)
}

fn option_sum_f64(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    Some(left? + right?)
}
