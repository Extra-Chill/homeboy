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
    pub(super) guardrails: Vec<extension_trace::TraceGuardrailOutput>,
    #[serde(default)]
    pub(super) guardrail_failure_count: usize,
}

#[derive(Deserialize)]
struct TraceAggregateEnvelopeInput {
    data: TraceAggregateInput,
}

#[derive(Deserialize)]
pub(super) struct TraceAggregateSpanInput {
    pub(super) id: String,
    pub(super) n: usize,
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
    );
    let exit_code = if output.focus_status.as_deref() == Some("fail")
        || output.guardrail_status.as_deref() == Some("fail")
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
) -> extension_trace::TraceCompareOutput {
    let before_spans = before
        .spans
        .into_iter()
        .map(|span| (span.id.clone(), span))
        .collect::<BTreeMap<_, _>>();
    let after_spans = after
        .spans
        .into_iter()
        .map(|span| (span.id.clone(), span))
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
                before_n: before_span.map(|span| span.n),
                after_n: after_span.map(|span| span.n),
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
        before_component: before.component,
        after_component: after.component,
        before_scenario_id: before.scenario_id,
        after_scenario_id: after.scenario_id,
        span_count: spans.len(),
        spans,
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

pub(super) fn aggregate_span(
    id: String,
    samples: Vec<TraceAggregateSpanSample>,
    failures: usize,
) -> extension_trace::TraceAggregateSpanOutput {
    let max_sample: Option<TraceAggregateSpanSample> =
        samples.iter().fold(None, |max, sample| match max {
            Some(current) if current.duration_ms >= sample.duration_ms => Some(current),
            _ => Some(sample.clone()),
        });
    let sample_outputs = samples
        .iter()
        .map(|sample| extension_trace::TraceAggregateSpanSampleOutput {
            run_index: sample.run_index,
            duration_ms: sample.duration_ms,
            artifact_path: sample.artifact_path.clone(),
        })
        .collect::<Vec<_>>();
    let mut durations = samples
        .iter()
        .map(|sample| sample.duration_ms)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let n = durations.len();
    let avg_ms = if n == 0 {
        None
    } else {
        Some(durations.iter().sum::<u64>() as f64 / n as f64)
    };
    extension_trace::TraceAggregateSpanOutput {
        id,
        n,
        min_ms: durations.first().copied(),
        median_ms: median(&durations),
        avg_ms,
        stddev_ms: stddev(&durations, avg_ms),
        p75_ms: percentile(&durations, 75, 4),
        p90_ms: percentile(&durations, 90, 10),
        p95_ms: percentile(&durations, 95, 20),
        max_ms: durations.last().copied(),
        max_run_index: max_sample.as_ref().map(|sample| sample.run_index),
        max_artifact_path: max_sample.map(|sample| sample.artifact_path),
        failures,
        samples: sample_outputs,
        metadata: None,
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

#[derive(Clone)]
pub(super) struct TraceAggregateSpanSample {
    pub(super) duration_ms: u64,
    pub(super) run_index: usize,
    pub(super) artifact_path: String,
}

fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let midpoint = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[midpoint])
    } else {
        Some((values[midpoint - 1] + values[midpoint]) / 2)
    }
}

fn stddev(values: &[u64], avg: Option<f64>) -> Option<f64> {
    let avg = avg?;
    if values.is_empty() {
        return None;
    }
    let variance = values
        .iter()
        .map(|value| {
            let delta = *value as f64 - avg;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64;
    Some(variance.sqrt())
}

fn percentile(values: &[u64], percentile: usize, min_samples: usize) -> Option<u64> {
    if values.len() < min_samples {
        return None;
    }
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values.get(index).copied()
}

pub(super) fn render_aggregate_markdown(
    aggregate: &extension_trace::TraceAggregateOutput,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Trace Aggregate: `{}`\n\n",
        aggregate.scenario_id
    ));
    out.push_str(&format!("- **Component:** `{}`\n", aggregate.component));
    out.push_str(&format!("- **Status:** `{}`\n", aggregate.status));
    out.push_str(&format!("- **Runs:** `{}`\n", aggregate.run_count));
    out.push_str(&format!("- **Failures:** `{}`\n", aggregate.failure_count));
    if !aggregate.guardrails.is_empty() {
        out.push_str(&format!(
            "- **Guardrails:** `{}` (`{}` failures)\n",
            if aggregate.guardrail_failure_count == 0 {
                "pass"
            } else {
                "fail"
            },
            aggregate.guardrail_failure_count
        ));
    }
    if let Some(schedule) = aggregate.schedule.as_deref() {
        out.push_str(&format!("- **Schedule:** `{}`\n", schedule));
    }
    extension_trace::push_overlay_markdown(&mut out, &aggregate.overlays);

    if !aggregate.classification_summaries.is_empty() {
        out.push_str("\n## Critical Path Classification\n\n");
        push_classification_summary_table(&mut out, &aggregate.classification_summaries);
    }
    if !aggregate.unmatched_span_metadata_ids.is_empty() {
        out.push_str("\n## Unmatched Span Metadata\n\n");
        for id in &aggregate.unmatched_span_metadata_ids {
            out.push_str(&format!("- `{id}`\n"));
        }
    }

    if !aggregate.focus_span_ids.is_empty() {
        out.push_str("\n## Focus Spans\n\n");
        if aggregate.focus_spans.is_empty() {
            out.push_str("No focused spans matched the aggregate output.\n");
        } else {
            out.push_str(
                "| Span | n | min | median | avg | stddev | p75 | p90 | p95 | max | failures |\n",
            );
            out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
            for span in &aggregate.focus_spans {
                push_aggregate_span_row(&mut out, span);
            }
        }
    }

    if !aggregate.spans.is_empty() {
        out.push_str("\n## Spans\n\n");
        out.push_str(
            "| Span | n | min | median | avg | stddev | p75 | p90 | p95 | max | failures |\n",
        );
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for span in &aggregate.spans {
            push_aggregate_span_row(&mut out, span);
        }

        let mut outliers = aggregate
            .spans
            .iter()
            .filter(|span| span.max_ms.is_some() && span.max_run_index.is_some())
            .collect::<Vec<_>>();
        if !outliers.is_empty() {
            out.push_str("\n## Outliers\n\n");
            out.push_str("| Span | max | max run | max artifact |\n");
            out.push_str("|---|---:|---:|---|\n");
            outliers.sort_by(|left, right| {
                right
                    .max_ms
                    .cmp(&left.max_ms)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for span in outliers {
                out.push_str(&format!(
                    "| `{}` | {} | {} | `{}` |\n",
                    span.id,
                    fmt_ms(span.max_ms),
                    span.max_run_index.unwrap_or_default(),
                    span.max_artifact_path.as_deref().unwrap_or("")
                ));
            }
        }
    }

    push_guardrail_markdown(&mut out, "Guardrails", &aggregate.guardrails);

    out.push_str("\n## Run Artifacts\n\n");
    for run in &aggregate.runs {
        out.push_str(&format!(
            "- Run {}: `{}` `{}`\n",
            run.index, run.status, run.artifact_path
        ));
    }
    out
}

fn push_classification_summary_table(
    out: &mut String,
    summaries: &[extension_trace::TraceClassificationSummaryOutput],
) {
    out.push_str("| Classification | spans | total median | total avg |\n");
    out.push_str("|---|---:|---:|---:|\n");
    for summary in summaries {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            summary.classification,
            summary.span_count,
            fmt_ms(summary.total_median_ms),
            fmt_avg_ms(summary.total_avg_ms),
        ));
    }
}

fn push_aggregate_span_row(out: &mut String, span: &extension_trace::TraceAggregateSpanOutput) {
    out.push_str(&format!(
        "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
        span.id,
        span.n,
        fmt_ms(span.min_ms),
        fmt_ms(span.median_ms),
        span.avg_ms
            .map(|value| format!("{:.1}ms", value))
            .unwrap_or_else(|| "-".to_string()),
        span.stddev_ms
            .map(|value| format!("{:.1}ms", value))
            .unwrap_or_else(|| "-".to_string()),
        fmt_ms(span.p75_ms),
        fmt_ms(span.p90_ms),
        fmt_ms(span.p95_ms),
        fmt_ms(span.max_ms),
        span.failures
    ));
}

pub(super) fn render_compare_markdown(compare: &extension_trace::TraceCompareOutput) -> String {
    let mut out = String::new();
    out.push_str("# Trace Compare\n\n");
    out.push_str(&format!("- **Before:** `{}`\n", compare.before_path));
    out.push_str(&format!("- **After:** `{}`\n", compare.after_path));
    if let (Some(before), Some(after)) = (&compare.before_scenario_id, &compare.after_scenario_id) {
        out.push_str(&format!("- **Scenario:** `{}` -> `{}`\n", before, after));
    }

    if !compare.spans.is_empty() {
        out.push_str("\n## Spans\n\n");
        out.push_str("| Span | before median | after median | median delta | median % | before avg | after avg | avg delta | avg % |\n");
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for span in &compare.spans {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                span.id,
                fmt_ms(span.before_median_ms),
                fmt_ms(span.after_median_ms),
                fmt_signal_delta_ms(span.median_delta_ms),
                fmt_percent(span.median_delta_percent),
                fmt_avg_ms(span.before_avg_ms),
                fmt_avg_ms(span.after_avg_ms),
                fmt_signal_delta_avg_ms(span.avg_delta_ms),
                fmt_percent(span.avg_delta_percent),
            ));
        }
    }

    if !compare.classification_summaries.is_empty() {
        out.push_str("\n## Critical Path Classification\n\n");
        out.push_str(
            "| Classification | spans | before median total | after median total | delta |\n",
        );
        out.push_str("|---|---:|---:|---:|---:|\n");
        for summary in &compare.classification_summaries {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                summary.classification,
                summary.span_count,
                fmt_ms(summary.before_total_median_ms),
                fmt_ms(summary.after_total_median_ms),
                fmt_signal_delta_ms(summary.median_delta_ms),
            ));
        }
    }

    if !compare.focus_span_ids.is_empty() {
        out.push_str("\n## Focus Spans\n\n");
        out.push_str(&format!(
            "- **Status:** `{}`\n",
            compare.focus_status.as_deref().unwrap_or("pass")
        ));
        out.push_str(&format!(
            "- **Regressions:** `{}`\n",
            compare.focus_regression_count
        ));
        out.push_str(&format!(
            "- **Failures:** `{}`\n",
            compare.focus_failure_count
        ));
        if compare.focus_spans.is_empty() {
            out.push_str("\nNo focused spans matched the compared aggregates.\n");
        } else {
            out.push_str("\n| Span | before median | after median | median delta | median % | before avg | after avg | avg delta | avg % | before failures | after failures |\n");
            out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
            for span in &compare.focus_spans {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                    span.id,
                    fmt_ms(span.before_median_ms),
                    fmt_ms(span.after_median_ms),
                    fmt_signal_delta_ms(span.median_delta_ms),
                    fmt_percent(span.median_delta_percent),
                    fmt_avg_ms(span.before_avg_ms),
                    fmt_avg_ms(span.after_avg_ms),
                    fmt_signal_delta_avg_ms(span.avg_delta_ms),
                    fmt_percent(span.avg_delta_percent),
                    fmt_count(span.before_failures),
                    fmt_count(span.after_failures),
                ));
            }
        }
    }

    push_guardrail_markdown(&mut out, "Before Guardrails", &compare.before_guardrails);
    push_guardrail_markdown(&mut out, "After Guardrails", &compare.after_guardrails);

    out
}

fn push_guardrail_markdown(
    out: &mut String,
    title: &str,
    guardrails: &[extension_trace::TraceGuardrailOutput],
) {
    if guardrails.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {title}\n\n"));
    out.push_str("| Guardrail | Source | Status | Failure |\n");
    out.push_str("|---|---|---|---|\n");
    for guardrail in guardrails {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            guardrail.label,
            guardrail.source,
            guardrail.status,
            guardrail
                .failure
                .as_deref()
                .map(|failure| format!("`{}`", failure.replace('`', "'")))
                .unwrap_or_else(|| "-".to_string())
        ));
    }
}

fn fmt_count(value: Option<usize>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

pub(super) fn render_matrix_markdown(matrix: &extension_trace::TraceVariantMatrixOutput) -> String {
    let mut out = String::new();
    out.push_str("# Trace Variant Matrix\n\n");
    out.push_str(&format!("- **Component:** `{}`\n", matrix.component));
    out.push_str(&format!("- **Scenario:** `{}`\n", matrix.scenario_id));
    out.push_str(&format!("- **Matrix:** `{}`\n", matrix.matrix));
    out.push_str(&format!("- **Status:** `{}`\n", matrix.status));
    out.push_str(&format!("- **Output dir:** `{}`\n", matrix.output_dir));
    out.push_str(&format!("- **Baseline:** `{}`\n", matrix.baseline_path));

    out.push_str("\n## Combinations\n\n");
    out.push_str("| Combination | Variants | Status | Exit | Aggregate | Compare |\n");
    out.push_str("|---|---|---|---:|---|---|\n");
    for run in &matrix.runs {
        let variants = if run.variants.is_empty() {
            "-".to_string()
        } else {
            run.variants
                .iter()
                .map(|variant| format!("`{}`", variant))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} | `{}` | `{}` |\n",
            run.label, variants, run.status, run.exit_code, run.aggregate_path, run.compare_path
        ));
    }

    out
}

pub(super) fn fmt_ms(value: Option<u64>) -> String {
    value
        .map(|value| format!("{}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_avg_ms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.1}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

pub(super) fn fmt_delta_ms(value: Option<i64>) -> String {
    value
        .map(|value| format!("{:+}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_signal_delta_ms(value: Option<i64>) -> String {
    let formatted = fmt_delta_ms(value);
    if value.is_some_and(|value| value != 0) {
        format!("**{}**", formatted)
    } else {
        formatted
    }
}

pub(super) fn fmt_delta_avg_ms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:+.1}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_signal_delta_avg_ms(value: Option<f64>) -> String {
    let formatted = fmt_delta_avg_ms(value);
    if value.is_some_and(|value| value.abs() >= f64::EPSILON) {
        format!("**{}**", formatted)
    } else {
        formatted
    }
}

fn fmt_percent(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:+.1}%", value))
        .unwrap_or_else(|| "-".to_string())
}
