use std::collections::{BTreeMap, BTreeSet};

use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::core::Error;

use crate::commands::fuzz::{compare_fuzz_runs, FuzzCompareHotspotPolicy};
use crate::commands::{escape_markdown_table_cell, CmdResult};

use super::{bench_numeric_metrics, run_contains_scenario, run_summary, RunSummary, RunsOutput};

/// History compare: selected metrics across the newest runs of a kind.
/// Run-to-run compare (`--from-run` + `--to-run`): one kind's adapter on the
/// shared compare engine diffs two exact runs.
#[derive(Args, Clone)]
pub struct RunsCompareArgs {
    /// Run kind: bench, rig, trace, etc. Run-to-run compare supports bench and fuzz.
    #[arg(long, default_value = "bench")]
    pub kind: String,
    /// Baseline run id. Compares exactly two runs; requires `--to-run`.
    #[arg(long = "from-run", value_name = "RUN_ID", requires = "to_run")]
    pub from_run: Option<String>,
    /// Candidate run id. Compares exactly two runs; requires `--from-run`.
    #[arg(long = "to-run", value_name = "RUN_ID", requires = "from_run")]
    pub to_run: Option<String>,
    /// Fuzz run-to-run compare: how relative hotspot regressions affect the
    /// blocking compare status.
    #[arg(long = "hotspot-policy", value_enum, default_value_t = FuzzCompareHotspotPolicy::Advisory)]
    pub hotspot_policy: FuzzCompareHotspotPolicy,
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
    /// Metric to include. Repeat to compare multiple metrics. History compare
    /// defaults to `total_elapsed_ms`; bench run-to-run compare to every
    /// metric either run recorded.
    #[arg(long = "metric")]
    pub metrics: Vec<String>,
    /// Maximum runs to inspect
    #[arg(long, default_value_t = super::DEFAULT_LIMIT)]
    pub limit: i64,
    /// History output format. Run-to-run compare always emits JSON.
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

pub(crate) fn is_table_mode(args: &RunsCompareArgs) -> bool {
    args.format == RunsCompareFormat::Table && args.from_run.is_none()
}

pub(crate) fn run_markdown(store: &ObservationStore, args: RunsCompareArgs) -> CmdResult<String> {
    let (output, exit_code) = compare_runs(store, args)?;
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

pub(super) fn compare_runs(
    store: &ObservationStore,
    mut args: RunsCompareArgs,
) -> CmdResult<RunsOutput> {
    if let (Some(from_run), Some(to_run)) = (&args.from_run, &args.to_run) {
        return compare_two_runs(store, &args, from_run, to_run);
    }
    if args.metrics.is_empty() {
        args.metrics.push("total_elapsed_ms".to_string());
    }
    let limit = args.limit.clamp(1, 1000);
    let runs = store.list_runs(RunListFilter {
        kind: Some(args.kind.clone()),
        component_id: args.component_id.clone(),
        status: args.status.clone(),
        rig_id: args.rig.clone(),
        limit: Some(limit),
        ..RunListFilter::default()
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

/// Run-to-run compare, keyed by kind to that kind's compare adapter.
fn compare_two_runs(
    store: &ObservationStore,
    args: &RunsCompareArgs,
    from_run: &str,
    to_run: &str,
) -> CmdResult<RunsOutput> {
    match args.kind.as_str() {
        "bench" => super::bench::bench_compare(store, from_run, to_run, &args.metrics),
        "fuzz" => compare_fuzz_runs(store, from_run, to_run, args.hotspot_policy, "runs.compare")
            .map(|output| (RunsOutput::FuzzCompare(output), 0)),
        kind => Err(Error::validation_invalid_argument(
            "kind",
            format!("run-to-run compare is not available for kind '{kind}'"),
            Some(kind.to_string()),
            Some(vec![
                "Run-to-run compare supports `--kind bench` and `--kind fuzz`.".to_string(),
                "Drop --from-run/--to-run to compare metrics across the run history.".to_string(),
            ]),
        )),
    }
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

    /// The observation store the enclosing isolated home installs.
    ///
    /// A test is the entry point for its own unit of work, so opening once here
    /// is a boundary open, not an ambient one inside production code (#7505).
    fn test_store() -> homeboy::core::observation::ObservationStore {
        homeboy::core::observation::ObservationStore::open_initialized().expect("observation store")
    }
    use super::*;
    use homeboy::core::observation::RunStatus;
    use homeboy::test_support::with_isolated_home;

    use crate::commands::runs::test_support::sample_run;

    fn history_args(kind: &str) -> RunsCompareArgs {
        RunsCompareArgs {
            kind: kind.to_string(),
            from_run: None,
            to_run: None,
            hotspot_policy: FuzzCompareHotspotPolicy::Advisory,
            component_id: Some("studio".to_string()),
            rig: Some("studio-bfb".to_string()),
            scenario_id: Some("site-build".to_string()),
            status: None,
            metrics: Vec::new(),
            limit: 20,
            format: RunsCompareFormat::Json,
        }
    }

    fn two_runs(kind: &str, from_run: &str, to_run: &str) -> RunsCompareArgs {
        RunsCompareArgs {
            from_run: Some(from_run.to_string()),
            to_run: Some(to_run.to_string()),
            component_id: None,
            rig: None,
            scenario_id: None,
            ..history_args(kind)
        }
    }

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        compare: RunsCompareArgs,
    }

    #[test]
    fn run_to_run_flags_parse_together_and_skip_table_mode() {
        use clap::Parser;
        let parsed = Cli::try_parse_from([
            "compare",
            "--kind",
            "fuzz",
            "--from-run",
            "a",
            "--to-run",
            "b",
            "--hotspot-policy",
            "blocking",
        ])
        .expect("run-to-run args parse")
        .compare;
        assert_eq!(parsed.from_run.as_deref(), Some("a"));
        assert_eq!(parsed.to_run.as_deref(), Some("b"));
        assert_eq!(parsed.hotspot_policy, FuzzCompareHotspotPolicy::Blocking);
        assert!(!is_table_mode(&parsed));

        assert!(Cli::try_parse_from(["compare", "--from-run", "a"]).is_err());
        assert!(Cli::try_parse_from(["compare", "--to-run", "b"]).is_err());
        assert!(is_table_mode(
            &Cli::try_parse_from(["compare"]).unwrap().compare
        ));
    }

    #[test]
    fn history_compare_defaults_to_total_elapsed_metric() {
        with_isolated_home(|_home| {
            let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
            let (output, _) = compare_runs(&test_store(), history_args("bench")).expect("compare");
            let RunsOutput::Compare(output) = output else {
                panic!("expected history compare output");
            };
            assert_eq!(output.metrics, vec!["total_elapsed_ms".to_string()]);
        });
    }

    #[test]
    fn run_to_run_compare_rejects_kinds_without_an_adapter() {
        with_isolated_home(|_home| {
            let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
            let err = compare_runs(&test_store(), two_runs("trace", "a", "b"))
                .err()
                .expect("trace has no run-to-run adapter");
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("kind 'trace'"));
        });
    }

    fn fuzz_run() -> homeboy::core::observation::NewRunRecord {
        homeboy::core::observation::NewRunRecord::builder("fuzz")
            .component_id("homeboy")
            .command("homeboy fuzz run homeboy")
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .metadata(serde_json::json!({}))
            .build()
    }

    fn fuzz_envelope(id: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "homeboy/fuzz-result-envelope/v1",
            "version": 1,
            "id": id,
            "status": "passed",
            "request": {
                "schema": "homeboy/fuzz-execution-request/v1",
                "version": 1,
                "id": format!("{id}-request"),
                "component": "homeboy"
            }
        })
    }

    /// Moved from the deleted `runs fuzz-compare` subcommand: the same exact
    /// run-id comparison, now reached through `runs compare --kind fuzz`.
    #[test]
    fn fuzz_compare_resolves_envelopes_from_run_artifacts() {
        with_isolated_home(|home| {
            homeboy::core::set_artifact_root_override(Some(home.path().join("artifacts")));
            let store = ObservationStore::open_initialized().expect("store");
            let baseline = store.start_run(fuzz_run()).expect("baseline run");
            store
                .finish_run(&baseline.id, RunStatus::Pass, None)
                .expect("finish baseline");
            let candidate = store.start_run(fuzz_run()).expect("candidate run");
            store
                .finish_run(&candidate.id, RunStatus::Pass, None)
                .expect("finish candidate");

            for (run, id) in [(&baseline, "baseline"), (&candidate, "candidate")] {
                let path = home.path().join(format!("{id}-envelope.json"));
                std::fs::write(&path, fuzz_envelope(id).to_string()).expect("envelope");
                store
                    .record_artifact(&run.id, "fuzz_result_envelope", &path)
                    .expect("envelope artifact");
            }

            let (output, exit_code) =
                compare_runs(&test_store(), two_runs("fuzz", &baseline.id, &candidate.id))
                    .expect("fuzz compare");

            assert_eq!(exit_code, 0);
            let RunsOutput::FuzzCompare(output) = output else {
                panic!("expected fuzz compare output");
            };
            assert_eq!(output.schema, "homeboy/fuzz-compare/v1");
            assert_eq!(output.command, "runs.compare");
            assert_eq!(output.baseline.envelope_id, "baseline");
            assert_eq!(output.candidate.envelope_id, "candidate");
            assert!(output
                .baseline_file
                .starts_with(&format!("homeboy://run/{}/artifact/", baseline.id)));
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn runs_compare_filters_history_and_reports_selected_metrics() {
        with_isolated_home(|_home| {
            let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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

            let (output, _) = compare_runs(
                &test_store(),
                RunsCompareArgs {
                    metrics: vec!["total_elapsed_ms".to_string(), "p95_ms".to_string()],
                    ..history_args("bench")
                },
            )
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
                    sub_runs: Vec::new(),
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
