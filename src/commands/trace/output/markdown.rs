//! Markdown rendering for trace command output.
//!
//! Renders aggregate, compare, matrix, and evidence reports plus the shared
//! duration/delta formatting helpers. Split out of the trace `output` module
//! to keep that command-output root under its structural line threshold; the
//! parent re-exports the externally consumed renderers so call sites keep the
//! stable `output::render_*` paths.

use std::fmt::Write as _;
use std::path::Path;

use homeboy::core::extension::trace as extension_trace;

#[cfg(test)]
pub(crate) fn render_aggregate_markdown(
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

    push_aggregate_metric_markdown(&mut out, &aggregate.metrics);

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

pub(crate) fn render_trace_run_evidence_markdown(
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

pub(crate) fn render_trace_aggregate_evidence_markdown(
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

    push_aggregate_metric_markdown(&mut out, &aggregate.metrics);

    push_guardrail_markdown(&mut out, "Assertion Status", &aggregate.guardrails);
    push_aggregate_artifact_completeness_markdown(&mut out, &aggregate.runs, &aggregate.spans);
    out
}

pub(crate) fn render_trace_compare_evidence_markdown(
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
    if let Some(status) = compare.metric_guardrail_status.as_deref() {
        let _ = writeln!(out, "- **Metric guardrail status:** `{}`", status);
    }

    push_compare_proof_markdown(&mut out, compare);

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

    push_metric_compare_markdown(&mut out, compare);
    push_metric_guardrail_markdown(&mut out, compare);

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

fn push_compare_proof_markdown(out: &mut String, compare: &extension_trace::TraceCompareOutput) {
    if !compare.proof_run_order.is_empty() {
        out.push_str("\n## A/B Run Matrix\n\n");
        out.push_str("| Run | Group | Iteration | Status | Exit | Artifact | Failure |\n");
        out.push_str("|---:|---|---:|---|---:|---|---|\n");
        for run in &compare.proof_run_order {
            let artifact = run
                .artifact_path
                .as_deref()
                .map(safe_report_path)
                .unwrap_or_else(|| "-".to_string());
            let failure = run
                .failure
                .as_deref()
                .map(|failure| format!("`{}`", failure.replace('`', "'")))
                .unwrap_or_else(|| "-".to_string());
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | `{}` | {} | `{}` | {} |",
                run.index, run.group, run.iteration, run.status, run.exit_code, artifact, failure
            );
        }
    }

    if !compare.caveats.is_empty() {
        out.push_str("\n## Caveats\n\n");
        for caveat in &compare.caveats {
            let _ = writeln!(out, "- {}", caveat);
        }
    }

    if let Some(browser_proof) = compare.browser_proof.as_ref() {
        out.push('\n');
        out.push_str(&browser_proof.markdown);
    }
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
        let status = assertion.status.as_str();
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

fn push_aggregate_metric_markdown(
    out: &mut String,
    metrics: &[extension_trace::TraceAggregateMetricOutput],
) {
    if metrics.is_empty() {
        return;
    }
    out.push_str("\n## Scalar Metrics\n\n");
    out.push_str("| Metric | n | min | median | max | samples |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|\n");
    for metric in metrics {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} |",
            metric.id,
            metric.n,
            fmt_number(metric.min),
            fmt_number(metric.median),
            fmt_number(metric.max),
            metric.samples.len(),
        );
    }
}

pub(crate) fn render_compare_markdown(compare: &extension_trace::TraceCompareOutput) -> String {
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

    push_metric_compare_markdown(&mut out, compare);

    push_compare_proof_markdown(&mut out, compare);

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

    push_metric_guardrail_markdown(&mut out, compare);

    push_guardrail_markdown(&mut out, "Before Guardrails", &compare.before_guardrails);
    push_guardrail_markdown(&mut out, "After Guardrails", &compare.after_guardrails);

    out
}

fn push_metric_compare_markdown(out: &mut String, compare: &extension_trace::TraceCompareOutput) {
    if compare.metrics.is_empty() {
        return;
    }
    out.push_str("\n## Metric Delta Summary\n\n");
    out.push_str("| Metric | before n | after n | before median | after median | delta | delta % | before min | after min | before max | after max |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for metric in &compare.metrics {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            metric.id,
            fmt_count(metric.before_n),
            fmt_count(metric.after_n),
            fmt_number(metric.before_median),
            fmt_number(metric.after_median),
            fmt_signal_delta_number(metric.median_delta),
            fmt_percent(metric.median_delta_percent),
            fmt_number(metric.before_min),
            fmt_number(metric.after_min),
            fmt_number(metric.before_max),
            fmt_number(metric.after_max),
        );
    }
}

fn push_metric_guardrail_markdown(out: &mut String, compare: &extension_trace::TraceCompareOutput) {
    if compare.metric_guardrails.is_empty() {
        return;
    }
    out.push_str("\n## Metric Guardrails\n\n");
    out.push_str("| Metric | Statistic | Policy | Status | Baseline | Candidate | Delta | Delta % | Failure |\n");
    out.push_str("|---|---|---|---|---:|---:|---:|---:|---|\n");
    for guardrail in &compare.metric_guardrails {
        let _ = writeln!(
            out,
            "| `{}` | `{}` | `{}` | `{}` | {} | {} | {} | {} | {} |",
            guardrail.metric,
            guardrail.statistic,
            guardrail.policy,
            guardrail.status,
            fmt_number(guardrail.before_value),
            fmt_number(guardrail.after_value),
            fmt_signal_delta_number(guardrail.delta),
            fmt_percent(guardrail.delta_percent),
            guardrail
                .failure
                .as_deref()
                .map(|failure| format!("`{}`", failure.replace('`', "'")))
                .unwrap_or_else(|| "-".to_string())
        );
    }
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

pub(crate) fn render_matrix_markdown(matrix: &extension_trace::TraceVariantMatrixOutput) -> String {
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

pub(crate) fn render_scenario_matrix_markdown(
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

pub(crate) fn fmt_ms(value: Option<u64>) -> String {
    value
        .map(|value| format!("{}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_avg_ms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.1}ms", value))
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn fmt_delta_ms(value: Option<i64>) -> String {
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

pub(crate) fn fmt_delta_avg_ms(value: Option<f64>) -> String {
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

fn fmt_number(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.3}", value))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_signal_delta_number(value: Option<f64>) -> String {
    let formatted = value
        .map(|value| format!("{:+.3}", value))
        .unwrap_or_else(|| "-".to_string());
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
