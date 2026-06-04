use std::path::{Path, PathBuf};

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::output::{
    compare_trace_aggregates_with_focus, render_matrix_markdown, TraceAggregateInput,
    TraceAggregateSpanInput,
};
use super::{
    apply_command_target_component, run_repeat, trace_scenario, TraceArgs, TraceVariantMatrixMode,
};
use crate::commands::CmdResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TraceVariantStackItem {
    pub(super) label: String,
    pub(super) overlay: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TraceVariantCombination {
    pub(super) label: String,
    pub(super) items: Vec<TraceVariantStackItem>,
}

pub(super) fn run_variant_matrix(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    if args.keep_overlay {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--keep-overlay",
            "trace compare-variant reuses the same component checkout across runs, so overlays must be reverted after each combination",
            None,
            None,
        ));
    }

    let scenario_id = trace_scenario(&args)?.to_string();
    let stack = variant_stack_items(&args)?;
    let combinations = expand_variant_matrix(&stack, args.matrix);
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".homeboy").join("experiments").join(format!(
            "{}-{}",
            scenario_id,
            chrono::Utc::now().format("%Y%m%d%H%M%S")
        ))
    });
    std::fs::create_dir_all(&output_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace variant output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.variant.output_dir".to_string()),
        )
    })?;

    let baseline = run_variant_aggregate(&args, Vec::new())?;
    let baseline_path = output_dir.join("baseline.aggregate.json");
    write_json_artifact(&baseline_path, &baseline)?;

    let mut runs = Vec::new();
    let mut failure_count = usize::from(!baseline.passed);
    for combination in combinations {
        let overlays = combination
            .items
            .iter()
            .map(|item| item.overlay.clone())
            .collect::<Vec<_>>();
        let aggregate = run_variant_aggregate(&args, overlays.clone())?;
        let slug = variant_combination_slug(&combination.label);
        let aggregate_path = output_dir.join(format!("{}.aggregate.json", slug));
        let compare_path = output_dir.join(format!("{}.compare.json", slug));
        write_json_artifact(&aggregate_path, &aggregate)?;
        let compare = compare_trace_aggregates_with_focus(
            &baseline_path,
            aggregate_to_compare_input(&baseline),
            &aggregate_path,
            aggregate_to_compare_input(&aggregate),
            &args.focus_spans,
            args.regression_threshold,
            args.regression_min_delta_ms,
        );
        write_json_artifact(&compare_path, &compare)?;
        if !aggregate.passed
            || compare.focus_status.as_deref() == Some("fail")
            || compare.guardrail_status.as_deref() == Some("fail")
        {
            failure_count += 1;
        }
        runs.push(extension_trace::TraceVariantMatrixRunOutput {
            label: combination.label,
            variants: combination
                .items
                .into_iter()
                .map(|item| item.label)
                .collect(),
            overlays,
            aggregate_path: aggregate_path.to_string_lossy().to_string(),
            compare_path: compare_path.to_string_lossy().to_string(),
            passed: aggregate.passed,
            status: aggregate.status,
            exit_code: aggregate.exit_code,
            span_count: compare.span_count,
        });
    }

    let summary_path = output_dir.join("summary.md");
    let exit_code = if failure_count == 0 { 0 } else { 1 };
    let output = extension_trace::TraceVariantMatrixOutput {
        command: "trace.variant_matrix",
        passed: failure_count == 0,
        status: if failure_count == 0 { "pass" } else { "fail" }.to_string(),
        component: baseline.component.clone(),
        scenario_id,
        matrix: args.matrix.as_str().to_string(),
        output_dir: output_dir.to_string_lossy().to_string(),
        baseline_path: baseline_path.to_string_lossy().to_string(),
        summary_path: summary_path.to_string_lossy().to_string(),
        run_count: runs.len() + 1,
        failure_count,
        exit_code,
        runs,
    };
    std::fs::write(&summary_path, render_matrix_markdown(&output)).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace variant summary {}: {}",
                summary_path.display(),
                err
            ),
            Some("trace.variant.summary".to_string()),
        )
    })?;

    Ok((TraceCommandOutput::Matrix(output), exit_code))
}

fn run_variant_aggregate(
    args: &TraceArgs,
    overlays: Vec<String>,
) -> homeboy::core::Result<extension_trace::TraceAggregateOutput> {
    let mut run_args = args.clone();
    apply_command_target_component(&mut run_args);
    run_args.repeat = args.repeat.max(1);
    run_args.aggregate = Some("spans".to_string());
    run_args.overlays = overlays;
    run_args.variants = Vec::new();
    run_args.output_dir = None;
    match run_repeat(run_args)?.0 {
        TraceCommandOutput::Aggregate(output) => Ok(output),
        _ => unreachable!("run_repeat returns aggregate output"),
    }
}

fn variant_stack_items(args: &TraceArgs) -> homeboy::core::Result<Vec<TraceVariantStackItem>> {
    if !args.variants.is_empty() && !args.overlays.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--variant",
            "mixing --variant and --overlay would make stack order ambiguous; use one ordered stack source",
            None,
            None,
        ));
    }
    let values = if !args.variants.is_empty() {
        &args.variants
    } else {
        &args.overlays
    };
    if values.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--overlay",
            "trace compare-variant requires at least one --overlay or --variant",
            None,
            None,
        ));
    }
    Ok(values
        .iter()
        .map(|value| TraceVariantStackItem {
            label: variant_label(value),
            overlay: value.clone(),
        })
        .collect())
}

pub(super) fn expand_variant_matrix(
    stack: &[TraceVariantStackItem],
    mode: TraceVariantMatrixMode,
) -> Vec<TraceVariantCombination> {
    match mode {
        TraceVariantMatrixMode::None => vec![TraceVariantCombination {
            label: variant_combination_label(stack),
            items: stack.to_vec(),
        }],
        TraceVariantMatrixMode::Single => stack
            .iter()
            .map(|item| TraceVariantCombination {
                label: item.label.clone(),
                items: vec![item.clone()],
            })
            .collect(),
        TraceVariantMatrixMode::Cumulative => (1..=stack.len())
            .map(|len| {
                let items = stack[..len].to_vec();
                TraceVariantCombination {
                    label: variant_combination_label(&items),
                    items,
                }
            })
            .collect(),
    }
}

fn variant_combination_label(stack: &[TraceVariantStackItem]) -> String {
    stack
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

fn variant_label(value: &str) -> String {
    Path::new(value)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(value)
        .to_string()
}

fn variant_combination_slug(label: &str) -> String {
    label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub(super) fn aggregate_to_compare_input(
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
            .and_then(|state| serde_json::to_value(state).ok()),
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: aggregate
            .spans
            .iter()
            .map(|span| TraceAggregateSpanInput {
                id: span.id.clone(),
                n: span.n,
                median_ms: span.median_ms,
                avg_ms: span.avg_ms,
                max_ms: span.max_ms,
                max_run_index: span.max_run_index,
                max_artifact_path: span.max_artifact_path.clone(),
                failures: span.failures,
                metadata: span.metadata.clone(),
            })
            .collect(),
        guardrails: aggregate.guardrails.clone(),
        guardrail_failure_count: aggregate.guardrail_failure_count,
    }
}

pub(super) fn write_json_artifact<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> homeboy::core::Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|err| {
        homeboy::core::Error::internal_json(err.to_string(), Some("trace.variant.json".to_string()))
    })?;
    std::fs::write(path, content).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!("Failed to write trace artifact {}: {}", path.display(), err),
            Some("trace.variant.write".to_string()),
        )
    })
}
