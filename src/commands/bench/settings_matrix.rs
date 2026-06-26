use std::collections::BTreeMap;

use clap::Args;
use serde::Serialize;

use homeboy::core::agent_tasks::AgentTaskMatrixExecutionState;
use homeboy::core::extension::bench::{BenchCommandOutput, BenchScenario};
use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};

use super::{filter_homeboy_flags, matrix as bench_runner, BenchRunArgs};

#[derive(Args)]
pub(super) struct BenchMatrixArgs {
    #[command(flatten)]
    run: BenchRunArgs,

    /// Settings matrix axis in NAME=value,value form. Repeat the flag or pass
    /// multiple axes after it, e.g. --setting-matrix clients=10,100 rounds=3.
    #[arg(
        long = "setting-matrix",
        value_name = "NAME=VALUE[,VALUE...]",
        num_args = 1..
    )]
    setting_matrix: Vec<String>,
}

impl BenchMatrixArgs {
    pub(super) fn run_args(&self) -> &BenchRunArgs {
        &self.run
    }
}

#[derive(Serialize)]
pub struct BenchSettingsMatrixOutput {
    pub command: &'static str,
    pub component: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rigs: Vec<String>,
    pub axes: Vec<BenchSettingsMatrixAxisOutput>,
    pub cells: Vec<BenchSettingsMatrixCellOutput>,
    pub summary: BenchSettingsMatrixSummary,
    pub follow_ups: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchSettingsMatrixAxisOutput {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Serialize)]
pub struct BenchSettingsMatrixCellOutput {
    pub index: usize,
    pub settings: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub passed: bool,
    pub execution_state: AgentTaskMatrixExecutionState,
    pub status: String,
    pub exit_code: i32,
    pub metrics: Vec<BenchSettingsMatrixMetricSample>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BenchSettingsMatrixMetricSample {
    pub scenario_id: String,
    pub metric: String,
    pub value: f64,
}

#[derive(Serialize)]
pub struct BenchSettingsMatrixSummary {
    pub passed: bool,
    pub execution_state: AgentTaskMatrixExecutionState,
    pub cells: usize,
    pub succeeded: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    pub child_run_ids: Vec<String>,
}

pub(super) fn run_settings_matrix(
    args: &BenchMatrixArgs,
) -> homeboy::core::Result<BenchSettingsMatrixOutput> {
    let run_args = &args.run;
    let raw_axes = &args.setting_matrix;
    if run_args.baseline_args.baseline || run_args.baseline_args.ratchet {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "setting-matrix",
            "bench matrix does not write baselines; run baseline or ratchet on selected cells separately",
            None,
            None,
        ));
    }
    if run_args.rig.len() > 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "rig",
            "bench matrix currently supports zero or one --rig; use cross-rig comparison separately",
            Some(run_args.rig.join(",")),
            None,
        ));
    }

    let axes = resolve_setting_matrix_axes(run_args, raw_axes)?;
    let cells = expand_setting_matrix_cells(&axes);
    let passthrough_args = filter_homeboy_flags(&run_args.args);
    let mut outputs = Vec::with_capacity(cells.len());

    for (index, settings) in cells.into_iter().enumerate() {
        let mut child_args = run_args.clone();
        child_args.status_file = None;
        apply_matrix_settings(&mut child_args, &settings);
        let (output, exit_code) = match child_args.rig.first() {
            Some(rig_id) => {
                bench_runner::run_single_rig(&child_args, &passthrough_args, rig_id.clone())?
            }
            None => bench_runner::run_single(&child_args, &passthrough_args, None)?,
        };
        outputs.push(cell_output(index, settings, output, exit_code));
    }

    let child_run_ids = outputs
        .iter()
        .filter_map(|cell| cell.run_id.clone())
        .collect::<Vec<_>>();
    let succeeded = outputs.iter().filter(|cell| cell.passed).count();
    let failed = outputs.len().saturating_sub(succeeded);
    let execution_state = settings_matrix_execution_state(&outputs);
    let component = run_args
        .comp
        .id()
        .map(str::to_string)
        .unwrap_or_else(|| "<auto>".to_string());
    let parent_run_id = persist_settings_matrix_parent_run(
        run_args,
        &component,
        &axes,
        &outputs,
        &child_run_ids,
        failed == 0,
    );

    Ok(BenchSettingsMatrixOutput {
        command: "bench.matrix",
        component,
        rigs: run_args.rig.clone(),
        axes,
        summary: BenchSettingsMatrixSummary {
            passed: failed == 0,
            execution_state,
            cells: outputs.len(),
            succeeded,
            failed,
            parent_run_id,
            child_run_ids,
        },
        cells: outputs,
        follow_ups: vec![
            "Add typed JSON setting matrix axes when a benchmark needs non-string settings."
                .to_string(),
            "Add cross-rig matrix aggregation after the single-rig surface has real users."
                .to_string(),
        ],
    })
}

fn cell_output(
    index: usize,
    settings: BTreeMap<String, String>,
    output: BenchCommandOutput,
    exit_code: i32,
) -> BenchSettingsMatrixCellOutput {
    let hints = output.hints.clone().unwrap_or_default();
    let metrics = collect_metric_samples(&output);
    BenchSettingsMatrixCellOutput {
        index,
        settings,
        run_id: extract_run_id(&hints),
        passed: output.passed,
        execution_state: bench_cell_execution_state(&output, exit_code),
        status: output.status,
        exit_code,
        metrics,
        hints,
    }
}

fn bench_cell_execution_state(
    output: &BenchCommandOutput,
    exit_code: i32,
) -> AgentTaskMatrixExecutionState {
    if output.failure.is_some()
        || (exit_code != 0 && output.budget_findings.is_empty() && output.gate_failures.is_empty())
    {
        return AgentTaskMatrixExecutionState::ExecutionFailed;
    }

    if !output.passed || !output.budget_findings.is_empty() || !output.gate_failures.is_empty() {
        return AgentTaskMatrixExecutionState::ExecutedWithFindings;
    }

    AgentTaskMatrixExecutionState::ExecutedClean
}

fn settings_matrix_execution_state(
    cells: &[BenchSettingsMatrixCellOutput],
) -> AgentTaskMatrixExecutionState {
    let mut saw_clean = false;
    let mut saw_not_run = false;
    for cell in cells {
        match cell.execution_state {
            AgentTaskMatrixExecutionState::ExecutionFailed => {
                return AgentTaskMatrixExecutionState::ExecutionFailed;
            }
            AgentTaskMatrixExecutionState::ExecutedWithFindings => {
                return AgentTaskMatrixExecutionState::ExecutedWithFindings;
            }
            AgentTaskMatrixExecutionState::ExecutedClean => saw_clean = true,
            AgentTaskMatrixExecutionState::NotRun => saw_not_run = true,
        }
    }

    match (saw_clean, saw_not_run) {
        (true, false) => AgentTaskMatrixExecutionState::ExecutedClean,
        (false, true) => AgentTaskMatrixExecutionState::NotRun,
        (true, true) => AgentTaskMatrixExecutionState::NotRun,
        (false, false) => AgentTaskMatrixExecutionState::NotRun,
    }
}

fn persist_settings_matrix_parent_run(
    run_args: &BenchRunArgs,
    component: &str,
    axes: &[BenchSettingsMatrixAxisOutput],
    cells: &[BenchSettingsMatrixCellOutput],
    child_run_ids: &[String],
    passed: bool,
) -> Option<String> {
    try_persist_settings_matrix_parent_run(run_args, component, axes, cells, child_run_ids, passed)
        .ok()
}

fn try_persist_settings_matrix_parent_run(
    run_args: &BenchRunArgs,
    component: &str,
    axes: &[BenchSettingsMatrixAxisOutput],
    cells: &[BenchSettingsMatrixCellOutput],
    child_run_ids: &[String],
    passed: bool,
) -> homeboy::core::Result<String> {
    let store = ObservationStore::open_initialized()?;
    let cwd = std::env::current_dir().ok();
    let run = store.start_run(
        NewRunRecord::builder("bench.matrix")
            .component_id(component)
            .command(settings_matrix_parent_command(component, run_args))
            .optional_cwd_path(cwd.as_deref())
            .current_homeboy_version()
            .optional_rig_id(run_args.rig.first().map(String::as_str))
            .metadata(settings_matrix_parent_metadata(
                axes,
                cells,
                child_run_ids,
                passed,
            ))
            .build(),
    )?;
    store.finish_run(
        &run.id,
        if passed {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        },
        None,
    )?;
    Ok(run.id)
}

fn settings_matrix_parent_command(component: &str, run_args: &BenchRunArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "matrix".to_string(),
    ];
    if component != "<auto>" {
        parts.push(component.to_string());
    }
    for rig in &run_args.rig {
        parts.push("--rig".to_string());
        parts.push(rig.clone());
    }
    parts.join(" ")
}

fn settings_matrix_parent_metadata(
    axes: &[BenchSettingsMatrixAxisOutput],
    cells: &[BenchSettingsMatrixCellOutput],
    child_run_ids: &[String],
    passed: bool,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/bench-settings-matrix-run/v1",
        "command": "bench.matrix",
        "passed": passed,
        "execution_state": settings_matrix_execution_state(cells),
        "axes": axes,
        "cells": cells,
        "child_run_ids": child_run_ids,
        "inspect": {
            "show_children": child_run_ids
                .iter()
                .map(|run_id| format!("homeboy runs show {run_id}"))
                .collect::<Vec<_>>(),
            "compare_children": if child_run_ids.len() >= 2 {
                Some(format!(
                    "homeboy bench compare --from-run {} --to-run {}",
                    child_run_ids[0], child_run_ids[1]
                ))
            } else {
                None
            },
        },
    })
}

fn parse_setting_matrix_axes(
    raw_axes: &[String],
) -> homeboy::core::Result<Vec<BenchSettingsMatrixAxisOutput>> {
    raw_axes
        .iter()
        .map(|raw| {
            let (name, raw_values) = raw.split_once('=').ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "setting-matrix",
                    format!("setting matrix axis must be NAME=value,value; got '{raw}'"),
                    Some(raw.clone()),
                    None,
                )
            })?;
            let values = raw_values
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if name.is_empty() || values.is_empty() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "setting-matrix",
                    format!("setting matrix axis must include a name and at least one value; got '{raw}'"),
                    Some(raw.clone()),
                    None,
                ));
            }
            Ok(BenchSettingsMatrixAxisOutput {
                name: name.to_string(),
                values,
            })
        })
        .collect()
}

fn resolve_setting_matrix_axes(
    run_args: &BenchRunArgs,
    raw_axes: &[String],
) -> homeboy::core::Result<Vec<BenchSettingsMatrixAxisOutput>> {
    if !raw_axes.is_empty() {
        return parse_setting_matrix_axes(raw_axes);
    }

    if run_args.rig.len() == 1 && run_args.profile.is_some() {
        return Ok(Vec::new());
    }

    let single_cell_command = match (run_args.rig.first(), run_args.profile.as_deref()) {
        (Some(rig), Some(profile)) => format!("homeboy bench --rig {rig} --profile {profile}"),
        (Some(rig), None) => format!("homeboy bench --rig {rig} --profile <profile>"),
        (None, Some(profile)) => format!("homeboy bench --rig <id> --profile {profile}"),
        (None, None) => "homeboy bench --rig <id> --profile <profile>".to_string(),
    };

    Err(homeboy::core::Error::validation_invalid_argument(
        "setting-matrix",
        format!(
            "provide --setting-matrix NAME=value,value for multi-cell matrix runs, or use `{single_cell_command}` for single-cell runs"
        ),
        None,
        None,
    ))
}

fn expand_setting_matrix_cells(
    axes: &[BenchSettingsMatrixAxisOutput],
) -> Vec<BTreeMap<String, String>> {
    let mut cells = vec![BTreeMap::new()];
    for axis in axes {
        let mut next = Vec::with_capacity(cells.len() * axis.values.len());
        for cell in &cells {
            for value in &axis.values {
                let mut expanded = cell.clone();
                expanded.insert(axis.name.clone(), value.clone());
                next.push(expanded);
            }
        }
        cells = next;
    }
    cells
}

fn apply_matrix_settings(child_args: &mut BenchRunArgs, settings: &BTreeMap<String, String>) {
    for (name, value) in settings {
        child_args
            .setting_args
            .setting
            .push((name.clone(), value.clone()));
    }
}

fn extract_run_id(hints: &[String]) -> Option<String> {
    hints
        .iter()
        .find_map(|hint| hint.strip_prefix("Persisted benchmark run ID: "))
        .map(str::to_string)
}

fn collect_metric_samples(output: &BenchCommandOutput) -> Vec<BenchSettingsMatrixMetricSample> {
    output
        .results
        .as_ref()
        .map(|results| {
            results
                .scenarios
                .iter()
                .flat_map(collect_scenario_metric_samples)
                .collect()
        })
        .unwrap_or_default()
}

fn collect_scenario_metric_samples(
    scenario: &BenchScenario,
) -> Vec<BenchSettingsMatrixMetricSample> {
    let mut samples = Vec::new();
    for (metric, value) in &scenario.metrics.values {
        samples.push(BenchSettingsMatrixMetricSample {
            scenario_id: scenario.id.clone(),
            metric: metric.clone(),
            value: *value,
        });
    }
    for (group, metrics) in &scenario.metric_groups {
        for (metric, value) in metrics {
            samples.push(BenchSettingsMatrixMetricSample {
                scenario_id: scenario.id.clone(),
                metric: format!("{group}.{metric}"),
                value: *value,
            });
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::bench::BenchArgs;
    use crate::test_support::with_isolated_home;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        bench: BenchArgs,
    }

    #[test]
    fn bench_matrix_command_accepts_multiple_setting_axes() {
        TestCli::try_parse_from([
            "homeboy",
            "matrix",
            "--rig",
            "gutenberg-rtc",
            "--scenario",
            "gutenberg-rtc-protocol-load",
            "--setting-matrix",
            "clients=10,100",
            "rounds=3",
            "batch_size=1,10",
            "--runs",
            "3",
        ])
        .expect("bench matrix CLI should parse");
    }

    #[test]
    fn resolves_single_cell_matrix_without_setting_axes_for_rig_profile() {
        let args = MatrixCli::parse_from([
            "homeboy",
            "--rig",
            "studio",
            "--profile",
            "agentic",
            "--iterations",
            "1",
        ])
        .bench
        .run
        .clone();

        let axes = resolve_setting_matrix_axes(&args, &[]).expect("single-cell matrix is valid");
        let cells = expand_setting_matrix_cells(&axes);

        assert!(axes.is_empty());
        assert_eq!(cells, vec![BTreeMap::new()]);
    }

    #[test]
    fn missing_setting_axes_points_single_cell_users_to_plain_bench() {
        let args = MatrixCli::parse_from(["homeboy", "--rig", "studio", "--iterations", "1"])
            .bench
            .run
            .clone();

        let err = resolve_setting_matrix_axes(&args, &[])
            .expect_err("matrix without axes and profile should explain remediation");
        let message = err.to_string();

        assert!(message.contains("--setting-matrix"), "got: {message}");
        assert!(
            message.contains("homeboy bench --rig studio --profile <profile>"),
            "got: {message}"
        );
    }

    #[test]
    fn parses_setting_matrix_axes() {
        let axes =
            parse_setting_matrix_axes(&["clients=10,100".to_string(), "rounds=3".to_string()])
                .expect("axes should parse");

        assert_eq!(
            axes,
            vec![
                BenchSettingsMatrixAxisOutput {
                    name: "clients".to_string(),
                    values: vec!["10".to_string(), "100".to_string()],
                },
                BenchSettingsMatrixAxisOutput {
                    name: "rounds".to_string(),
                    values: vec!["3".to_string()],
                },
            ]
        );
    }

    #[test]
    fn expands_cartesian_setting_cells() {
        let axes = parse_setting_matrix_axes(&[
            "clients=10,100".to_string(),
            "batch_size=1,25".to_string(),
        ])
        .expect("axes should parse");

        let cells = expand_setting_matrix_cells(&axes);

        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0]["clients"], "10");
        assert_eq!(cells[0]["batch_size"], "1");
        assert_eq!(cells[3]["clients"], "100");
        assert_eq!(cells[3]["batch_size"], "25");
    }

    #[test]
    fn applies_dotted_setting_axes_as_string_settings() {
        #[derive(Parser)]
        struct MatrixCli {
            #[command(flatten)]
            bench: BenchMatrixArgs,
        }

        let mut args = MatrixCli::parse_from([
            "homeboy",
            "--setting-matrix",
            "clients=10",
            "--iterations",
            "1",
        ])
        .bench
        .run
        .clone();
        let mut settings = BTreeMap::new();
        settings.insert(
            "bench_env.GUTENBERG_RTC_CLIENTS".to_string(),
            "100".to_string(),
        );
        settings.insert(
            "bench_env.GUTENBERG_RTC_BATCH_SIZE".to_string(),
            "25".to_string(),
        );
        settings.insert(
            "sample_runtime_bin".to_string(),
            "/tmp/sample-runtime".to_string(),
        );

        apply_matrix_settings(&mut args, &settings);

        assert_eq!(
            args.setting_args.setting,
            vec![
                (
                    "bench_env.GUTENBERG_RTC_BATCH_SIZE".to_string(),
                    "25".to_string()
                ),
                (
                    "bench_env.GUTENBERG_RTC_CLIENTS".to_string(),
                    "100".to_string()
                ),
                (
                    "sample_runtime_bin".to_string(),
                    "/tmp/sample-runtime".to_string()
                )
            ]
        );
        assert!(args.setting_args.setting_json.is_empty());
    }

    #[test]
    fn extracts_child_run_id_from_hints() {
        let run_id = extract_run_id(&[
            "Persisted benchmark run ID: bench-123".to_string(),
            "View this run: homeboy runs show bench-123".to_string(),
        ]);

        assert_eq!(run_id.as_deref(), Some("bench-123"));
    }

    #[test]
    fn matrix_parent_metadata_records_child_run_contract() {
        let axes = vec![BenchSettingsMatrixAxisOutput {
            name: "clients".to_string(),
            values: vec!["10".to_string(), "100".to_string()],
        }];
        let cells = vec![matrix_cell(0, "clients", "10", Some("run-a"), true)];
        let child_run_ids = vec!["run-a".to_string(), "run-b".to_string()];

        let metadata = settings_matrix_parent_metadata(&axes, &cells, &child_run_ids, true);

        assert_eq!(metadata["schema"], "homeboy/bench-settings-matrix-run/v1");
        assert_eq!(metadata["command"], "bench.matrix");
        assert_eq!(metadata["passed"], true);
        assert_eq!(metadata["execution_state"], "executed_clean");
        assert_eq!(metadata["axes"][0]["name"], "clients");
        assert_eq!(metadata["cells"][0]["run_id"], "run-a");
        assert_eq!(metadata["cells"][0]["execution_state"], "executed_clean");
        assert_eq!(metadata["child_run_ids"][1], "run-b");
        assert_eq!(
            metadata["inspect"]["show_children"][0],
            "homeboy runs show run-a"
        );
        assert_eq!(
            metadata["inspect"]["compare_children"],
            "homeboy bench compare --from-run run-a --to-run run-b"
        );
    }

    #[test]
    fn persists_settings_matrix_parent_run_as_finished_matrix_record() {
        with_isolated_home(|_home| {
            let args = MatrixCli::parse_from([
                "homeboy",
                "studio-web",
                "--setting-matrix",
                "clients=10,100",
                "--rig",
                "studio",
                "--iterations",
                "1",
            ])
            .bench
            .run
            .clone();
            let axes = parse_setting_matrix_axes(&["clients=10,100".to_string()])
                .expect("axes should parse");
            let cells = vec![
                matrix_cell(0, "clients", "10", Some("run-a"), true),
                matrix_cell(1, "clients", "100", Some("run-b"), false),
            ];
            let child_run_ids = vec!["run-a".to_string(), "run-b".to_string()];

            let parent_run_id = try_persist_settings_matrix_parent_run(
                &args,
                "studio-web",
                &axes,
                &cells,
                &child_run_ids,
                false,
            )
            .expect("parent run should persist");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&parent_run_id)
                .expect("read parent")
                .expect("parent exists");
            assert_eq!(run.kind, "bench.matrix");
            assert_eq!(run.status, "fail");
            assert_eq!(run.component_id.as_deref(), Some("studio-web"));
            assert_eq!(run.rig_id.as_deref(), Some("studio"));
            assert_eq!(run.metadata_json["child_run_ids"][0], "run-a");
            assert_eq!(run.metadata_json["cells"][1]["status"], "fail");
            assert_eq!(
                run.metadata_json["cells"][1]["execution_state"],
                "executed_with_findings"
            );
        });
    }

    #[derive(Parser)]
    struct MatrixCli {
        #[command(flatten)]
        bench: BenchMatrixArgs,
    }

    fn matrix_cell(
        index: usize,
        name: &str,
        value: &str,
        run_id: Option<&str>,
        passed: bool,
    ) -> BenchSettingsMatrixCellOutput {
        let mut settings = BTreeMap::new();
        settings.insert(name.to_string(), value.to_string());
        BenchSettingsMatrixCellOutput {
            index,
            settings,
            run_id: run_id.map(str::to_string),
            passed,
            execution_state: if passed {
                AgentTaskMatrixExecutionState::ExecutedClean
            } else {
                AgentTaskMatrixExecutionState::ExecutedWithFindings
            },
            status: if passed { "pass" } else { "fail" }.to_string(),
            exit_code: if passed { 0 } else { 1 },
            metrics: Vec::new(),
            hints: Vec::new(),
        }
    }
}
