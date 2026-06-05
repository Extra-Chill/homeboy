use std::collections::{BTreeMap, BTreeSet};

use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::core::Error;

use crate::commands::{escape_markdown_table_cell, CmdResult};

use super::{bench_numeric_metrics, run_contains_scenario, run_summary, RunSummary, RunsOutput};

#[derive(Args, Clone)]
pub struct RunsCompareArgs {
    /// Run kind: bench, rig, trace, etc.
    #[arg(long, default_value = "bench")]
    pub kind: String,
    /// Component ID
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Rig ID
    #[arg(long)]
    pub rig: Option<String>,
    /// Scenario ID for scenario-scoped metrics
    #[arg(long = "scenario")]
    pub scenario_id: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
    /// Metric to include. Repeat to compare multiple metrics.
    #[arg(long = "metric", default_value = "total_elapsed_ms")]
    pub metrics: Vec<String>,
    /// Maximum runs to inspect
    #[arg(long, default_value_t = super::DEFAULT_LIMIT)]
    pub limit: i64,
    /// Output format
    #[arg(long, value_enum, default_value_t = RunsCompareFormat::Table)]
    pub format: RunsCompareFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RunsCompareFormat {
    Table,
    Json,
}

#[derive(Serialize)]
pub struct RunsCompareOutput {
    pub command: &'static str,
    pub kind: String,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub scenario_id: Option<String>,
    pub metrics: Vec<String>,
    pub rows: Vec<RunsCompareRow>,
}

#[derive(Serialize, Clone)]
pub struct RunsCompareRow {
    pub run: RunSummary,
    pub artifact_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub metrics: BTreeMap<String, Option<f64>>,
}

pub fn is_table_mode(args: &RunsCompareArgs) -> bool {
    args.format == RunsCompareFormat::Table
}

pub fn run_markdown(args: RunsCompareArgs) -> CmdResult<String> {
    let (output, exit_code) = compare_runs(args)?;
    match output {
        RunsOutput::Compare(output) => Ok((render_compare_table(&output), exit_code)),
        _ => Err(Error::validation_invalid_argument(
            "output_mode",
            "Only `homeboy runs compare --format=table` supports table output",
            None,
            None,
        )),
    }
}

pub(super) fn compare_runs(args: RunsCompareArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let limit = args.limit.clamp(1, 1000);
    let runs = store.list_runs(RunListFilter {
        kind: Some(args.kind.clone()),
        component_id: args.component_id.clone(),
        status: args.status.clone(),
        rig_id: args.rig.clone(),
        limit: Some(limit),
    })?;

    let mut rows = Vec::new();
    for run in runs {
        if args
            .scenario_id
            .as_deref()
            .is_some_and(|scenario| !run_contains_scenario(&run, scenario))
        {
            continue;
        }

        let artifact_count = store.list_artifacts(&run.id)?.len();
        let scenario_ids = compare_scenarios_for_run(
            &run.metadata_json,
            args.scenario_id.as_deref(),
            &args.metrics,
        );
        for scenario_id in scenario_ids {
            let metrics = args
                .metrics
                .iter()
                .map(|metric| {
                    (
                        metric.clone(),
                        run_metric_value(&run.metadata_json, scenario_id.as_deref(), metric),
                    )
                })
                .collect::<BTreeMap<_, _>>();

            rows.push(RunsCompareRow {
                run: run_summary(run.clone()),
                artifact_count,
                scenario_id,
                metrics,
            });
        }
    }

    Ok((
        RunsOutput::Compare(RunsCompareOutput {
            command: "runs.compare",
            kind: args.kind,
            component_id: args.component_id,
            rig_id: args.rig,
            scenario_id: args.scenario_id,
            metrics: args.metrics,
            rows: rows.into_iter().take(limit as usize).collect(),
        }),
        0,
    ))
}

fn compare_scenarios_for_run(
    metadata: &Value,
    scenario_id: Option<&str>,
    metrics: &[String],
) -> Vec<Option<String>> {
    if let Some(scenario_id) = scenario_id {
        return vec![Some(scenario_id.to_string())];
    }

    let bench_metrics = bench_numeric_metrics(metadata);
    let mut scenario_ids = bench_metrics
        .keys()
        .filter(|(_, metric)| metrics.iter().any(|requested| requested == metric))
        .map(|(scenario, _)| scenario.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();

    if metrics
        .iter()
        .any(|metric| top_level_metric_value(metadata, metric).is_some())
    {
        scenario_ids.insert(0, None);
    }

    if scenario_ids.is_empty() {
        vec![None]
    } else {
        scenario_ids
    }
}

fn run_metric_value(metadata: &Value, scenario_id: Option<&str>, metric: &str) -> Option<f64> {
    if let Some(scenario_id) = scenario_id {
        let key = (scenario_id.to_string(), metric.to_string());
        if let Some(value) = bench_numeric_metrics(metadata).get(&key) {
            return Some(*value);
        }
    }

    top_level_metric_value(metadata, metric)
}

fn top_level_metric_value(metadata: &Value, metric: &str) -> Option<f64> {
    dotted_value(metadata, metric)
        .and_then(Value::as_f64)
        .or_else(|| dotted_value(&metadata["results"], metric).and_then(Value::as_f64))
}

fn dotted_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn render_compare_table(output: &RunsCompareOutput) -> String {
    let mut out = String::new();
    out.push_str("# Runs Compare\n\n");
    out.push_str(&format!("- **Kind:** `{}`\n", output.kind));
    if let Some(component_id) = &output.component_id {
        out.push_str(&format!("- **Component:** `{component_id}`\n"));
    }
    if let Some(rig_id) = &output.rig_id {
        out.push_str(&format!("- **Rig:** `{rig_id}`\n"));
    }
    if let Some(scenario_id) = &output.scenario_id {
        out.push_str(&format!("- **Scenario:** `{scenario_id}`\n"));
    }

    out.push_str("\n| Run | Status | Started | Git SHA | Rig | Artifacts | Scenario |");
    for metric in &output.metrics {
        out.push_str(&format!(" {} |", escape_markdown_table_cell(metric)));
    }
    out.push('\n');
    out.push_str("|---|---|---|---|---|---:|---|");
    for _ in &output.metrics {
        out.push_str("---:|");
    }
    out.push('\n');

    for row in &output.rows {
        let git_sha = row.run.git_sha.as_deref().map(short_sha).unwrap_or("-");
        out.push_str(&format!(
            "| `{}` | `{}` | {} | `{}` | `{}` | {} | `{}` |",
            short_sha(&row.run.id),
            row.run.status,
            row.run.started_at,
            git_sha,
            row.run.rig_id.as_deref().unwrap_or("-"),
            row.artifact_count,
            row.scenario_id.as_deref().unwrap_or("-")
        ));
        for metric in &output.metrics {
            out.push_str(&format!(
                " {} |",
                fmt_metric(row.metrics.get(metric).copied().flatten())
            ));
        }
        out.push('\n');
    }

    out
}

fn short_sha(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn fmt_metric(value: Option<f64>) -> String {
    value
        .map(|value| {
            if value.fract().abs() < f64::EPSILON {
                format!("{value:.0}")
            } else {
                format!("{value:.3}")
            }
        })
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::{NewRunRecord, RunStatus};
    use homeboy::test_support::with_isolated_home;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id(component_id)
            .command(format!("homeboy {kind} {component_id}"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .git_sha(Some("abc123".to_string()))
            .rig_id(rig_id)
            .metadata(metadata)
            .build()
    }

    #[test]
    fn runs_compare_filters_history_and_reports_selected_metrics() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let old = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "studio-bfb",
                    serde_json::json!({
                        "results": { "total_elapsed_ms": 177754.0 },
                        "scenario_metrics": [{
                            "scenario_id": "site-build",
                            "metrics": { "p95_ms": 90.0 }
                        }]
                    }),
                ))
                .expect("old");
            store
                .finish_run(&old.id, RunStatus::Pass, None)
                .expect("finish old");
            let new = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "studio-bfb",
                    serde_json::json!({
                        "results": { "total_elapsed_ms": 213151.0 },
                        "scenario_metrics": [{
                            "scenario_id": "site-build",
                            "metrics": { "p95_ms": 120.0 }
                        }]
                    }),
                ))
                .expect("new");
            store
                .finish_run(&new.id, RunStatus::Fail, None)
                .expect("finish new");
            let other = store
                .start_run(sample_run("trace", "studio", "studio-bfb", Value::Null))
                .expect("trace");
            store
                .finish_run(&other.id, RunStatus::Pass, None)
                .expect("finish trace");

            let (output, _) = compare_runs(RunsCompareArgs {
                kind: "bench".to_string(),
                component_id: Some("studio".to_string()),
                rig: Some("studio-bfb".to_string()),
                scenario_id: Some("site-build".to_string()),
                status: None,
                metrics: vec!["total_elapsed_ms".to_string(), "p95_ms".to_string()],
                limit: 20,
                format: RunsCompareFormat::Json,
            })
            .expect("compare");

            let RunsOutput::Compare(output) = output else {
                panic!("expected compare output");
            };
            assert_eq!(output.rows.len(), 2);
            assert_eq!(output.rows[0].run.id, new.id);
            assert_eq!(output.rows[0].metrics["total_elapsed_ms"], Some(213151.0));
            assert_eq!(output.rows[0].metrics["p95_ms"], Some(120.0));
            assert_eq!(output.rows[1].run.id, old.id);
        });
    }

    #[test]
    fn runs_compare_table_renders_metric_columns() {
        let output = RunsCompareOutput {
            command: "runs.compare",
            kind: "bench".to_string(),
            component_id: Some("studio".to_string()),
            rig_id: Some("studio-bfb".to_string()),
            scenario_id: None,
            metrics: vec!["total_elapsed_ms".to_string()],
            rows: vec![RunsCompareRow {
                run: RunSummary {
                    id: "38f271b9-0000".to_string(),
                    kind: "bench".to_string(),
                    status: "pass".to_string(),
                    started_at: "2026-05-02T00:00:00Z".to_string(),
                    finished_at: None,
                    component_id: Some("studio".to_string()),
                    rig_id: Some("studio-bfb".to_string()),
                    git_sha: Some("abcdef123456".to_string()),
                    command: None,
                    cwd: None,
                    status_note: None,
                    artifact_index: None,
                },
                artifact_count: 3,
                scenario_id: None,
                metrics: BTreeMap::from([("total_elapsed_ms".to_string(), Some(213151.0))]),
            }],
        };

        let table = render_compare_table(&output);
        assert!(table.contains(
            "| Run | Status | Started | Git SHA | Rig | Artifacts | Scenario | total_elapsed_ms |"
        ));
        assert!(table.contains("| `38f271b9` | `pass`"));
        assert!(table.contains("| 213151 |"));
    }
}
