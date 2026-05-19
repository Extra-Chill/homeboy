mod bundle;
mod common;
mod compare;
#[cfg(test)]
mod corpus_tests;
mod distribution;
mod drift;
mod findings;
mod gh_actions;
mod latest;
mod query;
mod reconcile;
mod remote;
mod remote_artifact;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunListFilter, RunRecord};
use homeboy::core::Error;

use super::{CmdResult, GlobalArgs};
use bundle::{
    export_runs, import_runs, RunsExportArgs, RunsExportOutput, RunsImportArgs, RunsImportOutput,
};
pub use common::RunSummary;
use compare::{compare_runs, RunsCompareArgs, RunsCompareOutput};
pub use distribution::{runs_distribution, RunsDistributionArgs, RunsDistributionOutput};
use drift::{runs_drift, RunsDriftArgs, RunsDriftOutput};
use findings::{RunsFindingOutput, RunsFindingsOutput};
use gh_actions::GhActionsImportOutput;
use latest::{RunsLatestFindingOutput, RunsLatestRunArgs, RunsLatestRunOutput};
use query::{runs_query, RunsQueryArgs, RunsQueryOutput};
use reconcile::{reconcile_runs, RunsReconcileArgs, RunsReconcileOutput};

const DEFAULT_LIMIT: i64 = 20;

#[derive(Args, Clone)]
pub struct RunsArgs {
    #[command(subcommand)]
    command: RunsCommand,
}

#[derive(Subcommand, Clone)]
enum RunsCommand {
    /// List persisted observation runs
    List(RunsListArgs),
    /// Aggregate categorical values from persisted run metadata
    Distribution(RunsDistributionArgs),
    /// Show the latest persisted observation run matching filters
    LatestRun(RunsLatestRunArgs),
    /// Compare selected metrics across persisted run history
    Compare(RunsCompareArgs),
    /// Mark orphaned running observation records stale
    Reconcile(RunsReconcileArgs),
    /// Show one persisted observation run
    Show { run_id: String },
    /// List artifacts recorded for one run
    Artifacts { run_id: String },
    /// Retrieve or sync recorded run artifacts
    Artifact(RunsArtifactArgs),
    /// List findings recorded for one run
    Findings(findings::RunsFindingsArgs),
    /// Show one recorded finding
    Finding { finding_id: String },
    /// Show the latest finding from the latest run matching filters
    LatestFinding(findings::RunsLatestFindingArgs),
    /// Export observation records as an inspectable directory bundle
    Export(RunsExportArgs),
    /// Import an observation bundle (default) or ingest GitHub Actions artifacts
    /// (`--from-gh-actions`).
    Import(RunsImportArgs),
    /// Project JSONPath expressions over imported run artifact rows.
    Query(RunsQueryArgs),
    /// Window-based distribution drift over a JSONPath metric.
    Drift(RunsDriftArgs),
}

#[derive(Args, Clone, Default)]
pub struct RunsListArgs {
    /// Query runs from a connected execution runner daemon
    #[arg(long)]
    pub runner: Option<String>,
    /// Run kind: bench, rig, trace, etc.
    #[arg(long)]
    pub kind: Option<String>,
    /// Component ID
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Rig ID
    #[arg(long)]
    pub rig: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
    /// Maximum runs to return
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: i64,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RunsOutput {
    List(RunsListOutput),
    Distribution(RunsDistributionOutput),
    LatestRun(RunsLatestRunOutput),
    Compare(RunsCompareOutput),
    Show(RunsShowOutput),
    Artifacts(RunsArtifactsOutput),
    ArtifactGet(RunsArtifactGetOutput),
    ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput),
    Findings(RunsFindingsOutput),
    Finding(RunsFindingOutput),
    LatestFinding(RunsLatestFindingOutput),
    BenchHistory(BenchHistoryOutput),
    BenchCompare(BenchCompareOutput),
    Reconcile(RunsReconcileOutput),
    Export(RunsExportOutput),
    Import(RunsImportOutput),
    ImportFromGhActions(GhActionsImportOutput),
    Query(RunsQueryOutput),
    Drift(RunsDriftOutput),
}

#[derive(Serialize)]
pub struct RunsListOutput {
    pub command: &'static str,
    pub runs: Vec<RunSummary>,
}

#[derive(Serialize)]
pub struct RunsShowOutput {
    pub command: &'static str,
    pub run: RunDetail,
}

#[derive(Serialize)]
pub struct RunsArtifactsOutput {
    pub command: &'static str,
    pub run_id: String,
    pub artifacts: Vec<ArtifactRecord>,
}

#[derive(Args, Clone)]
pub struct RunsArtifactArgs {
    #[command(subcommand)]
    command: RunsArtifactCommand,
}

#[derive(Subcommand, Clone)]
enum RunsArtifactCommand {
    /// Copy a recorded file artifact to a local path
    Get(RunsArtifactGetArgs),
    /// Plan or delete locally cached runner artifact downloads
    CleanupDownloads(RunsArtifactCleanupDownloadsArgs),
}

#[derive(Args, Clone)]
pub struct RunsArtifactGetArgs {
    /// Observation run id that owns the artifact
    pub run_id: String,
    /// Artifact id/path token from `homeboy runs artifacts <run-id>`
    pub artifact_id: String,
    /// Destination file path. Defaults to the recorded artifact filename.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct RunsArtifactGetOutput {
    pub command: &'static str,
    pub run_id: String,
    pub artifact_id: String,
    pub output_path: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
}

#[derive(Args, Clone, Default)]
pub struct RunsArtifactCleanupDownloadsArgs {
    /// Delete the planned cached downloads. Without this flag, only reports the plan.
    #[arg(long)]
    pub apply: bool,
    /// Limit cleanup to one runner id under the local runner artifact cache.
    #[arg(long)]
    pub runner: Option<String>,
    /// Limit cleanup to one run id. Requires --runner.
    #[arg(long)]
    pub run_id: Option<String>,
}

#[derive(Serialize)]
pub struct RunsArtifactCleanupDownloadsOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub root: String,
    pub removed: bool,
    pub file_count: usize,
    pub directory_count: usize,
    pub size_bytes: u64,
    pub paths: Vec<String>,
}

#[derive(Serialize)]
pub struct BenchHistoryOutput {
    pub command: &'static str,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    pub runs: Vec<RunDetail>,
}

#[derive(Serialize)]
pub struct BenchCompareOutput {
    pub command: &'static str,
    pub from_run: RunSummary,
    pub to_run: RunSummary,
    pub comparisons: Vec<BenchMetricComparison>,
    pub missing: Vec<BenchMissingMetric>,
}

#[derive(Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub homeboy_version: Option<String>,
    pub metadata: Value,
    pub artifacts: Vec<ArtifactRecord>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BenchMetricComparison {
    pub scenario_id: String,
    pub metric: String,
    pub from_value: f64,
    pub to_value: f64,
    pub delta: f64,
    pub percent_change: Option<f64>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchMissingMetric {
    pub scenario_id: String,
    pub metric: String,
    pub missing_from: String,
}

pub fn run(args: RunsArgs, _global: &GlobalArgs) -> CmdResult<RunsOutput> {
    match args.command {
        RunsCommand::List(args) => list_runs(args, "runs.list"),
        RunsCommand::Distribution(args) => {
            distribution::runs_distribution(args, "runs.distribution")
        }
        RunsCommand::LatestRun(args) => latest::latest_run(args),
        RunsCommand::Compare(args) => compare_runs(args),
        RunsCommand::Reconcile(args) => reconcile_runs(args),
        RunsCommand::Show { run_id } => show_run(&run_id),
        RunsCommand::Artifacts { run_id } => artifacts(&run_id),
        RunsCommand::Artifact(args) => artifact_command(args),
        RunsCommand::Findings(args) => findings::findings(args),
        RunsCommand::Finding { finding_id } => findings::finding(&finding_id),
        RunsCommand::LatestFinding(args) => findings::latest_finding(args),
        RunsCommand::Export(args) => export_runs(args),
        RunsCommand::Import(args) => import_runs(args),
        RunsCommand::Query(args) => runs_query(args),
        RunsCommand::Drift(args) => runs_drift(args),
    }
}

impl RunsArgs {
    pub fn is_markdown_mode(&self) -> bool {
        matches!(self.command, RunsCommand::Compare(ref compare) if compare::is_table_mode(compare))
    }

    pub fn is_bundle_export(&self) -> bool {
        matches!(self.command, RunsCommand::Export(_))
    }
}

pub fn run_markdown(args: RunsArgs, _global: &GlobalArgs) -> CmdResult<String> {
    match args.command {
        RunsCommand::Compare(args) => compare::run_markdown(args),
        _ => Err(Error::validation_invalid_argument(
            "output_mode",
            "Only `homeboy runs compare --format=table` supports table output",
            None,
            None,
        )),
    }
}

pub fn list_runs(args: RunsListArgs, command: &'static str) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.clone() {
        return remote::list_runner_runs(&runner_id, args, command);
    }

    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    let runs = store
        .list_runs(RunListFilter {
            kind: args.kind,
            component_id: args.component_id,
            status: args.status,
            rig_id: args.rig,
            limit: Some(args.limit),
        })?
        .into_iter()
        .map(run_summary)
        .collect();

    Ok((RunsOutput::List(RunsListOutput { command, runs }), 0))
}

fn show_run(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    Ok((
        RunsOutput::Show(RunsShowOutput {
            command: "runs.show",
            run: run_detail(&store, run)?,
        }),
        0,
    ))
}

pub fn artifacts(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    require_run(&store, run_id)?;
    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: run_id.to_string(),
            artifacts: store.list_artifacts(run_id)?,
        }),
        0,
    ))
}

fn artifact_command(args: RunsArtifactArgs) -> CmdResult<RunsOutput> {
    match args.command {
        RunsArtifactCommand::Get(args) => artifact_get(args),
        RunsArtifactCommand::CleanupDownloads(args) => remote_artifact::cleanup_downloads(args),
    }
}

fn artifact_get(args: RunsArtifactGetArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    require_run(&store, &args.run_id)?;
    let artifact = store.get_artifact(&args.artifact_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!("artifact record not found: {}", args.artifact_id),
            Some(args.artifact_id.clone()),
            None,
        )
    })?;

    if artifact.run_id != args.run_id {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "artifact does not belong to requested run",
            Some(args.artifact_id),
            None,
        ));
    }
    if artifact.artifact_type != "file" {
        if remote_artifact::is_remote_artifact(&artifact) {
            return remote_artifact::get(artifact, args.output);
        }
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            None,
        ));
    }

    let source = PathBuf::from(&artifact.path);
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&artifact.id)
        .to_string();
    let output = args.output.unwrap_or_else(|| PathBuf::from(file_name));
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let mut reader = File::open(&source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("open artifact {}", source.display())),
        )
    })?;
    let mut writer = File::create(&output).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create {}", output.display())))
    })?;
    io::copy(&mut reader, &mut writer).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "copy artifact {} to {}",
                artifact.id,
                output.display()
            )),
        )
    })?;

    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: artifact.run_id,
            artifact_id: artifact.id,
            output_path: output.display().to_string(),
            content_type: artifact.mime,
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256,
        }),
        0,
    ))
}

pub fn bench_history(
    component_id: &str,
    scenario_id: Option<&str>,
    rig_id: Option<&str>,
    limit: i64,
) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let runs = store
        .list_runs(RunListFilter {
            kind: Some("bench".to_string()),
            component_id: Some(component_id.to_string()),
            rig_id: rig_id.map(str::to_string),
            limit: Some(limit.clamp(1, 1000)),
            ..RunListFilter::default()
        })?
        .into_iter()
        .filter(|run| scenario_id.is_none_or(|scenario| run_contains_scenario(run, scenario)))
        .take(limit.max(1) as usize)
        .map(|run| run_detail(&store, run))
        .collect::<homeboy::core::Result<Vec<_>>>()?;

    Ok((
        RunsOutput::BenchHistory(BenchHistoryOutput {
            command: "bench.history",
            component_id: component_id.to_string(),
            scenario_id: scenario_id.map(str::to_string),
            rig_id: rig_id.map(str::to_string),
            runs,
        }),
        0,
    ))
}

pub fn bench_compare(from_run_id: &str, to_run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let from_run = require_run(&store, from_run_id)?;
    let to_run = require_run(&store, to_run_id)?;
    require_kind(&from_run, "bench")?;
    require_kind(&to_run, "bench")?;

    let from_summary = run_summary(from_run.clone());
    let to_summary = run_summary(to_run.clone());
    let from_metrics = bench_numeric_metrics(&from_run.metadata_json);
    let to_metrics = bench_numeric_metrics(&to_run.metadata_json);
    let mut keys = BTreeSet::new();
    keys.extend(from_metrics.keys().cloned());
    keys.extend(to_metrics.keys().cloned());

    let mut comparisons = Vec::new();
    let mut missing = Vec::new();
    for key in keys {
        match (from_metrics.get(&key), to_metrics.get(&key)) {
            (Some(from), Some(to)) => comparisons.push(BenchMetricComparison {
                scenario_id: key.0,
                metric: key.1,
                from_value: *from,
                to_value: *to,
                delta: to - from,
                percent_change: if *from == 0.0 {
                    None
                } else {
                    Some(((to - from) / from) * 100.0)
                },
            }),
            (Some(_), None) => missing.push(BenchMissingMetric {
                scenario_id: key.0,
                metric: key.1,
                missing_from: "to_run".to_string(),
            }),
            (None, Some(_)) => missing.push(BenchMissingMetric {
                scenario_id: key.0,
                metric: key.1,
                missing_from: "from_run".to_string(),
            }),
            (None, None) => {}
        }
    }

    Ok((
        RunsOutput::BenchCompare(BenchCompareOutput {
            command: "bench.compare",
            from_run: from_summary,
            to_run: to_summary,
            comparisons,
            missing,
        }),
        0,
    ))
}

fn require_run(store: &ObservationStore, run_id: &str) -> homeboy::core::Result<RunRecord> {
    store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })
}

fn require_kind(run: &RunRecord, expected: &str) -> homeboy::core::Result<()> {
    if run.kind == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "run_id",
        format!(
            "run {} is kind '{}', expected '{expected}'",
            run.id, run.kind
        ),
        Some(run.id.clone()),
        None,
    ))
}

fn run_detail(store: &ObservationStore, run: RunRecord) -> homeboy::core::Result<RunDetail> {
    let artifacts = store.list_artifacts(&run.id)?;
    Ok(RunDetail {
        summary: run_summary(run.clone()),
        homeboy_version: run.homeboy_version,
        metadata: run.metadata_json,
        artifacts,
    })
}

pub(crate) fn run_summary(run: RunRecord) -> RunSummary {
    let status_note = reconcile::running_status_note(&run);
    RunSummary {
        id: run.id,
        kind: run.kind,
        status: run.status,
        started_at: run.started_at,
        finished_at: run.finished_at,
        component_id: run.component_id,
        rig_id: run.rig_id,
        git_sha: run.git_sha,
        command: run.command,
        cwd: run.cwd,
        status_note,
    }
}

pub(super) fn run_contains_scenario(run: &RunRecord, scenario_id: &str) -> bool {
    if run.metadata_json["selected_scenarios"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(scenario_id)))
    {
        return true;
    }
    bench_numeric_metrics(&run.metadata_json)
        .keys()
        .any(|(scenario, _)| scenario == scenario_id)
}

fn bench_numeric_metrics(metadata: &Value) -> BTreeMap<(String, String), f64> {
    let mut metrics = BTreeMap::new();
    if let Some(scenarios) = metadata["scenario_metrics"].as_array() {
        for scenario in scenarios {
            collect_scenario_metrics(scenario, &mut metrics);
        }
    }
    if metrics.is_empty() {
        if let Some(scenarios) = metadata["results"]["scenarios"].as_array() {
            for scenario in scenarios {
                collect_scenario_metrics(scenario, &mut metrics);
            }
        }
    }
    metrics
}

fn collect_scenario_metrics(scenario: &Value, metrics: &mut BTreeMap<(String, String), f64>) {
    let Some(scenario_id) = scenario["scenario_id"]
        .as_str()
        .or_else(|| scenario["id"].as_str())
    else {
        return;
    };

    collect_numeric_object(scenario_id, None, &scenario["metrics"], metrics);
    if let Some(groups) = scenario["metric_groups"].as_object() {
        for (group, values) in groups {
            collect_numeric_object(scenario_id, Some(group), values, metrics);
        }
    }
}

fn collect_numeric_object(
    scenario_id: &str,
    prefix: Option<&str>,
    value: &Value,
    metrics: &mut BTreeMap<(String, String), f64>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (name, value) in object {
        let Some(number) = value.as_f64() else {
            continue;
        };
        let metric = match prefix {
            Some(prefix) => format!("{prefix}.{name}"),
            None => name.clone(),
        };
        metrics.insert((scenario_id.to_string(), metric), number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use homeboy::core::observation::{
        FindingListFilter, FindingRecord, NewFindingRecord, NewRunRecord, NewTraceSpanRecord,
        RunRecord, RunStatus, TraceSpanRecord,
    };
    use homeboy::test_support::with_isolated_home;
    use serde::Deserialize;

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
    fn run_list_filters_kind_component_rig_and_status() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let bench = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("bench");
            store
                .finish_run(&bench.id, RunStatus::Pass, None)
                .expect("finish bench");
            let trace = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("trace");
            store
                .finish_run(&trace.id, RunStatus::Fail, None)
                .expect("finish trace");

            let (output, _) = list_runs(
                RunsListArgs {
                    runner: None,
                    kind: Some("bench".to_string()),
                    component_id: Some("homeboy".to_string()),
                    rig: Some("studio".to_string()),
                    status: Some("pass".to_string()),
                    limit: 20,
                },
                "runs.list",
            )
            .expect("list");

            let RunsOutput::List(output) = output else {
                panic!("expected list output");
            };
            assert_eq!(output.runs.len(), 1);
            assert_eq!(output.runs[0].id, bench.id);
        });
    }

    #[test]
    fn run_list_reconciles_owned_dead_running_runs_before_listing() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            store
                .import_run(&RunRecord {
                    id: "dead-owned-run".to_string(),
                    kind: "bench".to_string(),
                    component_id: Some("homeboy".to_string()),
                    started_at: "2026-05-02T16:46:46Z".to_string(),
                    finished_at: None,
                    status: "running".to_string(),
                    command: Some("homeboy bench".to_string()),
                    cwd: Some("/tmp/homeboy-fixture".to_string()),
                    homeboy_version: Some("test-version".to_string()),
                    git_sha: Some("abc123".to_string()),
                    rig_id: Some("studio".to_string()),
                    metadata_json: serde_json::json!({
                        "homeboy_run_owner": { "pid": u32::MAX }
                    }),
                })
                .expect("import stale fixture");

            let (output, _) = list_runs(
                RunsListArgs {
                    runner: None,
                    kind: Some("bench".to_string()),
                    component_id: Some("homeboy".to_string()),
                    rig: Some("studio".to_string()),
                    status: None,
                    limit: 20,
                },
                "runs.list",
            )
            .expect("list");

            let RunsOutput::List(output) = output else {
                panic!("expected list output");
            };
            assert_eq!(output.runs.len(), 1);
            assert_eq!(output.runs[0].id, "dead-owned-run");
            assert_eq!(output.runs[0].status, "stale");
            assert!(output.runs[0].finished_at.is_some());
            assert_eq!(output.runs[0].status_note, None);

            let stored = store
                .get_run("dead-owned-run")
                .expect("get run")
                .expect("run exists");
            assert_eq!(stored.status, "stale");
            assert_eq!(
                stored.metadata_json["homeboy_reconciled"]["reason"],
                "owner_process_not_running"
            );
        });
    }

    #[test]
    fn run_show_includes_metadata_and_artifacts() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({ "scenario_metrics": [] }),
                ))
                .expect("run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");

            let (output, _) = show_run(&run.id).expect("show");
            let RunsOutput::Show(output) = output else {
                panic!("expected show output");
            };
            assert_eq!(output.run.summary.id, run.id);
            assert_eq!(
                output.run.metadata["scenario_metrics"],
                serde_json::json!([])
            );
            assert_eq!(output.run.artifacts.len(), 1);
            assert_eq!(output.run.artifacts[0].kind, "bench_results");
        });
    }

    #[test]
    fn artifacts_command_reports_paths() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("run");
            let artifact_path = home.path().join("trace-results.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "trace_results", &artifact_path)
                .expect("record artifact");

            let (output, _) = artifacts(&run.id).expect("artifacts");
            let RunsOutput::Artifacts(output) = output else {
                panic!("expected artifacts output");
            };
            assert_eq!(output.artifacts.len(), 1);
            let reported_path = std::path::PathBuf::from(&output.artifacts[0].path);
            let expected_file_name = format!("{}-trace-results.json", output.artifacts[0].id);
            assert_ne!(reported_path, artifact_path);
            assert!(reported_path.is_file());
            assert_eq!(
                reported_path.file_name().and_then(|name| name.to_str()),
                Some(expected_file_name.as_str())
            );
        });
    }

    #[test]
    fn artifacts_command_reports_url_artifacts() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            store
                .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
                .expect("record URL artifact");

            let (output, _) = artifacts(&run.id).expect("artifacts");
            let RunsOutput::Artifacts(output) = output else {
                panic!("expected artifacts output");
            };
            assert_eq!(output.artifacts.len(), 1);
            assert_eq!(output.artifacts[0].kind, "frontend_url");
            assert_eq!(output.artifacts[0].artifact_type, "url");
            assert_eq!(
                output.artifacts[0].url.as_deref(),
                Some("https://example.test/")
            );
        });
    }

    #[test]
    fn artifact_get_copies_registered_file_without_raw_path_lookup() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact");
            let artifact = store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");
            let output_path = home.path().join("downloaded.json");

            let (output, _) = artifact_get(RunsArtifactGetArgs {
                run_id: run.id.clone(),
                artifact_id: artifact.id.clone(),
                output: Some(output_path.clone()),
            })
            .expect("get artifact");

            let RunsOutput::ArtifactGet(output) = output else {
                panic!("expected artifact get output");
            };
            assert_eq!(output.command, "runs.artifact.get");
            assert_eq!(output.artifact_id, artifact.id);
            assert_eq!(
                std::fs::read(&output_path).expect("downloaded"),
                br#"{"ok":true}"#
            );

            let err = match artifact_get(RunsArtifactGetArgs {
                run_id: run.id,
                artifact_id: artifact_path.display().to_string(),
                output: Some(home.path().join("bad.json")),
            }) {
                Ok(_) => panic!("raw paths are not accepted as artifact ids"),
                Err(err) => err,
            };
            assert!(err.to_string().contains("artifact record not found"));
        });
    }

    #[test]
    fn findings_commands_list_and_show_records() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
                .expect("run");
            let recorded = store
                .record_finding(&NewFindingRecord {
                    run_id: run.id.clone(),
                    tool: "lint".to_string(),
                    rule: Some("security".to_string()),
                    file: Some("src/foo.php".to_string()),
                    line: Some(12),
                    severity: Some("error".to_string()),
                    fingerprint: Some("src/foo.php::security".to_string()),
                    message: "Missing escaping".to_string(),
                    fixable: Some(true),
                    metadata_json: serde_json::json!({ "category": "security" }),
                })
                .expect("finding");

            let (output, _) = findings::findings(findings::RunsFindingsArgs {
                run_id: run.id,
                tool: Some("lint".to_string()),
                file: Some("src/foo.php".to_string()),
                fingerprint: None,
                limit: 20,
            })
            .expect("list findings");
            let RunsOutput::Findings(output) = output else {
                panic!("expected findings output");
            };
            assert_eq!(output.findings.len(), 1);
            assert_eq!(output.findings[0].id, recorded.id);
            assert_eq!(output.findings[0].message, "Missing escaping");

            let (output, _) = findings::finding(&recorded.id).expect("show finding");
            let RunsOutput::Finding(output) = output else {
                panic!("expected finding output");
            };
            assert_eq!(output.finding.metadata_json["category"], "security");
            assert_eq!(output.finding.fixable, Some(true));
        });
    }

    #[test]
    fn latest_run_command_returns_newest_matching_run() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let old = store
                .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
                .expect("old");
            store
                .finish_run(&old.id, RunStatus::Pass, None)
                .expect("finish old");
            let latest = store
                .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
                .expect("latest");
            store
                .finish_run(&latest.id, RunStatus::Fail, None)
                .expect("finish latest");

            let (output, _) = latest::latest_run(latest::RunsLatestRunArgs {
                kind: Some("lint".to_string()),
                component_id: Some("homeboy".to_string()),
                rig: Some("studio".to_string()),
                status: None,
            })
            .expect("latest run");

            let RunsOutput::LatestRun(output) = output else {
                panic!("expected latest run output");
            };
            assert_eq!(output.command, "runs.latest-run");
            assert_eq!(output.run.id, latest.id);
        });
    }

    #[test]
    fn latest_finding_command_uses_latest_matching_run() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let old_run = store
                .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
                .expect("old run");
            store
                .record_finding(&NewFindingRecord {
                    run_id: old_run.id.clone(),
                    tool: "lint".to_string(),
                    rule: Some("security".to_string()),
                    file: Some("src/foo.php".to_string()),
                    line: Some(12),
                    severity: Some("error".to_string()),
                    fingerprint: Some("old".to_string()),
                    message: "Old finding".to_string(),
                    fixable: Some(true),
                    metadata_json: serde_json::json!({}),
                })
                .expect("old finding");
            let latest_run = store
                .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
                .expect("latest run");
            let latest_finding = store
                .record_finding(&NewFindingRecord {
                    run_id: latest_run.id.clone(),
                    tool: "lint".to_string(),
                    rule: Some("security".to_string()),
                    file: Some("src/foo.php".to_string()),
                    line: Some(12),
                    severity: Some("error".to_string()),
                    fingerprint: Some("latest".to_string()),
                    message: "Latest finding".to_string(),
                    fixable: Some(true),
                    metadata_json: serde_json::json!({}),
                })
                .expect("latest finding");

            let (output, _) = findings::latest_finding(findings::RunsLatestFindingArgs {
                kind: Some("lint".to_string()),
                component_id: Some("homeboy".to_string()),
                rig: Some("studio".to_string()),
                status: None,
                tool: Some("lint".to_string()),
                file: Some("src/foo.php".to_string()),
            })
            .expect("latest finding command");

            let RunsOutput::LatestFinding(output) = output else {
                panic!("expected latest finding output");
            };
            assert_eq!(output.command, "runs.latest-finding");
            assert_eq!(output.run.id, latest_run.id);
            assert_eq!(output.finding.id, latest_finding.id);
            assert_eq!(output.finding.message, "Latest finding");
        });
    }

    #[test]
    fn bench_history_orders_and_filters_by_scenario() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let old = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 10.0 }
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
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 12.0 }
                        }]
                    }),
                ))
                .expect("new");
            store
                .finish_run(&new.id, RunStatus::Pass, None)
                .expect("finish new");

            let (output, _) =
                bench_history("homeboy", Some("cold"), Some("studio"), 20).expect("history");
            let RunsOutput::BenchHistory(output) = output else {
                panic!("expected history output");
            };
            assert_eq!(output.runs.len(), 2);
            assert_eq!(output.runs[0].summary.id, new.id);
            assert_eq!(output.runs[1].summary.id, old.id);
        });
    }

    #[test]
    fn bench_compare_reports_deltas_and_missing_metrics() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let from = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 100.0, "only_from": 1.0 },
                            "metric_groups": { "warm": { "mean_ms": 50.0 } }
                        }]
                    }),
                ))
                .expect("from");
            let to = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 125.0, "only_to": 2.0 },
                            "metric_groups": { "warm": { "mean_ms": 40.0 } }
                        }]
                    }),
                ))
                .expect("to");

            let (output, _) = bench_compare(&from.id, &to.id).expect("compare");
            let RunsOutput::BenchCompare(output) = output else {
                panic!("expected compare output");
            };
            let p95 = output
                .comparisons
                .iter()
                .find(|row| row.metric == "p95_ms")
                .expect("p95 row");
            assert_eq!(p95.delta, 25.0);
            assert_eq!(p95.percent_change, Some(25.0));
            assert!(output
                .comparisons
                .iter()
                .any(|row| row.metric == "warm.mean_ms" && row.delta == -10.0));
            assert!(output
                .missing
                .iter()
                .any(|row| row.metric == "only_from" && row.missing_from == "to_run"));
            assert!(output
                .missing
                .iter()
                .any(|row| row.metric == "only_to" && row.missing_from == "from_run"));
        });
    }

    #[test]
    fn missing_and_mismatched_run_ids_return_clear_errors() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let trace = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("trace");

            let missing = show_run("missing-run").err().expect("missing should fail");
            assert_eq!(missing.code.as_str(), "validation.invalid_argument");
            assert!(missing.message.contains("run record not found"));

            let mismatch = bench_compare(&trace.id, &trace.id)
                .err()
                .expect("kind mismatch should fail");
            assert_eq!(mismatch.code.as_str(), "validation.invalid_argument");
            assert!(mismatch.message.contains("expected 'bench'"));
        });
    }

    #[test]
    fn export_one_run_writes_directory_bundle() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish");
            let output = home.path().join("bundle");

            let (result, _) = export_runs(RunsExportArgs {
                run: Some(run.id.clone()),
                since: None,
                output: output.clone(),
            })
            .expect("export");

            let RunsOutput::Export(result) = result else {
                panic!("expected export output");
            };
            assert_eq!(result.run_count, 1);
            assert!(output.join("manifest.json").exists());
            assert!(output.join("runs.json").exists());
            assert!(output.join("artifacts.json").exists());
            assert!(output.join("trace_spans.json").exists());
            assert!(output.join("findings.json").exists());
            assert!(output.join("test_failures.json").exists());
            let runs: Vec<RunRecord> = read_bundle_test_json(&output.join("runs.json"));
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].id, run.id);
        });
    }

    #[test]
    fn export_includes_findings_and_test_failures() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("test", "homeboy", "studio", Value::Null))
                .expect("run");
            let lint = store
                .record_finding(&NewFindingRecord {
                    run_id: run.id.clone(),
                    tool: "lint".to_string(),
                    rule: Some("style".to_string()),
                    file: Some("src/lib.rs".to_string()),
                    line: Some(3),
                    severity: Some("warning".to_string()),
                    fingerprint: Some("lint::src/lib.rs".to_string()),
                    message: "style drift".to_string(),
                    fixable: Some(true),
                    metadata_json: serde_json::json!({ "record_kind": "lint" }),
                })
                .expect("lint finding");
            let failure = store
                .record_finding(&NewFindingRecord {
                    run_id: run.id.clone(),
                    tool: "test".to_string(),
                    rule: Some("assertion".to_string()),
                    file: Some("tests/fail.rs".to_string()),
                    line: Some(42),
                    severity: Some("error".to_string()),
                    fingerprint: Some("test::fails".to_string()),
                    message: "assertion failed".to_string(),
                    fixable: None,
                    metadata_json: serde_json::json!({
                        "record_kind": "failure",
                        "source_sidecar": "test-failures",
                    }),
                })
                .expect("test failure");
            let output = home.path().join("findings-bundle");

            let (result, _) = export_runs(RunsExportArgs {
                run: Some(run.id),
                since: None,
                output: output.clone(),
            })
            .expect("export");

            let RunsOutput::Export(result) = result else {
                panic!("expected export output");
            };
            assert_eq!(result.finding_count, 2);
            assert_eq!(result.test_failure_count, 1);
            let findings: Vec<FindingRecord> = read_bundle_test_json(&output.join("findings.json"));
            let test_failures: Vec<FindingRecord> =
                read_bundle_test_json(&output.join("test_failures.json"));
            assert_eq!(findings, vec![lint, failure.clone()]);
            assert_eq!(test_failures, vec![failure]);
        });
    }

    #[test]
    fn export_since_writes_multiple_runs() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let first = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("first");
            let second = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("second");
            let output = home.path().join("recent-bundle");

            export_runs(RunsExportArgs {
                run: None,
                since: Some("1d".to_string()),
                output: output.clone(),
            })
            .expect("export recent");

            let runs: Vec<RunRecord> = read_bundle_test_json(&output.join("runs.json"));
            let ids = runs
                .iter()
                .map(|run| run.id.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(ids, BTreeSet::from([first.id, second.id]));
        });
    }

    #[test]
    fn export_artifacts_is_metadata_only() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact");
            let artifact = store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");
            let output = home.path().join("artifact-bundle");

            export_runs(RunsExportArgs {
                run: Some(run.id),
                since: None,
                output: output.clone(),
            })
            .expect("export");

            let artifacts: Vec<ArtifactRecord> =
                read_bundle_test_json(&output.join("artifacts.json"));
            assert_eq!(artifacts, vec![artifact]);
            assert!(!output.join("files").exists());
        });
    }

    #[test]
    fn export_trace_spans_when_present() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("run");
            let span = store
                .record_trace_span(
                    NewTraceSpanRecord::builder(&run.id, "boot", "ok")
                        .duration_ms(Some(12.5))
                        .from_event(Some("start"))
                        .to_event(Some("ready"))
                        .metadata(serde_json::json!({ "phase": "cold" }))
                        .build(),
                )
                .expect("span");
            let output = home.path().join("trace-bundle");

            export_runs(RunsExportArgs {
                run: Some(run.id),
                since: None,
                output: output.clone(),
            })
            .expect("export");

            let spans: Vec<TraceSpanRecord> =
                read_bundle_test_json(&output.join("trace_spans.json"));
            assert_eq!(spans, vec![span]);
        });
    }

    #[test]
    fn import_into_empty_db_and_reimport_is_idempotent() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let bundle = home.path().join("portable-bundle");
            let run_id = {
                let store = ObservationStore::open_initialized().expect("store");
                let run = store
                    .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                    .expect("run");
                let artifact_path = home.path().join("trace.json");
                std::fs::write(&artifact_path, b"{}").expect("artifact");
                store
                    .record_artifact(&run.id, "trace_results", &artifact_path)
                    .expect("artifact record");
                store
                    .record_trace_span(
                        NewTraceSpanRecord::builder(&run.id, "first", "ok")
                            .duration_ms(Some(1.0))
                            .to_event(Some("done"))
                            .build(),
                    )
                    .expect("span");
                store
                    .record_finding(&NewFindingRecord {
                        run_id: run.id.clone(),
                        tool: "test".to_string(),
                        rule: Some("assertion".to_string()),
                        file: Some("tests/fail.rs".to_string()),
                        line: Some(42),
                        severity: Some("error".to_string()),
                        fingerprint: Some("test::fails".to_string()),
                        message: "assertion failed".to_string(),
                        fixable: None,
                        metadata_json: serde_json::json!({ "record_kind": "failure" }),
                    })
                    .expect("finding");
                export_runs(RunsExportArgs {
                    run: Some(run.id.clone()),
                    since: None,
                    output: bundle.clone(),
                })
                .expect("export");
                run.id
            };
            std::fs::remove_file(home.path().join(".local/share/homeboy/homeboy.sqlite"))
                .expect("remove db");

            import_runs(RunsImportArgs {
                input: Some(bundle.clone()),
                ..RunsImportArgs::default()
            })
            .expect("import");
            import_runs(RunsImportArgs {
                input: Some(bundle.clone()),
                ..RunsImportArgs::default()
            })
            .expect("second import is idempotent");

            let store = ObservationStore::open_initialized().expect("store");
            assert!(store.get_run(&run_id).expect("get").is_some());
            assert_eq!(store.list_artifacts(&run_id).expect("artifacts").len(), 1);
            assert_eq!(store.list_trace_spans(&run_id).expect("spans").len(), 1);
            assert_eq!(
                store
                    .list_findings(FindingListFilter {
                        run_id: Some(run_id),
                        ..FindingListFilter::default()
                    })
                    .expect("findings")
                    .len(),
                1
            );
        });
    }

    #[test]
    fn malformed_bundle_validation_fails_clearly() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let bundle = home.path().join("bad-bundle");
            std::fs::create_dir_all(&bundle).expect("bundle dir");
            std::fs::write(bundle.join("manifest.json"), "not json").expect("manifest");

            let err = match import_runs(RunsImportArgs {
                input: Some(bundle),
                ..RunsImportArgs::default()
            }) {
                Ok(_) => panic!("malformed bundle should fail"),
                Err(err) => err,
            };

            assert_eq!(err.code.as_str(), "validation.invalid_json");
        });
    }

    #[test]
    fn conflicting_existing_rows_fail_clearly() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            let bundle = home.path().join("conflict-bundle");
            export_runs(RunsExportArgs {
                run: Some(run.id.clone()),
                since: None,
                output: bundle.clone(),
            })
            .expect("export");
            let mut runs: Vec<RunRecord> = read_bundle_test_json(&bundle.join("runs.json"));
            runs[0].status = "pass".to_string();
            std::fs::write(
                bundle.join("runs.json"),
                serde_json::to_string_pretty(&runs).expect("json"),
            )
            .expect("rewrite runs");

            let err = match import_runs(RunsImportArgs {
                input: Some(bundle),
                ..RunsImportArgs::default()
            }) {
                Ok(_) => panic!("conflicting import should fail"),
                Err(err) => err,
            };

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err
                .message
                .contains("conflicts with imported bundle record"));
        });
    }

    fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
    }
}
