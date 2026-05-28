use std::path::PathBuf;

use clap::Args;
use homeboy::core::observation::ArtifactRecord;
use serde::Serialize;
use serde_json::Value;

use super::super::disk::DiskBudget;

#[derive(Args, Clone)]
pub struct RunsLoopSyncArgs {
    /// Local directory containing copied remote loop archives.
    pub archive_root: PathBuf,

    /// Optional component label for filtering the resulting observation run.
    #[arg(long = "component")]
    pub component_id: Option<String>,

    /// Optional rig/loop label for filtering the resulting observation run.
    #[arg(long)]
    pub rig: Option<String>,

    /// Optional free-form labels recorded in run metadata.
    #[arg(long = "label")]
    pub labels: Vec<String>,

    /// Mark heartbeat/session files stale after this many minutes.
    #[arg(long, default_value_t = 120)]
    pub stale_after_minutes: u64,

    /// Retention budget used for reporting old archive candidates.
    #[arg(long, default_value_t = 30)]
    pub retention_days: u64,

    /// Maximum ranked patch candidates to include in triage output.
    #[arg(long, default_value_t = 20)]
    pub patch_limit: usize,

    /// Inspect and triage without writing observation runs or artifacts.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Serialize)]
pub struct RunsLoopSyncOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub archive_root: String,
    pub run_id: Option<String>,
    pub synced_artifacts: Vec<ArtifactRecord>,
    pub triage: LoopTriageSummary,
}

#[derive(Clone, Serialize)]
pub struct LoopTriageSummary {
    pub heartbeat: LoopHeartbeatSummary,
    pub disk: DiskBudget,
    pub retention: LoopRetentionSummary,
    pub archive_count: usize,
    pub report_count: usize,
    pub patch_count: usize,
    pub reviewer_failure_count: usize,
    pub stale_job_count: usize,
    pub patch_candidates: Vec<LoopPatchCandidate>,
    pub reviewer_failures: Vec<LoopReviewerFailure>,
    pub indexed_files: Vec<LoopIndexedFile>,
}

#[derive(Clone, Serialize)]
pub struct LoopHeartbeatSummary {
    pub status: String,
    pub stale: bool,
    pub stale_after_minutes: u64,
    pub latest_path: Option<String>,
    pub latest_modified_at: Option<String>,
    pub latest_age_seconds: Option<u64>,
    pub payload: Value,
}

#[derive(Clone, Serialize)]
pub struct LoopRetentionSummary {
    pub retention_days: u64,
    pub candidate_count: usize,
    pub candidate_bytes: u64,
    pub candidates: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct LoopPatchCandidate {
    pub path: String,
    pub fingerprint: String,
    pub rank: i64,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub duplicate_count: usize,
    pub labels: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct LoopReviewerFailure {
    pub path: String,
    pub reason: String,
    pub modified_at: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct LoopIndexedFile {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

#[derive(Clone)]
pub(super) struct LoopInventory {
    pub archive_root: PathBuf,
    pub archives: Vec<PathBuf>,
    pub reports: Vec<LoopIndexedFile>,
    pub heartbeat_files: Vec<LoopIndexedFile>,
    pub stale_jobs: Vec<LoopIndexedFile>,
    pub reviewer_failures: Vec<LoopReviewerFailure>,
    pub patch_candidates: Vec<LoopPatchCandidate>,
    pub indexed_files: Vec<LoopIndexedFile>,
    pub retention_candidates: Vec<LoopIndexedFile>,
    pub total_size_bytes: u64,
}
