#[cfg(test)]
mod artifact_index_tests;
mod bench;
mod bundle;
#[cfg(test)]
mod bundle_import_tests;
mod common;
mod compare;
#[cfg(test)]
mod corpus_tests;
mod disk;
mod distribution;
mod drift;
mod evidence;
mod findings;
mod gh_actions;
mod latest;
mod loop_sync;
mod query;
mod reconcile;
mod remote;
mod remote_artifact;

use std::fs::File;
use std::io;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunListFilter, RunRecord};
use homeboy::core::Error;

use super::{CmdResult, GlobalArgs};
pub use bench::{bench_compare, bench_history, BenchCompareOutput, BenchHistoryOutput};
pub(super) use bench::{bench_numeric_metrics, run_contains_scenario};
use bundle::{
    export_runs, import_runs, RunsExportArgs, RunsExportOutput, RunsImportArgs, RunsImportOutput,
};
pub use common::RunSummary;
use compare::{compare_runs, RunsCompareArgs, RunsCompareOutput};
pub use distribution::{runs_distribution, RunsDistributionArgs, RunsDistributionOutput};
use drift::{runs_drift, RunsDriftArgs};
use evidence::{evidence, RunsEvidenceOutput};
use findings::{RunsFindingOutput, RunsFindingsOutput};
use gh_actions::GhActionsImportOutput;
use latest::{RunsLatestFindingOutput, RunsLatestRunArgs, RunsLatestRunOutput};
use loop_sync::{loop_sync, RunsLoopSyncArgs, RunsLoopSyncOutput};
use query::{runs_query, RunsQueryArgs, RunsQueryOutput};
use reconcile::{reconcile_runs, RunsReconcileArgs, RunsReconcileOutput};

#[cfg(test)]
pub(crate) use common::SkippedArtifactRow;
pub(crate) use drift::RunsDriftOutput;
#[cfg(test)]
pub(crate) use drift::{DriftValue, RunsDriftFilters};
#[cfg(test)]
pub(crate) use query::{
    QueryGroup, QueryRow, RunsQueryFilters, RunsQueryOutput as TestRunsQueryOutput,
};

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
    /// Show stable evidence registry data for one run
    Evidence { run_id: String },
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
    /// Sync continuous-loop archive directories into observation artifacts.
    LoopSync(RunsLoopSyncArgs),
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
#[serde(tag = "variant", content = "payload", rename_all = "snake_case")]
pub enum RunsOutput {
    List(RunsListOutput),
    Distribution(RunsDistributionOutput),
    LatestRun(RunsLatestRunOutput),
    Compare(RunsCompareOutput),
    Show(RunsShowOutput),
    Evidence(RunsEvidenceOutput),
    Artifacts(RunsArtifactsOutput),
    ArtifactGet(RunsArtifactGetOutput),
    ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput),
    ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput),
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
    LoopSync(RunsLoopSyncOutput),
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
    /// Plan or delete persisted local run artifacts and their database records
    CleanupPersisted(RunsArtifactCleanupPersistedArgs),
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

#[derive(Args, Clone)]
pub struct RunsArtifactCleanupPersistedArgs {
    /// Delete planned artifact files/directories and their DB rows. Without this flag, only reports the plan.
    #[arg(long)]
    pub apply: bool,
    /// Only include artifacts older than this many days.
    #[arg(long, default_value_t = 30)]
    pub older_than_days: i64,
    /// Limit cleanup to one run id.
    #[arg(long)]
    pub run_id: Option<String>,
    /// Limit cleanup to one artifact kind.
    #[arg(long)]
    pub kind: Option<String>,
    /// Limit cleanup to one artifact type (`file` or `directory`).
    #[arg(long = "type")]
    pub artifact_type: Option<String>,
    /// Limit cleanup to one run kind (`bench`, `trace`, etc.).
    #[arg(long)]
    pub run_kind: Option<String>,
    /// Limit cleanup to one component id.
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Maximum artifact rows to inspect in one invocation.
    #[arg(long, default_value_t = 1000)]
    pub limit: i64,
}

#[derive(Serialize)]
pub struct RunsArtifactCleanupPersistedOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub artifact_root: String,
    pub older_than_days: i64,
    pub inspected_count: usize,
    pub planned_record_count: usize,
    pub planned_file_count: usize,
    pub planned_directory_count: usize,
    pub planned_size_bytes: u64,
    pub removed_record_count: usize,
    pub removed_file_count: usize,
    pub removed_directory_count: usize,
    pub removed_size_bytes: u64,
    pub skipped_count: usize,
    pub rows: Vec<RunsArtifactCleanupPersistedRow>,
}

#[derive(Serialize)]
pub struct RunsArtifactCleanupPersistedRow {
    pub artifact_id: String,
    pub run_id: String,
    pub run_kind: String,
    pub component_id: Option<String>,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub created_at: String,
    pub exists: bool,
    pub action: String,
    pub reason: String,
    pub size_bytes: u64,
}

#[derive(Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub homeboy_version: Option<String>,
    pub metadata: Value,
    pub artifacts: Vec<ArtifactRecord>,
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
        RunsCommand::Evidence { run_id } => evidence(&run_id),
        RunsCommand::Artifacts { run_id } => artifacts(&run_id),
        RunsCommand::Artifact(args) => artifact_command(args),
        RunsCommand::Findings(args) => findings::findings(args),
        RunsCommand::Finding { finding_id } => findings::finding(&finding_id),
        RunsCommand::LatestFinding(args) => findings::latest_finding(args),
        RunsCommand::Export(args) => export_runs(args),
        RunsCommand::Import(args) => import_runs(args),
        RunsCommand::Query(args) => runs_query(args),
        RunsCommand::Drift(args) => runs_drift(args),
        RunsCommand::LoopSync(args) => loop_sync(args),
    }
}

pub fn global_runner_error(args: &RunsArgs, runner_id: &str) -> Error {
    let (message, hints) = args.global_runner_guidance(runner_id);
    Error::validation_invalid_argument("runner", message, Some(runner_id.to_string()), Some(hints))
}

impl RunsArgs {
    pub fn is_markdown_mode(&self) -> bool {
        matches!(self.command, RunsCommand::Compare(ref compare) if compare::is_table_mode(compare))
    }

    pub fn is_bundle_export(&self) -> bool {
        matches!(self.command, RunsCommand::Export(_))
    }

    pub fn is_artifact_get(&self) -> bool {
        matches!(
            self.command,
            RunsCommand::Artifact(RunsArtifactArgs {
                command: RunsArtifactCommand::Get(_),
            })
        )
    }

    fn global_runner_guidance(&self, runner_id: &str) -> (String, Vec<String>) {
        match &self.command {
            RunsCommand::List(_) => (
                format!(
                    "Use the runs-list runner option after the subcommand: `homeboy runs list --runner {runner_id}`."
                ),
                vec![
                    "The top-level --runner flag is reserved for Lab offload commands, not observation-store queries.".to_string(),
                    format!("Run `homeboy runs list --runner {runner_id}` to query the connected runner daemon."),
                ],
            ),
            RunsCommand::Show { run_id }
            | RunsCommand::Evidence { run_id }
            | RunsCommand::Artifacts { run_id } => (
                format!(
                    "Lab-offloaded run records are mirrored locally; inspect run `{run_id}` with `homeboy runs show {run_id}` without --runner."
                ),
                vec![
                    format!("Run `homeboy runs show {run_id}` to inspect the mirrored local run record."),
                    format!("Run `homeboy runs artifacts {run_id}` to list mirrored artifact records."),
                    "Use `homeboy runs artifact get <run-id> <artifact-id>` for retrievable runner artifacts recorded in the local observation store.".to_string(),
                ],
            ),
            RunsCommand::Artifact(_) => (
                "Runner artifact commands use the local mirrored observation store; rerun without top-level --runner.".to_string(),
                vec![
                    "Run `homeboy runs artifacts <run-id>` without --runner to find the artifact id.".to_string(),
                    "Run `homeboy runs artifact get <run-id> <artifact-id>` without --runner to retrieve a recorded runner artifact.".to_string(),
                ],
            ),
            _ => (
                "The top-level --runner flag is reserved for Lab offload commands; runs queries inspect the local observation store unless a subcommand documents its own --runner option.".to_string(),
                vec![
                    "Omit top-level --runner for local mirrored run records.".to_string(),
                    "Use `homeboy runs list --runner <id>` only when listing runs from a connected runner daemon.".to_string(),
                ],
            ),
        }
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
        .map(|run| run_summary_with_artifact_index(&store, run))
        .collect();

    Ok((RunsOutput::List(RunsListOutput { command, runs }), 0))
}

fn show_run(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
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
        RunsArtifactCommand::CleanupPersisted(args) => remote_artifact::cleanup_persisted(args),
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
        if artifact.artifact_type == "metadata-only" {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "artifact {} was imported as metadata only; artifact bytes are not available in this bundle",
                    artifact.id
                ),
                Some(artifact.id),
                None,
            ));
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
    if !source.is_file() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} file is missing or unreadable at {}; rerun the source command or import a bundle that includes artifact bytes",
                artifact.id,
                source.display()
            ),
            Some(artifact.id),
            None,
        ));
    }
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

pub(super) fn require_run(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<RunRecord> {
    store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })
}

pub(super) fn run_detail(
    store: &ObservationStore,
    run: RunRecord,
) -> homeboy::core::Result<RunDetail> {
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
        artifact_index: None,
    }
}

fn run_summary_with_artifact_index(store: &ObservationStore, run: RunRecord) -> RunSummary {
    let artifact_index = homeboy::core::rig::artifact_index_for_run(store, &run);
    RunSummary {
        artifact_index,
        ..run_summary(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    use homeboy::core::observation::{
        FindingListFilter, NewFindingRecord, NewRunRecord, NewTraceSpanRecord,
        RecordedHomeboyFinding, RunRecord, RunStatus, TraceSpanRecord,
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

    fn dead_owned_run(id: &str) -> RunRecord {
        RunRecord {
            id: id.to_string(),
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
            metadata_json: serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } }),
        }
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
                .import_run(&dead_owned_run("dead-owned-run"))
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
    fn run_show_reconciles_owned_dead_running_run_before_displaying() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            store
                .import_run(&dead_owned_run("dead-owned-run"))
                .expect("import stale fixture");
            let (output, _) = show_run("dead-owned-run").expect("show");
            let RunsOutput::Show(output) = output else {
                panic!("expected show output");
            };
            assert_eq!(output.run.summary.status, "stale");
            assert_eq!(
                output.run.metadata["homeboy_reconciled"]["reason"],
                "owner_process_not_running"
            );
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
            assert_eq!(output.findings[0].finding.message, "Missing escaping");

            let (output, _) = findings::finding(&recorded.id).expect("show finding");
            let RunsOutput::Finding(output) = output else {
                panic!("expected finding output");
            };
            assert_eq!(output.finding.finding.category.as_deref(), Some("security"));
            assert_eq!(output.finding.finding.fix.fixable, Some(true));
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
            assert_eq!(output.finding.finding.message, "Latest finding");
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

            let mismatch = bench_compare(&trace.id, &trace.id, &[])
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
            let findings: Vec<RecordedHomeboyFinding> =
                read_bundle_test_json(&output.join("findings.json"));
            let test_failures: Vec<RecordedHomeboyFinding> =
                read_bundle_test_json(&output.join("test_failures.json"));
            assert_eq!(findings, vec![lint.into(), failure.clone().into()]);
            assert_eq!(test_failures, vec![failure.into()]);
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
    fn export_rewrites_unproven_remote_artifact_paths_as_metadata_only() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            store
                .import_artifact(&ArtifactRecord {
                    id: "remote-trace".to_string(),
                    run_id: run.id.clone(),
                    kind: "trace".to_string(),
                    artifact_type: "file".to_string(),
                    path: "/srv/remote-only/trace.zip".to_string(),
                    url: None,
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    metadata_json: serde_json::json!({}),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");
            let output = home.path().join("remote-artifact-bundle");

            export_runs(RunsExportArgs {
                run: Some(run.id),
                since: None,
                output: output.clone(),
            })
            .expect("export");

            let artifacts: Vec<ArtifactRecord> =
                read_bundle_test_json(&output.join("artifacts.json"));
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].artifact_type, "metadata-only");
            assert_eq!(artifacts[0].path, "metadata-only:trace.zip");
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
