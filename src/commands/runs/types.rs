//! Argument, command, and output type definitions for the `runs` command.
//!
//! These are the clap argument/subcommand enums and the serializable output
//! payloads. Behavior lives in [`super::dispatch`] and [`super::handlers`].

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::artifact_links::ArtifactViewerDescriptor;
use homeboy::core::artifacts::{ArtifactPreviewEntrypoint, MatrixArtifactSummary};
use homeboy::core::observation::runs_service;
use homeboy::core::observation::ArtifactRecord;
use homeboy::core::runners::RunnerArtifactRef;
use homeboy::core::validation_progress::ValidationCommandSummary;

use super::bench::{BenchCompareOutput, RunsBenchCompareArgs};
use super::bundle::{RunsExportArgs, RunsExportOutput, RunsImportArgs, RunsImportOutput};
use super::common::RunSummary;
use super::compare::{RunsCompareArgs, RunsCompareOutput};
use super::distribution::{RunsDistributionArgs, RunsDistributionOutput};
use super::drift::{RunsDriftArgs, RunsDriftOutput};
use super::evidence::RunsEvidenceOutput;
use super::findings;
use super::findings::{RunsFindingOutput, RunsFindingsOutput};
use super::fuzz_compare::RunsFuzzCompareArgs;
use super::gh_actions::GhActionsImportOutput;
use super::hotspots::{RunsHotspotsArgs, RunsHotspotsOutput};
use super::latest::{RunsLatestFindingOutput, RunsLatestRunArgs, RunsLatestRunOutput};
use super::loop_sync::{RunsLoopSyncArgs, RunsLoopSyncOutput};
use super::query::{RunsQueryArgs, RunsQueryOutput};
use super::reconcile::{RunsReconcileArgs, RunsReconcileOutput};
use super::refs::{RunsRefsArgs, RunsRefsOutput};
use crate::commands::fuzz::FuzzCompareOutput;

pub(super) const DEFAULT_LIMIT: i64 = 20;

/// Command-layer artifact viewer for WordPress Playground blueprint artifacts.
///
/// The viewer is an ecosystem-specific presentation concern, so it lives in the
/// command layer (which composes ecosystem integrations) rather than in core,
/// which stays agnostic and only owns the generic [`ArtifactViewerDescriptor`].
pub const WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER: ArtifactViewerDescriptor =
    ArtifactViewerDescriptor::new(
        "wordpress-playground-blueprint",
        "https://playground.wordpress.net/",
        "blueprint-url",
    );

#[derive(Args, Clone)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub(super) command: RunsCommand,
}

#[derive(Subcommand, Clone)]
pub(super) enum RunsCommand {
    /// List persisted observation runs; canonical replacement for `bench history` and `rig runs`
    List(RunsListArgs),
    /// Aggregate persisted run metadata; canonical replacement for `bench distribution`
    Distribution(RunsDistributionArgs),
    /// Show the latest persisted observation run matching filters
    LatestRun(RunsLatestRunArgs),
    /// Compare selected metrics across persisted run history
    Compare(RunsCompareArgs),
    /// Compare two persisted benchmark runs by exact run id; canonical replacement for `bench compare`
    BenchCompare(RunsBenchCompareArgs),
    /// Compare two persisted fuzz runs by exact run id
    FuzzCompare(RunsFuzzCompareArgs),
    /// Aggregate hotspot rankings across persisted fuzz run artifacts
    Hotspots(RunsHotspotsArgs),
    /// Mark orphaned running observation records stale
    Reconcile(RunsReconcileArgs),
    /// Show one persisted observation run
    Show {
        run_id: String,
        /// Print the full JSON output instead of the compact human summary.
        /// The compact summary surfaces status, key metadata, and artifact
        /// pointers with inspect commands; the full payload is unchanged and
        /// always available with this flag or via `--output <file>`.
        #[arg(long)]
        json: bool,
    },
    /// Show a generic resume plan for a validation-progress run
    ResumePlan { run_id: String },
    /// Show stable evidence registry data for one run; start here for reviewer-facing evidence
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
    /// Emit stable run/artifact refs for matching runs.
    Refs(RunsRefsArgs),
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
    /// Benchmark scenario ID. Only applies to bench metadata.
    #[arg(long = "scenario")]
    pub scenario_id: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
    /// Maximum runs to return
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: i64,
    /// Include active runner jobs from connected runner daemons
    #[arg(long)]
    pub include_active_runner_jobs: bool,
}

#[derive(Serialize)]
#[serde(tag = "variant", content = "payload", rename_all = "snake_case")]
pub enum RunsOutput {
    List(RunsListOutput),
    Distribution(RunsDistributionOutput),
    LatestRun(RunsLatestRunOutput),
    Compare(RunsCompareOutput),
    Show(RunsShowOutput),
    ResumePlan(RunsResumePlanOutput),
    Evidence(RunsEvidenceOutput),
    Artifacts(RunsArtifactsOutput),
    ArtifactAttach(RunsArtifactAttachOutput),
    ArtifactGet(RunsArtifactGetOutput),
    ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput),
    ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput),
    Findings(RunsFindingsOutput),
    Finding(RunsFindingOutput),
    LatestFinding(RunsLatestFindingOutput),
    BenchCompare(BenchCompareOutput),
    FuzzCompare(FuzzCompareOutput),
    Hotspots(RunsHotspotsOutput),
    Reconcile(RunsReconcileOutput),
    Export(RunsExportOutput),
    Import(RunsImportOutput),
    ImportFromGhActions(GhActionsImportOutput),
    Query(RunsQueryOutput),
    Refs(RunsRefsOutput),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_entrypoints: Vec<ArtifactPreviewEntrypoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix_summary: Option<MatrixArtifactSummary>,
}

#[derive(Serialize)]
pub struct RunsResumePlanOutput {
    pub command: &'static str,
    pub run_id: String,
    pub status: String,
    pub completed_count: usize,
    pub command_count: usize,
    pub failed_count: usize,
    pub last_completed_command: Option<ValidationCommandSummary>,
    pub active_command: Option<ValidationCommandSummary>,
    pub next_command: Option<ValidationCommandSummary>,
    pub hints: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RunsArtifactArgs {
    #[command(subcommand)]
    pub(super) command: RunsArtifactCommand,
}

#[derive(Subcommand, Clone)]
pub(super) enum RunsArtifactCommand {
    /// Attach an existing runner-side output file to a persisted run
    Attach(RunsArtifactAttachArgs),
    /// Copy a recorded file artifact to a local path
    Get(RunsArtifactGetArgs),
    /// Plan or delete locally cached runner artifact downloads
    CleanupDownloads(RunsArtifactCleanupDownloadsArgs),
    /// Plan or delete persisted local run artifacts and their database records
    CleanupPersisted(RunsArtifactCleanupPersistedArgs),
}

#[derive(Args, Clone)]
pub struct RunsArtifactAttachArgs {
    /// Observation run id that should own the attached artifact
    pub run_id: String,
    /// Runner ID that can read the path
    #[arg(long)]
    pub runner: String,
    /// Absolute runner-side file path under an allowed workspace/output root
    #[arg(long)]
    pub path: String,
    /// Artifact kind/name to record in the observation store
    #[arg(long)]
    pub name: String,
}

#[derive(Serialize)]
pub struct RunsArtifactAttachOutput {
    pub command: &'static str,
    pub run_id: String,
    pub runner_id: String,
    pub source_path: String,
    pub artifact: ArtifactRecord,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<RunnerArtifactRef>,
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

pub type RunsArtifactCleanupPersistedRow = runs_service::PersistedArtifactCleanupRow;

#[derive(Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub homeboy_version: Option<String>,
    pub metadata: Value,
    pub artifacts: Vec<ArtifactRecord>,
}
