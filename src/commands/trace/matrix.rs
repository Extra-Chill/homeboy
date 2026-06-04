use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::output::{
    compare_trace_aggregates_with_focus, render_matrix_markdown, TraceAggregateInput,
    TraceAggregateSpanInput,
};
use super::{
    apply_command_target_component, execute_trace_run, required_trace_scenario, run_repeat,
    trace_scenario, TraceArgs, TraceVariantMatrixMode,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceMatrixAxis {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TraceMatrixCell {
    pub(super) index: usize,
    pub(super) label: String,
    pub(super) values: BTreeMap<String, String>,
}

pub(super) fn parse_trace_matrix_axis(value: &str) -> Result<TraceMatrixAxis, String> {
    let (name, values) = value
        .split_once('=')
        .ok_or_else(|| "axis must use NAME=value1,value2".to_string())?;
    let name = name.trim();
    if name.is_empty() {
        return Err("axis name must not be empty".to_string());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(
            "axis name may only contain letters, numbers, underscores, and dashes".to_string(),
        );
    }
    let values = values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err("axis must include at least one value".to_string());
    }
    Ok(TraceMatrixAxis {
        name: name.to_string(),
        values,
    })
}

pub(super) fn expand_trace_matrix(axes: &[TraceMatrixAxis]) -> Vec<TraceMatrixCell> {
    let mut partials = vec![BTreeMap::new()];
    for axis in axes {
        let mut next = Vec::new();
        for partial in &partials {
            for value in &axis.values {
                let mut values = partial.clone();
                values.insert(axis.name.clone(), value.clone());
                next.push(values);
            }
        }
        partials = next;
    }
    partials
        .into_iter()
        .enumerate()
        .map(|(index, values)| TraceMatrixCell {
            index,
            label: trace_matrix_cell_label(&values),
            values,
        })
        .collect()
}

fn apply_trace_matrix_cell_to_args(args: &mut TraceArgs, cell: &TraceMatrixCell) {
    args.axes = Vec::new();
    args.output_dir = None;
    args.setting_args.setting_json.extend(
        cell.values
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone()))),
    );
    args.setting_args.setting_json.push((
        "trace_matrix".to_string(),
        serde_json::Value::Object(
            cell.values
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                .collect(),
        ),
    ));
    args.matrix_env.extend(matrix_cell_env(cell));
}

pub(super) fn matrix_cell_env(cell: &TraceMatrixCell) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "HOMEBOY_TRACE_MATRIX_CELL".to_string(),
            cell.index.to_string(),
        ),
        ("HOMEBOY_TRACE_MATRIX_LABEL".to_string(), cell.label.clone()),
        (
            "HOMEBOY_TRACE_MATRIX_JSON".to_string(),
            serde_json::to_string(&cell.values).unwrap_or_else(|_| "{}".to_string()),
        ),
    ];
    env.extend(cell.values.iter().map(|(key, value)| {
        (
            format!("HOMEBOY_TRACE_MATRIX_{}", env_key_suffix(key)),
            value.clone(),
        )
    }));
    env
}

fn trace_matrix_cell_label(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(key, value)| format!("{}-{}", key, value))
        .collect::<Vec<_>>()
        .join("__")
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

fn env_key_suffix(key: &str) -> String {
    key.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn run_scenario_matrix(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    if args.axes.is_empty() {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--axis".to_string(),
        ]));
    }
    if args.repeat != 1 || args.aggregate.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--repeat",
            "trace matrix runs one scenario per cell; use axis values for matrix dimensions instead of repeat/aggregate",
            None,
            None,
        ));
    }

    let component = args.comp.component.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let scenario_id = required_trace_scenario(&args)?;
    let cells = expand_trace_matrix(&args.axes);
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".homeboy").join("experiments").join(format!(
            "{}-matrix-{}",
            scenario_id,
            chrono::Utc::now().format("%Y%m%d%H%M%S")
        ))
    });
    std::fs::create_dir_all(&output_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace matrix output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.matrix.output_dir".to_string()),
        )
    })?;

    let mut outputs = Vec::new();
    let mut failure_count = 0;
    for cell in cells {
        let mut cell_args = args.clone();
        apply_trace_matrix_cell_to_args(&mut cell_args, &cell);
        let cell_dir = output_dir.join(format!("cell-{:03}-{}", cell.index + 1, cell.label));
        std::fs::create_dir_all(&cell_dir).map_err(|err| {
            homeboy::core::Error::internal_io(
                format!(
                    "Failed to create trace matrix cell dir {}: {}",
                    cell_dir.display(),
                    err
                ),
                Some("trace.matrix.cell_dir".to_string()),
            )
        })?;
        let output_path = cell_dir.join("trace.json");
        let (passed, status, exit_code, artifact_path, artifact_dir, failure) =
            match execute_trace_run(cell_args) {
                Ok(execution) => {
                    let passed =
                        execution.workflow.exit_code == 0 && execution.workflow.status == "pass";
                    let stdout_output = extension_trace::from_main_workflow(
                        execution.workflow.clone(),
                        execution.rig_state,
                        false,
                    )
                    .0;
                    write_json_artifact(&output_path, &stdout_output)?;
                    let artifact_path = execution
                        .run_dir
                        .step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS)
                        .to_string_lossy()
                        .to_string();
                    let artifact_dir = execution
                        .run_dir
                        .path()
                        .join("artifacts")
                        .to_string_lossy()
                        .to_string();
                    (
                        passed,
                        execution.workflow.status,
                        execution.workflow.exit_code,
                        artifact_path,
                        artifact_dir,
                        None,
                    )
                }
                Err(err) => {
                    let failure = err.to_string();
                    let failure_output = serde_json::json!({
                        "command": "trace.matrix.cell",
                        "passed": false,
                        "status": "error",
                        "exit_code": 1,
                        "component": component.clone(),
                        "scenario_id": scenario_id.clone(),
                        "cell": cell.values.clone(),
                        "failure": failure,
                    });
                    write_json_artifact(&output_path, &failure_output)?;
                    (
                        false,
                        "error".to_string(),
                        1,
                        String::new(),
                        String::new(),
                        Some(failure),
                    )
                }
            };
        if !passed {
            failure_count += 1;
        }
        outputs.push(extension_trace::TraceScenarioMatrixCellOutput {
            index: cell.index,
            label: cell.label,
            axes: cell.values,
            passed,
            status,
            exit_code,
            artifact_path,
            artifact_dir,
            output_path: output_path.to_string_lossy().to_string(),
            failure,
        });
    }

    let matrix_path = output_dir.join("matrix.json");
    let summary_path = output_dir.join("summary.md");
    let exit_code = if failure_count == 0 { 0 } else { 1 };
    let output = extension_trace::TraceScenarioMatrixOutput {
        command: "trace.matrix",
        passed: failure_count == 0,
        status: if failure_count == 0 { "pass" } else { "fail" }.to_string(),
        component,
        scenario_id,
        output_dir: output_dir.to_string_lossy().to_string(),
        matrix_path: matrix_path.to_string_lossy().to_string(),
        summary_path: summary_path.to_string_lossy().to_string(),
        axes: args
            .axes
            .iter()
            .map(|axis| extension_trace::TraceScenarioMatrixAxisOutput {
                name: axis.name.clone(),
                values: axis.values.clone(),
            })
            .collect(),
        cell_count: outputs.len(),
        failure_count,
        exit_code,
        cells: outputs,
    };
    write_json_artifact(&matrix_path, &output)?;
    std::fs::write(
        &summary_path,
        super::output::render_scenario_matrix_markdown(&output),
    )
    .map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace matrix summary {}: {}",
                summary_path.display(),
                err
            ),
            Some("trace.matrix.summary".to_string()),
        )
    })?;

    Ok((TraceCommandOutput::ScenarioMatrix(output), exit_code))
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
