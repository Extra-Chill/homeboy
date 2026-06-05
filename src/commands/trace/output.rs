use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
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

#[cfg(test)]
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

pub(super) fn render_trace_run_evidence_markdown(
    run: &extension_trace::report::TraceRunOutput,
) -> String {
    let mut out = String::new();
    let scenario = run
        .results
        .as_ref()
        .map(|results| results.scenario_id.as_str())
        .unwrap_or("unknown");
    let _ = writeln!(out, "# Trace Evidence: `{}`\n", scenario);
    let _ = writeln!(out, "- **Command:** `homeboy trace`");
    let _ = writeln!(out, "- **Component:** `{}`", run.component);
    let _ = writeln!(out, "- **Scenario:** `{}`", scenario);
    let _ = writeln!(out, "- **Status:** `{}`", run.status);
    let _ = writeln!(out, "- **Exit code:** `{}`", run.exit_code);
    if let Some(summary) = run
        .results
        .as_ref()
        .and_then(|results| results.summary.as_ref())
    {
        let _ = writeln!(out, "- **Summary:** {}", summary);
    }
    if let Some(failure) = run
        .results
        .as_ref()
        .and_then(|results| results.failure.as_ref())
    {
        let _ = writeln!(out, "- **Failure:** {}", failure);
    }
    push_trace_refs_markdown(&mut out, run.rig_state.as_ref());
    extension_trace::push_overlay_markdown(&mut out, &run.overlays);

    if !run.span_summaries.is_empty() {
        out.push_str("\n## Metric Summary\n\n");
        out.push_str("| Span | From | To | Duration | Status | Metadata |\n");
        out.push_str("|---|---|---|---:|---|---|\n");
        for span in &run.span_summaries {
            let duration = span
                .duration_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            let status = extension_trace::format_span_summary_status(span);
            let metadata = extension_trace::format_span_summary_metadata(span.metadata.as_ref());
            let _ = writeln!(
                out,
                "| `{}` | `{}` | `{}` | {} | {} | {} |",
                span.id, span.from, span.to, duration, status, metadata
            );
        }
    }

    if let Some(results) = run.results.as_ref() {
        push_trace_assertions_markdown(&mut out, &results.assertions);
        push_browser_summary_markdown(&mut out, &results.artifacts);
        push_trace_artifact_completeness_markdown(&mut out, &results.artifacts, &run.artifacts);
    } else {
        out.push_str("\n## Artifact Completeness\n\n- **Status:** `missing`\n- No trace results were available.\n");
    }

    out
}

pub(super) fn render_trace_aggregate_evidence_markdown(
    aggregate: &extension_trace::TraceAggregateOutput,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Trace Evidence: `{}`\n", aggregate.scenario_id);
    let _ = writeln!(out, "- **Command:** `{}`", aggregate.command);
    let _ = writeln!(out, "- **Component:** `{}`", aggregate.component);
    let _ = writeln!(out, "- **Scenario:** `{}`", aggregate.scenario_id);
    let _ = writeln!(out, "- **Status:** `{}`", aggregate.status);
    let _ = writeln!(out, "- **Exit code:** `{}`", aggregate.exit_code);
    let _ = writeln!(out, "- **Runs:** `{}`", aggregate.run_count);
    let _ = writeln!(out, "- **Failures:** `{}`", aggregate.failure_count);
    if let Some(schedule) = aggregate.schedule.as_deref() {
        let _ = writeln!(out, "- **Schedule:** `{}`", schedule);
    }
    push_trace_refs_markdown(&mut out, aggregate.rig_state.as_ref());
    extension_trace::push_overlay_markdown(&mut out, &aggregate.overlays);

    if !aggregate.spans.is_empty() {
        out.push_str("\n## Metric Summary\n\n");
        out.push_str(
            "| Span | n | min | median | avg | stddev | p75 | p90 | p95 | max | failures |\n",
        );
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for span in &aggregate.spans {
            push_aggregate_span_row(&mut out, span);
        }
    }

    push_guardrail_markdown(&mut out, "Assertion Status", &aggregate.guardrails);
    push_aggregate_artifact_completeness_markdown(&mut out, &aggregate.runs, &aggregate.spans);
    out
}

pub(super) fn render_trace_compare_evidence_markdown(
    compare: &extension_trace::TraceCompareOutput,
) -> String {
    let mut out = String::new();
    out.push_str("# Trace Compare Evidence\n\n");
    out.push_str("- **Command:** `trace.compare.spans`\n");
    let _ = writeln!(
        out,
        "- **Before:** `{}`",
        safe_report_path(&compare.before_path)
    );
    let _ = writeln!(
        out,
        "- **After:** `{}`",
        safe_report_path(&compare.after_path)
    );
    if let (Some(before), Some(after)) = (&compare.before_component, &compare.after_component) {
        let _ = writeln!(out, "- **Components:** `{}` -> `{}`", before, after);
    }
    if let (Some(before), Some(after)) = (&compare.before_scenario_id, &compare.after_scenario_id) {
        let _ = writeln!(out, "- **Scenarios:** `{}` -> `{}`", before, after);
    }
    let _ = writeln!(out, "- **Span count:** `{}`", compare.span_count);
    if let Some(status) = compare.focus_status.as_deref() {
        let _ = writeln!(out, "- **Focus assertion status:** `{}`", status);
    }
    if let Some(status) = compare.guardrail_status.as_deref() {
        let _ = writeln!(out, "- **Guardrail assertion status:** `{}`", status);
    }

    if !compare.spans.is_empty() {
        out.push_str("\n## Metric Delta Summary\n\n");
        out.push_str("| Span | before median | after median | median delta | median % | before avg | after avg | avg delta | avg % |\n");
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for span in &compare.spans {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                span.id,
                fmt_ms(span.before_median_ms),
                fmt_ms(span.after_median_ms),
                fmt_signal_delta_ms(span.median_delta_ms),
                fmt_percent(span.median_delta_percent),
                fmt_avg_ms(span.before_avg_ms),
                fmt_avg_ms(span.after_avg_ms),
                fmt_signal_delta_avg_ms(span.avg_delta_ms),
                fmt_percent(span.avg_delta_percent),
            );
        }
    }

    if !compare.focus_span_ids.is_empty() {
        out.push_str("\n## Assertion Status\n\n");
        let _ = writeln!(
            out,
            "- Focus spans: `{}`",
            compare.focus_span_ids.join("`, `")
        );
        let _ = writeln!(
            out,
            "- Focus regressions: `{}`",
            compare.focus_regression_count
        );
        let _ = writeln!(out, "- Focus failures: `{}`", compare.focus_failure_count);
    }
    push_guardrail_markdown(&mut out, "Before Guardrails", &compare.before_guardrails);
    push_guardrail_markdown(&mut out, "After Guardrails", &compare.after_guardrails);

    out.push_str("\n## Artifact Completeness\n\n");
    out.push_str("- **Status:** `input-only`\n");
    out.push_str("- Compare evidence references aggregate JSON inputs; run-level artifact completeness is reported by the aggregate reports.\n");
    out
}

fn push_trace_refs_markdown(
    out: &mut String,
    rig_state: Option<&homeboy::core::rig::RigStateSnapshot>,
) {
    let Some(rig_state) = rig_state else {
        return;
    };
    out.push_str("\n## Git Refs\n\n");
    let _ = writeln!(out, "- **Rig:** `{}`", rig_state.rig_id);
    let _ = writeln!(out, "- **Captured at:** `{}`", rig_state.captured_at);
    out.push_str("\n| Component | Branch | SHA | Path |\n");
    out.push_str("|---|---|---|---|\n");
    for (component, snapshot) in &rig_state.components {
        let _ = writeln!(
            out,
            "| `{}` | `{}` | `{}` | `{}` |",
            component,
            snapshot.branch.as_deref().unwrap_or("unknown"),
            snapshot.sha.as_deref().unwrap_or("unknown"),
            safe_report_path(&snapshot.path),
        );
    }
}

fn push_trace_assertions_markdown(
    out: &mut String,
    assertions: &[extension_trace::TraceAssertion],
) {
    out.push_str("\n## Assertion Status\n\n");
    if assertions.is_empty() {
        out.push_str("- No assertions were reported.\n");
        return;
    }
    for assertion in assertions {
        let status = match assertion.status {
            extension_trace::TraceAssertionStatus::Pass => "pass",
            extension_trace::TraceAssertionStatus::Fail => "fail",
            extension_trace::TraceAssertionStatus::Error => "error",
        };
        match assertion.message.as_deref() {
            Some(message) => {
                let _ = writeln!(out, "- `{}`: **{}** - {}", assertion.id, status, message);
            }
            None => {
                let _ = writeln!(out, "- `{}`: **{}**", assertion.id, status);
            }
        }
    }
}

fn push_browser_summary_markdown(out: &mut String, artifacts: &[extension_trace::TraceArtifact]) {
    let browser_artifacts = artifacts
        .iter()
        .filter(|artifact| {
            let label = artifact.label.to_ascii_lowercase();
            let path = artifact.path.to_ascii_lowercase();
            label.contains("console")
                || label.contains("browser")
                || label.contains("screenshot")
                || label.contains("trace")
                || path.contains("console")
                || path.contains("screenshot")
                || path.contains("trace")
        })
        .collect::<Vec<_>>();
    if browser_artifacts.is_empty() {
        return;
    }
    out.push_str("\n## Browser Evidence Summary\n\n");
    for artifact in browser_artifacts {
        let _ = writeln!(
            out,
            "- **{}:** `{}`",
            artifact.label,
            safe_report_path(&artifact.path)
        );
    }
}

fn push_trace_artifact_completeness_markdown(
    out: &mut String,
    declared_artifacts: &[extension_trace::TraceArtifact],
    reportable_artifacts: &[extension_trace::TraceArtifact],
) {
    out.push_str("\n## Artifact Completeness\n\n");
    let status = artifact_completeness_status(declared_artifacts.len(), reportable_artifacts.len());
    let _ = writeln!(out, "- **Status:** `{}`", status);
    let _ = writeln!(
        out,
        "- **Declared artifacts:** `{}`",
        declared_artifacts.len()
    );
    let _ = writeln!(
        out,
        "- **Reportable artifacts:** `{}`",
        reportable_artifacts.len()
    );
    if reportable_artifacts.is_empty() {
        out.push_str("- No reportable artifact paths were produced.\n");
        return;
    }
    for artifact in reportable_artifacts {
        let _ = writeln!(
            out,
            "- **{}:** `{}`",
            artifact.label,
            safe_report_path(&artifact.path)
        );
    }
}

fn push_aggregate_artifact_completeness_markdown(
    out: &mut String,
    runs: &[extension_trace::TraceAggregateRunOutput],
    spans: &[extension_trace::TraceAggregateSpanOutput],
) {
    out.push_str("\n## Artifact Completeness\n\n");
    let run_artifact_count = runs
        .iter()
        .filter(|run| !run.artifact_path.is_empty())
        .count();
    let span_artifact_count = spans
        .iter()
        .filter(|span| span.max_artifact_path.is_some())
        .count();
    let status = if runs.is_empty() {
        "missing"
    } else if run_artifact_count == runs.len() {
        "complete"
    } else if run_artifact_count > 0 || span_artifact_count > 0 {
        "partial"
    } else {
        "missing"
    };
    let _ = writeln!(out, "- **Status:** `{}`", status);
    let _ = writeln!(
        out,
        "- **Run artifacts:** `{}/{}`",
        run_artifact_count,
        runs.len()
    );
    let _ = writeln!(
        out,
        "- **Span outlier artifacts:** `{}`",
        span_artifact_count
    );
    for run in runs.iter().filter(|run| !run.artifact_path.is_empty()) {
        let _ = writeln!(
            out,
            "- Run {}: `{}` `{}`",
            run.index,
            run.status,
            safe_report_path(&run.artifact_path)
        );
    }
    for span in spans.iter().filter(|span| span.max_artifact_path.is_some()) {
        let _ = writeln!(
            out,
            "- Span `{}` max artifact: `{}`",
            span.id,
            safe_report_path(span.max_artifact_path.as_deref().unwrap_or_default())
        );
    }
}

fn artifact_completeness_status(declared: usize, reportable: usize) -> &'static str {
    if declared == 0 {
        "missing"
    } else if declared == reportable {
        "complete"
    } else if reportable > 0 {
        "partial"
    } else {
        "missing"
    }
}

fn safe_report_path(path: &str) -> String {
    if is_url(path) || is_relative_artifact_path(path) {
        return path.to_string();
    }
    let file_name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("path");
    format!("[local path redacted: {file_name}]")
}

fn is_url(path: &str) -> bool {
    path.starts_with("https://") || path.starts_with("http://")
}

fn is_relative_artifact_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

#[cfg(test)]
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
    if let (Some(before), Some(after)) = (&compare.before_target, &compare.after_target) {
        out.push_str(&format!("- **Targets:** `{}` -> `{}`\n", before, after));
    }
    if let (Some(before), Some(after)) = (&compare.before_git_sha, &compare.after_git_sha) {
        out.push_str(&format!("- **Git SHAs:** `{}` -> `{}`\n", before, after));
    }
    if let (Some(before), Some(after)) = (&compare.before_status, &compare.after_status) {
        out.push_str(&format!("- **Status:** `{}` -> `{}`\n", before, after));
    }
    if let Some(output_dir) = compare.output_dir.as_deref() {
        out.push_str(&format!("- **Output dir:** `{}`\n", output_dir));
    }
    if let Some(summary_path) = compare.summary_path.as_deref() {
        out.push_str(&format!("- **Summary:** `{}`\n", summary_path));
    }
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

pub(super) fn render_scenario_matrix_markdown(
    matrix: &extension_trace::TraceScenarioMatrixOutput,
) -> String {
    let mut out = String::new();
    out.push_str("# Trace Scenario Matrix\n\n");
    out.push_str(&format!("- **Component:** `{}`\n", matrix.component));
    out.push_str(&format!("- **Scenario:** `{}`\n", matrix.scenario_id));
    out.push_str(&format!("- **Status:** `{}`\n", matrix.status));
    out.push_str(&format!("- **Cells:** `{}`\n", matrix.cell_count));
    out.push_str(&format!("- **Failures:** `{}`\n", matrix.failure_count));
    out.push_str(&format!("- **Output dir:** `{}`\n", matrix.output_dir));
    out.push_str(&format!("- **Matrix JSON:** `{}`\n", matrix.matrix_path));

    if !matrix.axes.is_empty() {
        out.push_str("\n## Axes\n\n");
        for axis in &matrix.axes {
            out.push_str(&format!(
                "- `{}`: {}\n",
                axis.name,
                axis.values
                    .iter()
                    .map(|value| format!("`{}`", value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    out.push_str("\n## Cells\n\n");
    out.push_str("| Cell | Axes | Status | Exit | Artifact | Output | Failure |\n");
    out.push_str("|---|---|---|---:|---|---|---|\n");
    for cell in &matrix.cells {
        let axes = cell
            .axes
            .iter()
            .map(|(key, value)| format!("`{}`=`{}`", key, value))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} | `{}` | `{}` | {} |\n",
            cell.label,
            axes,
            cell.status,
            cell.exit_code,
            cell.artifact_path,
            cell.output_path,
            cell.failure
                .as_deref()
                .map(|failure| format!("`{}`", failure.replace('`', "'")))
                .unwrap_or_else(|| "-".to_string())
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
