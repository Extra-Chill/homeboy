use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::aggregate::{aggregate_span, TraceAggregateSpanSample};
use super::{
    attach_span_metadata, classification_summaries, cli_span_definitions_for_args,
    execute_trace_run, plan_trace_run_order, resolved_profile_output_for_args,
    run_trace_guardrails_for_args, trace_scenario, TraceArgs,
};
use crate::commands::CmdResult;

pub(super) fn run_repeat(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    let repeat = args.repeat;
    let scenario_id = trace_scenario(&args)?.to_string();
    let mut runs = Vec::new();
    let mut overlays = Vec::new();
    let mut span_samples: BTreeMap<String, Vec<TraceAggregateSpanSample>> = BTreeMap::new();
    let mut span_failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_span_ids: BTreeSet<String> = cli_span_definitions_for_args(&args)?
        .into_iter()
        .map(|span| span.id)
        .collect();
    let mut rig_state = None;
    let mut component = None;
    let span_metadata = super::trace_span_metadata_for_args(&args)?;
    let mut failure_count = 0;

    let run_order = plan_trace_run_order(repeat, args.schedule, &["run"]);

    for plan_entry in &run_order {
        let index = plan_entry.index();
        let mut run_args = args.clone();
        run_args.repeat = 1;
        match execute_trace_run(run_args) {
            Ok(execution) => {
                if rig_state.is_none() {
                    rig_state = execution.rig_state.clone();
                }
                if component.is_none() {
                    component = Some(execution.workflow.component.clone());
                }
                if overlays.is_empty() && !execution.workflow.overlays.is_empty() {
                    overlays = execution.workflow.overlays.clone();
                }
                let passed =
                    execution.workflow.exit_code == 0 && execution.workflow.status == "pass";
                if !passed {
                    failure_count += 1;
                }
                let artifact_path = execution
                    .run_dir
                    .step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS)
                    .to_string_lossy()
                    .to_string();
                let mut seen_span_ids = BTreeSet::new();
                if let Some(results) = execution.workflow.results.as_ref() {
                    for span in &results.span_results {
                        all_span_ids.insert(span.id.clone());
                        seen_span_ids.insert(span.id.clone());
                        if span.status == extension_trace::parsing::TraceSpanStatus::Ok {
                            if let Some(duration) = span.duration_ms {
                                span_samples.entry(span.id.clone()).or_default().push(
                                    TraceAggregateSpanSample {
                                        duration_ms: duration,
                                        run_index: index,
                                        artifact_path: artifact_path.clone(),
                                    },
                                );
                                continue;
                            }
                        }
                        *span_failures.entry(span.id.clone()).or_default() += 1;
                    }
                    for span_id in all_span_ids.difference(&seen_span_ids) {
                        *span_failures.entry(span_id.clone()).or_default() += 1;
                    }
                } else {
                    for span_id in &all_span_ids {
                        *span_failures.entry(span_id.clone()).or_default() += 1;
                    }
                }
                runs.push(extension_trace::TraceAggregateRunOutput {
                    index,
                    passed,
                    status: execution.workflow.status,
                    exit_code: execution.workflow.exit_code,
                    artifact_path,
                    scenario_id: execution
                        .workflow
                        .results
                        .as_ref()
                        .map(|r| r.scenario_id.clone()),
                    summary: execution
                        .workflow
                        .results
                        .as_ref()
                        .and_then(|r| r.summary.clone()),
                    failure: execution
                        .workflow
                        .failure
                        .as_ref()
                        .map(|failure| failure.stderr_excerpt.clone())
                        .or_else(|| {
                            execution
                                .workflow
                                .results
                                .as_ref()
                                .and_then(|r| r.failure.clone())
                        }),
                });
            }
            Err(error) => {
                failure_count += 1;
                for span_id in &all_span_ids {
                    *span_failures.entry(span_id.clone()).or_default() += 1;
                }
                runs.push(extension_trace::TraceAggregateRunOutput {
                    index,
                    passed: false,
                    status: "error".to_string(),
                    exit_code: 1,
                    artifact_path: String::new(),
                    scenario_id: Some(scenario_id.clone()),
                    summary: None,
                    failure: Some(error.message),
                });
            }
        }
    }

    let mut spans = all_span_ids
        .into_iter()
        .map(|id| {
            let samples = span_samples.remove(&id).unwrap_or_default();
            let failures = span_failures.remove(&id).unwrap_or(0);
            aggregate_span(id, samples, failures)
        })
        .collect::<Vec<_>>();
    let unmatched_span_metadata_ids = attach_span_metadata(&mut spans, &span_metadata);
    let classification_summaries = classification_summaries(&spans);
    let guardrails = run_trace_guardrails_for_args(&args)?;
    let guardrail_failure_count = guardrails
        .iter()
        .filter(|guardrail| !guardrail.passed)
        .count();
    let focus_spans = focus_aggregate_spans(&spans, &args.focus_spans);
    let exit_code = if failure_count == 0 && guardrail_failure_count == 0 {
        0
    } else {
        1
    };
    let output = extension_trace::TraceAggregateOutput {
        command: "trace.aggregate.spans",
        passed: exit_code == 0,
        status: if exit_code == 0 { "pass" } else { "fail" }.to_string(),
        component: component.unwrap_or_else(|| args.comp.component.clone().unwrap_or_default()),
        scenario_id,
        phase_preset: args.phase_preset.clone(),
        repeat,
        run_count: runs.len(),
        failure_count,
        exit_code,
        schedule: Some(args.schedule.as_str().to_string()),
        run_order: run_order
            .into_iter()
            .map(|entry| extension_trace::TraceRunOrderEntryOutput {
                index: entry.index(),
                group: entry.group().to_string(),
                iteration: entry.iteration(),
            })
            .collect(),
        rig_state,
        overlays,
        runs,
        spans,
        guardrails,
        guardrail_failure_count,
        focus_span_ids: args.focus_spans.clone(),
        focus_spans,
        classification_summaries,
        unmatched_span_metadata_ids,
        profile: resolved_profile_output_for_args(&args),
    };

    Ok((TraceCommandOutput::Aggregate(output), exit_code))
}

pub(super) fn focus_aggregate_spans(
    spans: &[extension_trace::TraceAggregateSpanOutput],
    focus_span_ids: &[String],
) -> Vec<extension_trace::TraceAggregateSpanOutput> {
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
