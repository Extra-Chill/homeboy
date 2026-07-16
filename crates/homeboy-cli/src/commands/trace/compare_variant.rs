use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;
use homeboy::core::rig;
use homeboy::core::trace_compare;

use super::aggregate::{
    aggregate_metric, aggregate_span, TraceAggregateMetricSample, TraceAggregateSpanSample,
};
use super::output::{
    classification_summaries, compare_trace_aggregates_with_focus, TraceAggregateIdentity,
    TraceAggregateInput, TraceAggregateMetricInput, TraceAggregateRunInput,
    TraceAggregateSpanInput, TraceOverlayInput,
};
use super::repeat::{focus_aggregate_spans, run_repeat};
use super::{
    apply_command_target_component, apply_resolved_trace_secret_env, plan_trace_run_order,
    resolve_trace_secret_env_once, trace_secret_env_project_id_for_args,
    validate_trace_variants_for_args, TraceArgs, TraceRunPlanEntry, TraceSchedule,
};
use crate::commands::CmdResult;

const TRACE_COMPARE_VARIANT_BASELINE_FILE: &str = "baseline.json";
const TRACE_COMPARE_VARIANT_VARIANT_FILE: &str = "variant.json";
const TRACE_COMPARE_VARIANT_COMPARE_FILE: &str = "compare.json";
const TRACE_COMPARE_VARIANT_RUN_ORDER_FILE: &str = "run-order.json";
const TRACE_COMPARE_VARIANT_SUMMARY_FILE: &str = "summary.md";

pub(super) fn run_compare_variant(mut args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    let output_dir = args.output_dir.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec!["--output-dir".to_string()])
    })?;
    if !args.variants.is_empty() && !args.overlays.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--variant",
            "mixing --variant and --overlay would make stack order ambiguous; use one ordered stack source",
            None,
            None,
        ));
    }
    if args.overlays.is_empty() && args.variants.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--overlay",
            "trace compare-variant requires at least one --overlay or --variant for the variant run",
            None,
            None,
        ));
    }
    if args.keep_overlay {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--keep-overlay",
            "trace compare-variant reuses the same component checkout and must revert overlays between runs",
            None,
            None,
        ));
    }

    apply_command_target_component(&mut args);
    args.aggregate = Some("spans".to_string());
    args.json_summary = false;
    args.report = None;
    args.compare_after = None;
    validate_trace_variants_for_args(&args)?;
    let project_id = trace_secret_env_project_id_for_args(&args)?;
    let resolved_trace_secret_env =
        resolve_trace_secret_env_once(&args.secret_env, Some(&project_id))?;
    apply_resolved_trace_secret_env(&mut args, resolved_trace_secret_env.as_ref());
    let focus_spans = args.focus_spans.clone();
    let regression_threshold = args.regression_threshold;
    let regression_min_delta_ms = args.regression_min_delta_ms;
    let metric_guardrails = args.metric_guardrails.clone();

    let (baseline, variant, run_order) = run_compare_variant_pair(args)?;

    trace_compare::prepare_compare_variant_dir(&output_dir)?;
    let baseline_path = output_dir.join(TRACE_COMPARE_VARIANT_BASELINE_FILE);
    let variant_path = output_dir.join(TRACE_COMPARE_VARIANT_VARIANT_FILE);
    let compare_path = output_dir.join(TRACE_COMPARE_VARIANT_COMPARE_FILE);
    let run_order_path = output_dir.join(TRACE_COMPARE_VARIANT_RUN_ORDER_FILE);
    let summary_path = output_dir.join(TRACE_COMPARE_VARIANT_SUMMARY_FILE);

    trace_compare::write_compare_variant_json(&baseline_path, &baseline)?;
    trace_compare::write_compare_variant_json(&variant_path, &variant)?;
    trace_compare::write_compare_variant_json(&run_order_path, &run_order)?;

    let compare = compare_trace_aggregates_with_focus(
        &baseline_path,
        aggregate_to_compare_input(&baseline),
        &variant_path,
        aggregate_to_compare_input(&variant),
        &focus_spans,
        regression_threshold,
        regression_min_delta_ms,
        &metric_guardrails,
    );
    trace_compare::write_compare_variant_json(&compare_path, &compare)?;
    write_trace_compare_variant_summary(
        &summary_path,
        &output_dir,
        &baseline,
        &variant,
        &compare,
        &run_order,
    )?;

    let exit_code = if baseline.exit_code == 0
        && variant.exit_code == 0
        && compare.focus_status.as_deref() != Some("fail")
        && compare.guardrail_status.as_deref() != Some("fail")
        && compare.metric_guardrail_status.as_deref() != Some("fail")
    {
        0
    } else {
        1
    };
    Ok((TraceCommandOutput::Compare(compare), exit_code))
}

fn run_compare_variant_pair(
    args: TraceArgs,
) -> homeboy::core::Result<(
    extension_trace::TraceAggregateOutput,
    extension_trace::TraceAggregateOutput,
    Vec<extension_trace::TraceRunOrderEntryOutput>,
)> {
    if args.schedule == TraceSchedule::Grouped {
        let mut baseline_args = args.clone();
        baseline_args.overlays.clear();
        baseline_args.variants.clear();
        let baseline = run_repeat_output(baseline_args)?;
        let variant = run_repeat_output(args)?;
        let run_order = plan_trace_run_order(
            variant.repeat,
            TraceSchedule::Grouped,
            &["baseline", "variant"],
        )
        .into_iter()
        .map(trace_run_order_entry_output)
        .collect();
        return Ok((baseline, variant, run_order));
    }

    let plan = plan_trace_run_order(args.repeat, args.schedule, &["baseline", "variant"]);
    let mut baseline_args = args.clone();
    baseline_args.overlays.clear();
    baseline_args.variants.clear();
    let mut baseline = TraceCompareVariantAggregateBuilder::new("baseline", &baseline_args);
    let mut variant = TraceCompareVariantAggregateBuilder::new("variant", &args);

    for entry in &plan {
        let mut run_args = if entry.group() == "baseline" {
            baseline_args.clone()
        } else {
            args.clone()
        };
        run_args.repeat = 1;
        let single = run_repeat_output(run_args)?;
        if entry.group() == "baseline" {
            baseline.push(entry, single);
        } else {
            variant.push(entry, single);
        }
    }

    Ok((
        baseline.finish(),
        variant.finish(),
        plan.into_iter().map(trace_run_order_entry_output).collect(),
    ))
}

fn trace_run_order_entry_output(
    entry: TraceRunPlanEntry,
) -> extension_trace::TraceRunOrderEntryOutput {
    extension_trace::TraceRunOrderEntryOutput {
        index: entry.index(),
        group: entry.group().to_string(),
        iteration: entry.iteration(),
    }
}

struct TraceCompareVariantAggregateBuilder {
    group: &'static str,
    args: TraceArgs,
    template: Option<extension_trace::TraceAggregateOutput>,
    runs: Vec<extension_trace::TraceAggregateRunOutput>,
    run_order: Vec<extension_trace::TraceRunOrderEntryOutput>,
    span_samples: BTreeMap<String, Vec<TraceAggregateSpanSample>>,
    metric_samples: BTreeMap<String, Vec<TraceAggregateMetricSample>>,
    span_failures: BTreeMap<String, usize>,
    all_span_ids: BTreeSet<String>,
    overlays: Vec<extension_trace::run::TraceOverlay>,
    guardrails: Vec<extension_trace::TraceGuardrailOutput>,
    guardrail_failure_count: usize,
    failure_count: usize,
}

impl TraceCompareVariantAggregateBuilder {
    fn new(group: &'static str, args: &TraceArgs) -> Self {
        Self {
            group,
            args: args.clone(),
            template: None,
            runs: Vec::new(),
            run_order: Vec::new(),
            span_samples: BTreeMap::new(),
            metric_samples: BTreeMap::new(),
            span_failures: BTreeMap::new(),
            all_span_ids: BTreeSet::new(),
            overlays: Vec::new(),
            guardrails: Vec::new(),
            guardrail_failure_count: 0,
            failure_count: 0,
        }
    }

    fn push(&mut self, plan: &TraceRunPlanEntry, aggregate: extension_trace::TraceAggregateOutput) {
        if self.template.is_none() {
            self.template = Some(aggregate.clone());
        }
        if self.overlays.is_empty() && !aggregate.overlays.is_empty() {
            self.overlays = aggregate.overlays.clone();
        }
        self.failure_count += aggregate.failure_count;
        self.guardrail_failure_count += aggregate.guardrail_failure_count;
        self.guardrails.extend(aggregate.guardrails.clone());
        self.run_order
            .push(extension_trace::TraceRunOrderEntryOutput {
                index: plan.index(),
                group: self.group.to_string(),
                iteration: plan.iteration(),
            });

        for mut run in aggregate.runs {
            run.index = plan.index();
            self.runs.push(run);
        }

        for span in aggregate.spans {
            self.all_span_ids.insert(span.id.clone());
            if let Some(duration) = span.median_ms {
                self.span_samples.entry(span.id.clone()).or_default().push(
                    TraceAggregateSpanSample {
                        duration_ms: duration,
                        run_index: plan.index(),
                        artifact_path: span.max_artifact_path.clone().unwrap_or_default(),
                    },
                );
            }
            if span.failures > 0 {
                *self.span_failures.entry(span.id).or_default() += span.failures;
            }
        }
        for metric in aggregate.metrics {
            for sample in metric.samples {
                self.metric_samples
                    .entry(metric.id.clone())
                    .or_default()
                    .push(TraceAggregateMetricSample {
                        value: sample.value,
                        run_index: plan.index(),
                        artifact_path: sample.artifact_path,
                    });
            }
        }
    }

    fn finish(mut self) -> extension_trace::TraceAggregateOutput {
        let template = self.template.take();
        let spans = self
            .all_span_ids
            .into_iter()
            .map(|id| {
                let samples = self.span_samples.remove(&id).unwrap_or_default();
                let failures = self.span_failures.remove(&id).unwrap_or(0);
                aggregate_span(id, samples, failures)
            })
            .collect::<Vec<_>>();
        let focus_spans = focus_aggregate_spans(&spans, &self.args.focus_spans);
        let metrics = self
            .metric_samples
            .into_iter()
            .map(|(id, samples)| aggregate_metric(id, samples))
            .collect::<Vec<_>>();
        let classification_summaries = classification_summaries(&spans);
        let exit_code = if self.failure_count == 0 && self.guardrail_failure_count == 0 {
            0
        } else {
            1
        };
        extension_trace::TraceAggregateOutput {
            command: "trace.aggregate.spans",
            passed: exit_code == 0,
            status: if exit_code == 0 { "pass" } else { "fail" }.to_string(),
            component: template
                .as_ref()
                .map(|aggregate| aggregate.component.clone())
                .or_else(|| self.args.comp.component.clone())
                .unwrap_or_default(),
            scenario_id: template
                .as_ref()
                .map(|aggregate| aggregate.scenario_id.clone())
                .or_else(|| self.args.scenario_arg.clone())
                .or_else(|| self.args.scenario.clone())
                .unwrap_or_default(),
            phase_preset: template
                .as_ref()
                .and_then(|aggregate| aggregate.phase_preset.clone())
                .or_else(|| self.args.phase_preset.clone()),
            repeat: self.args.repeat,
            run_count: self.runs.len(),
            failure_count: self.failure_count,
            exit_code,
            schedule: Some(self.args.schedule.as_str().to_string()),
            run_order: self.run_order,
            rig_state: template.and_then(|aggregate| aggregate.rig_state),
            overlays: self.overlays,
            runs: self.runs,
            spans,
            metrics,
            guardrails: self.guardrails,
            guardrail_failure_count: self.guardrail_failure_count,
            focus_span_ids: self.args.focus_spans.clone(),
            focus_spans,
            classification_summaries,
            unmatched_span_metadata_ids: Vec::new(),
            profile: None,
        }
    }
}

fn run_repeat_output(
    args: TraceArgs,
) -> homeboy::core::Result<extension_trace::TraceAggregateOutput> {
    let (output, _exit_code) = run_repeat(args)?;
    match output {
        TraceCommandOutput::Aggregate(aggregate) => Ok(aggregate),
        _ => Err(homeboy::core::Error::internal_unexpected(
            "trace compare-variant expected aggregate output",
        )),
    }
}

fn aggregate_to_compare_input(
    aggregate: &extension_trace::TraceAggregateOutput,
) -> TraceAggregateInput {
    TraceAggregateInput {
        component: Some(aggregate.component.clone()),
        scenario_id: Some(aggregate.scenario_id.clone()),
        phase_preset: aggregate.phase_preset.clone(),
        repeat: Some(aggregate.repeat),
        rig_state: aggregate
            .rig_state
            .as_ref()
            .and_then(|rig_state| serde_json::to_value(rig_state).ok()),
        overlays: aggregate
            .overlays
            .iter()
            .map(|overlay| TraceOverlayInput {
                path: overlay.path.clone(),
                component_path: overlay.component_path.clone(),
                touched_files: overlay.touched_files.clone(),
                kept: overlay.kept,
            })
            .collect(),
        runs: aggregate
            .runs
            .iter()
            .map(|run| TraceAggregateRunInput {
                index: run.index,
                status: run.status.clone(),
                exit_code: run.exit_code,
                artifact_path: run.artifact_path.clone(),
                failure: run.failure.clone(),
            })
            .collect(),
        spans: aggregate
            .spans
            .iter()
            .map(|span| TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: span.id.clone(),
                    n: span.n,
                },
                median_ms: span.median_ms,
                avg_ms: span.avg_ms,
                max_ms: span.max_ms,
                max_run_index: span.max_run_index,
                max_artifact_path: span.max_artifact_path.clone(),
                failures: span.failures,
                metadata: span.metadata.clone(),
            })
            .collect(),
        metrics: aggregate
            .metrics
            .iter()
            .map(|metric| TraceAggregateMetricInput {
                identity: TraceAggregateIdentity {
                    id: metric.id.clone(),
                    n: metric.n,
                },
                min: metric.min,
                median: metric.median,
                max: metric.max,
                samples: metric.samples.clone(),
            })
            .collect(),
        guardrails: aggregate.guardrails.clone(),
        guardrail_failure_count: aggregate.guardrail_failure_count,
    }
}

fn write_trace_compare_variant_summary(
    path: &Path,
    output_dir: &Path,
    baseline: &extension_trace::TraceAggregateOutput,
    variant: &extension_trace::TraceAggregateOutput,
    compare: &extension_trace::TraceCompareOutput,
    run_order: &[extension_trace::TraceRunOrderEntryOutput],
) -> homeboy::core::Result<()> {
    let mut out = String::new();
    out.push_str(&format!(
        "# Trace Compare Variant: `{}`\n\n",
        baseline.scenario_id
    ));
    out.push_str(&format!("- **Component:** `{}`\n", baseline.component));
    out.push_str(&format!("- **Output dir:** `{}`\n", output_dir.display()));
    out.push_str(&format!("- **Baseline runs:** `{}`\n", baseline.run_count));
    out.push_str(&format!("- **Variant runs:** `{}`\n", variant.run_count));
    out.push_str(&format!(
        "- **Schedule:** `{}`\n",
        baseline.schedule.as_deref().unwrap_or("grouped")
    ));
    out.push_str(&format!("- **Baseline status:** `{}`\n", baseline.status));
    out.push_str(&format!("- **Variant status:** `{}`\n", variant.status));
    if compare.guardrail_status.is_some() {
        out.push_str(&format!(
            "- **Guardrails:** `{}` (`{}` failures)\n",
            compare.guardrail_status.as_deref().unwrap_or("pass"),
            compare.guardrail_failure_count
        ));
    }
    out.push_str(&format!(
        "- **Files:** `{}`, `{}`, `{}`, `{}`\n",
        TRACE_COMPARE_VARIANT_BASELINE_FILE,
        TRACE_COMPARE_VARIANT_VARIANT_FILE,
        TRACE_COMPARE_VARIANT_COMPARE_FILE,
        TRACE_COMPARE_VARIANT_RUN_ORDER_FILE
    ));

    push_run_order_summary(&mut out, run_order);
    push_component_sha_summary(&mut out, "Baseline", baseline.rig_state.as_ref());
    push_component_sha_summary(&mut out, "Variant", variant.rig_state.as_ref());
    push_overlay_summary(&mut out, &variant.overlays);
    push_focus_span_summary(&mut out, baseline, variant, compare);
    push_compare_variant_span_summary(&mut out, compare);

    trace_compare::write_compare_variant_summary(path, &out)
}

fn push_run_order_summary(
    out: &mut String,
    run_order: &[extension_trace::TraceRunOrderEntryOutput],
) {
    out.push_str("\n## Run Order\n\n");
    if run_order.is_empty() {
        out.push_str("No run order was recorded.\n");
        return;
    }
    out.push_str("| Index | Group | Iteration |\n");
    out.push_str("|---:|---|---:|\n");
    for run in run_order {
        out.push_str(&format!(
            "| {} | `{}` | {} |\n",
            run.index, run.group, run.iteration
        ));
    }
}

fn push_focus_span_summary(
    out: &mut String,
    baseline: &extension_trace::TraceAggregateOutput,
    variant: &extension_trace::TraceAggregateOutput,
    compare: &extension_trace::TraceCompareOutput,
) {
    if compare.focus_span_ids.is_empty() {
        return;
    }
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
        return;
    }
    out.push_str(&format!(
        "- **Noise context:** {}\n",
        focus_noise_context(baseline, variant, compare)
    ));
    out.push_str("\n| Span | baseline median | variant median | delta | delta % | baseline failures | variant failures |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for span in &compare.focus_spans {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            span.id,
            fmt_summary_ms(span.before_median_ms),
            fmt_summary_ms(span.after_median_ms),
            span.median_delta_ms
                .map(|value| format!("{:+}ms", value))
                .unwrap_or_else(|| "-".to_string()),
            span.median_delta_percent
                .map(|value| format!("{:+.1}%", value))
                .unwrap_or_else(|| "-".to_string()),
            span.before_failures
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            span.after_failures
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        ));
    }

    out.push_str("\n### Focus Variance And Outliers\n\n");
    out.push_str("| Group | Span | min | median | max | max run | max artifact |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---|\n");
    push_focus_variance_rows(out, "baseline", &baseline.focus_spans);
    push_focus_variance_rows(out, "variant", &variant.focus_spans);
}

fn push_focus_variance_rows(
    out: &mut String,
    group: &str,
    spans: &[extension_trace::TraceAggregateSpanOutput],
) {
    for span in spans {
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} | {} | `{}` |\n",
            group,
            span.id,
            fmt_summary_ms(span.min_ms),
            fmt_summary_ms(span.median_ms),
            fmt_summary_ms(span.max_ms),
            span.max_run_index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            span.max_artifact_path.as_deref().unwrap_or("-")
        ));
    }
}

fn focus_noise_context(
    baseline: &extension_trace::TraceAggregateOutput,
    variant: &extension_trace::TraceAggregateOutput,
    compare: &extension_trace::TraceCompareOutput,
) -> &'static str {
    if compare.focus_regression_count > 0 || compare.focus_failure_count > 0 {
        return "focused span threshold failed; inspect outlier artifacts before trusting the delta";
    }
    let mut noisy = false;
    for span in &compare.focus_spans {
        let delta = span.median_delta_ms.map(i64::abs).unwrap_or(0) as u64;
        if delta == 0 {
            continue;
        }
        let baseline_spread = span_spread_ms(&baseline.focus_spans, &span.id);
        let variant_spread = span_spread_ms(&variant.focus_spans, &span.id);
        if baseline_spread.max(variant_spread) >= delta {
            noisy = true;
        }
    }
    if noisy {
        "focused medians passed thresholds, but within-group spread overlaps at least one delta"
    } else {
        "focused medians passed thresholds and observed spread does not dominate the deltas"
    }
}

fn span_spread_ms(spans: &[extension_trace::TraceAggregateSpanOutput], id: &str) -> u64 {
    spans
        .iter()
        .find(|span| span.id == id)
        .and_then(|span| Some(span.max_ms? - span.min_ms?))
        .unwrap_or(0)
}

fn push_component_sha_summary(
    out: &mut String,
    label: &str,
    rig_state: Option<&rig::RigStateSnapshot>,
) {
    out.push_str(&format!("\n## {label} Component SHAs\n\n"));
    let Some(rig_state) = rig_state else {
        out.push_str("- No rig state captured.\n");
        return;
    };
    for (component_id, component) in &rig_state.components {
        out.push_str(&format!(
            "- `{}`: `{}` ({})\n",
            component_id,
            component.sha.as_deref().unwrap_or("unknown"),
            component.path
        ));
    }
}

fn push_overlay_summary(out: &mut String, overlays: &[extension_trace::run::TraceOverlay]) {
    out.push_str("\n## Variant Overlays\n\n");
    if overlays.is_empty() {
        out.push_str("- No overlays applied.\n");
        return;
    }
    for overlay in overlays {
        out.push_str(&format!("- `{}`\n", overlay.path));
        for file in &overlay.touched_files {
            out.push_str(&format!("- touched: `{}`\n", file));
        }
        if overlay.touched_files.is_empty() {
            out.push_str("- touched: none reported\n");
        }
    }
}

fn push_compare_variant_span_summary(
    out: &mut String,
    compare: &extension_trace::TraceCompareOutput,
) {
    out.push_str("\n## Largest Span Deltas\n\n");
    if compare.spans.is_empty() {
        out.push_str("No comparable spans were produced.\n");
        return;
    }
    out.push_str("| Span | baseline median | variant median | delta | delta % |\n");
    out.push_str("|---|---:|---:|---:|---:|\n");
    for span in compare.spans.iter().take(10) {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            span.id,
            fmt_summary_ms(span.before_median_ms),
            fmt_summary_ms(span.after_median_ms),
            span.median_delta_ms
                .map(|value| format!("{:+}ms", value))
                .unwrap_or_else(|| "-".to_string()),
            span.median_delta_percent
                .map(|value| format!("{:+.1}%", value))
                .unwrap_or_else(|| "-".to_string())
        ));
    }
}

fn fmt_summary_ms(value: Option<u64>) -> String {
    value
        .map(|value| format!("{}ms", value))
        .unwrap_or_else(|| "-".to_string())
}
