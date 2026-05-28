use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::Args;
use homeboy::core::observation::{ArtifactRecord, NewRunRecord, ObservationStore, RunStatus};
use homeboy::core::{Error, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{CmdResult, RunsOutput};

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
    pub disk: LoopDiskSummary,
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
pub struct LoopDiskSummary {
    pub path: String,
    pub available_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub used_percent: Option<f64>,
    pub status: String,
    pub warning: Option<String>,
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
struct LoopInventory {
    archive_root: PathBuf,
    archives: Vec<PathBuf>,
    reports: Vec<LoopIndexedFile>,
    heartbeat_files: Vec<LoopIndexedFile>,
    stale_jobs: Vec<LoopIndexedFile>,
    reviewer_failures: Vec<LoopReviewerFailure>,
    patch_candidates: Vec<LoopPatchCandidate>,
    indexed_files: Vec<LoopIndexedFile>,
    retention_candidates: Vec<LoopIndexedFile>,
    total_size_bytes: u64,
}

pub fn loop_sync(args: RunsLoopSyncArgs) -> CmdResult<RunsOutput> {
    if !args.archive_root.is_dir() {
        return Err(Error::validation_invalid_argument(
            "archive_root",
            format!(
                "archive root is not a directory: {}",
                args.archive_root.display()
            ),
            Some(args.archive_root.to_string_lossy().to_string()),
            None,
        ));
    }

    let inventory = inventory_loop_archives(&args)?;
    let triage = loop_triage_summary(&inventory, &args);

    let (run_id, artifacts) = if args.dry_run {
        (None, Vec::new())
    } else {
        persist_loop_inventory(&inventory, &triage, &args)?
    };

    Ok((
        RunsOutput::LoopSync(RunsLoopSyncOutput {
            command: "runs.loop-sync",
            dry_run: args.dry_run,
            archive_root: inventory.archive_root.display().to_string(),
            run_id,
            synced_artifacts: artifacts,
            triage,
        }),
        0,
    ))
}

fn inventory_loop_archives(args: &RunsLoopSyncArgs) -> Result<LoopInventory> {
    let archive_root = args.archive_root.canonicalize().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "canonicalize archive root {}",
                args.archive_root.display()
            )),
        )
    })?;
    let mut archives = top_level_archives(&archive_root)?;
    archives.sort();

    let mut reports = Vec::new();
    let mut heartbeat_files = Vec::new();
    let mut stale_jobs = Vec::new();
    let mut reviewer_failures = Vec::new();
    let mut patch_candidates = Vec::new();
    let mut indexed_files = Vec::new();
    let mut retention_candidates = Vec::new();
    let mut total_size_bytes = 0u64;
    let mut patch_fingerprints = BTreeMap::<String, usize>::new();

    for file in recursive_files(&archive_root)? {
        let metadata = fs::metadata(&file).map_err(|e| io_error("read metadata", &file, e))?;
        let size_bytes = metadata.len();
        total_size_bytes = total_size_bytes.saturating_add(size_bytes);
        let relative = relative_display(&archive_root, &file);
        let name = file_name_lowercase(&file);
        let kind = classify_file(&file);
        let indexed = LoopIndexedFile {
            path: relative.clone(),
            kind: kind.to_string(),
            size_bytes,
            modified_at: metadata.modified().ok().map(system_time_to_rfc3339),
        };

        if is_older_than(
            metadata.modified().ok(),
            args.retention_days.saturating_mul(24 * 60),
        ) {
            retention_candidates.push(indexed.clone());
        }
        if kind == "heartbeat" {
            heartbeat_files.push(indexed.clone());
        }
        if kind == "report" {
            reports.push(indexed.clone());
        }
        if (name.contains("job") || name.contains("session") || name.contains("reviewer"))
            && is_older_than(metadata.modified().ok(), args.stale_after_minutes)
        {
            stale_jobs.push(indexed.clone());
        }
        if kind == "patch" {
            let fingerprint = patch_fingerprint(&file, size_bytes)?;
            let count = patch_fingerprints.entry(fingerprint.clone()).or_insert(0);
            *count += 1;
            patch_candidates.push(LoopPatchCandidate {
                path: relative.clone(),
                fingerprint,
                rank: patch_rank(&file, size_bytes),
                size_bytes,
                modified_at: indexed.modified_at.clone(),
                duplicate_count: 0,
                labels: patch_labels(&file),
            });
        }
        if kind == "reviewer_failure" {
            reviewer_failures.push(LoopReviewerFailure {
                path: relative.clone(),
                reason: reviewer_failure_reason(&file),
                modified_at: indexed.modified_at.clone(),
            });
        }
        indexed_files.push(indexed);
    }

    for candidate in &mut patch_candidates {
        candidate.duplicate_count = patch_fingerprints
            .get(&candidate.fingerprint)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
    }
    patch_candidates.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.path.cmp(&b.path)));
    patch_candidates.truncate(args.patch_limit);

    Ok(LoopInventory {
        archive_root,
        archives,
        reports,
        heartbeat_files,
        stale_jobs,
        reviewer_failures,
        patch_candidates,
        indexed_files,
        retention_candidates,
        total_size_bytes,
    })
}

fn persist_loop_inventory(
    inventory: &LoopInventory,
    triage: &LoopTriageSummary,
    args: &RunsLoopSyncArgs,
) -> Result<(Option<String>, Vec<ArtifactRecord>)> {
    let store = ObservationStore::open_initialized()?;
    let run = store.start_run(loop_run_record(inventory, triage, args))?;
    let mut artifacts = Vec::new();

    for archive in &inventory.archives {
        if archive.is_dir() {
            artifacts.push(store.record_directory_artifact(&run.id, "loop_archive", archive)?);
        } else if archive.is_file() {
            artifacts.push(store.record_artifact(&run.id, "loop_archive", archive)?);
        }
    }

    for file in files_for_artifact_index(inventory) {
        if file.is_file() {
            artifacts.push(store.record_artifact(&run.id, classify_file(&file), file)?);
        }
    }

    let finished = store.finish_run(&run.id, RunStatus::Pass, None)?;
    Ok((Some(finished.id), artifacts))
}

fn loop_run_record(
    inventory: &LoopInventory,
    triage: &LoopTriageSummary,
    args: &RunsLoopSyncArgs,
) -> NewRunRecord {
    let mut builder = NewRunRecord::builder("loop-archive-sync")
        .current_homeboy_version()
        .cwd_path(&inventory.archive_root)
        .command(format!(
            "homeboy runs loop-sync {}",
            inventory.archive_root.display()
        ))
        .metadata(serde_json::json!({
            "archive_root": inventory.archive_root,
            "labels": args.labels,
            "sync": {
                "archive_count": inventory.archives.len(),
                "indexed_file_count": inventory.indexed_files.len(),
                "total_size_bytes": inventory.total_size_bytes,
            },
            "triage": triage,
        }));
    if let Some(component_id) = &args.component_id {
        builder = builder.component_id(component_id.clone());
    }
    if let Some(rig) = &args.rig {
        builder = builder.rig_id(rig.clone());
    }
    builder.build()
}

fn files_for_artifact_index(inventory: &LoopInventory) -> Vec<PathBuf> {
    let mut paths = HashSet::<PathBuf>::new();
    for indexed in inventory
        .heartbeat_files
        .iter()
        .chain(inventory.reports.iter())
    {
        paths.insert(inventory.archive_root.join(&indexed.path));
    }
    for candidate in &inventory.patch_candidates {
        paths.insert(inventory.archive_root.join(&candidate.path));
    }
    for failure in &inventory.reviewer_failures {
        paths.insert(inventory.archive_root.join(&failure.path));
    }
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort();
    paths
}

fn loop_triage_summary(inventory: &LoopInventory, args: &RunsLoopSyncArgs) -> LoopTriageSummary {
    let heartbeat = latest_heartbeat(inventory, args.stale_after_minutes);
    LoopTriageSummary {
        heartbeat,
        disk: disk_summary(&inventory.archive_root),
        retention: retention_summary(inventory, args.retention_days),
        archive_count: inventory.archives.len(),
        report_count: inventory.reports.len(),
        patch_count: inventory
            .indexed_files
            .iter()
            .filter(|file| file.kind == "patch")
            .count(),
        reviewer_failure_count: inventory.reviewer_failures.len(),
        stale_job_count: inventory.stale_jobs.len(),
        patch_candidates: inventory.patch_candidates.clone(),
        reviewer_failures: inventory.reviewer_failures.clone(),
        indexed_files: inventory.indexed_files.clone(),
    }
}

fn latest_heartbeat(inventory: &LoopInventory, stale_after_minutes: u64) -> LoopHeartbeatSummary {
    let latest = inventory
        .heartbeat_files
        .iter()
        .filter_map(|file| {
            let path = inventory.archive_root.join(&file.path);
            fs::metadata(path)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .map(|modified| (file, modified))
        })
        .max_by_key(|(_, modified)| *modified);

    let Some((file, modified)) = latest else {
        return LoopHeartbeatSummary {
            status: "missing".to_string(),
            stale: true,
            stale_after_minutes,
            latest_path: None,
            latest_modified_at: None,
            latest_age_seconds: None,
            payload: Value::Null,
        };
    };

    let age_seconds = SystemTime::now()
        .duration_since(modified)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let stale = age_seconds > stale_after_minutes.saturating_mul(60);
    let payload = read_json_value(&inventory.archive_root.join(&file.path)).unwrap_or(Value::Null);
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or(if stale { "stale" } else { "ok" })
        .to_string();

    LoopHeartbeatSummary {
        status,
        stale,
        stale_after_minutes,
        latest_path: Some(file.path.clone()),
        latest_modified_at: file.modified_at.clone(),
        latest_age_seconds: Some(age_seconds),
        payload,
    }
}

fn retention_summary(inventory: &LoopInventory, retention_days: u64) -> LoopRetentionSummary {
    LoopRetentionSummary {
        retention_days,
        candidate_count: inventory.retention_candidates.len(),
        candidate_bytes: inventory
            .retention_candidates
            .iter()
            .map(|file| file.size_bytes)
            .sum(),
        candidates: inventory
            .retention_candidates
            .iter()
            .map(|file| file.path.clone())
            .collect(),
    }
}

fn top_level_archives(root: &Path) -> Result<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for entry in read_dir_sorted(root)? {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() || is_archive_file(&path) {
            archives.push(path);
        }
    }
    Ok(archives)
}

fn recursive_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    recursive_files_into(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn recursive_files_into(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in read_dir_sorted(path)? {
        let child = entry.path();
        if child.is_dir() {
            recursive_files_into(&child, files)?;
        } else if child.is_file() {
            files.push(child);
        }
    }
    Ok(())
}

fn read_dir_sorted(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| io_error("read directory", path, e))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| io_error("list directory", path, e))?;
    entries.sort_by_key(|entry| entry.path());
    Ok(entries)
}

fn classify_file(path: &Path) -> &'static str {
    let name = file_name_lowercase(path);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if (name.contains("heartbeat") || name == "status.json") && extension == "json" {
        return "heartbeat";
    }
    if extension == "patch" || extension == "diff" {
        return "patch";
    }
    if name.contains("failure") || name.contains("error") || name.contains("failed") {
        return "reviewer_failure";
    }
    if name.contains("report") || name.contains("summary") || name.contains("gap") {
        return "report";
    }
    if is_archive_file(path) {
        return "archive";
    }
    "file"
}

fn is_archive_file(path: &Path) -> bool {
    let name = file_name_lowercase(path);
    name.ends_with(".zip")
        || name.ends_with(".tar")
        || name.ends_with(".tar.gz")
        || name.ends_with(".tgz")
        || name.ends_with(".tar.zst")
}

fn patch_fingerprint(path: &Path, size_bytes: u64) -> Result<String> {
    let content = fs::read(path).map_err(|e| io_error("read patch", path, e))?;
    let digest = format!("{:x}", Sha256::digest(&content));
    Ok(format!("{}:{size_bytes}", &digest[..16]))
}

fn patch_rank(path: &Path, size_bytes: u64) -> i64 {
    let name = file_name_lowercase(path);
    let mut rank = i64::try_from(size_bytes.min(10_000_000) / 1024).unwrap_or(0);
    if name.contains("candidate") || name.contains("apply") {
        rank += 50;
    }
    if name.contains("review") {
        rank += 25;
    }
    if name.contains("failed") || name.contains("reject") {
        rank -= 50;
    }
    rank
}

fn patch_labels(path: &Path) -> Vec<String> {
    let name = file_name_lowercase(path);
    let mut labels = Vec::new();
    for label in ["candidate", "review", "apply", "failed", "reject"] {
        if name.contains(label) {
            labels.push(label.to_string());
        }
    }
    labels
}

fn reviewer_failure_reason(path: &Path) -> String {
    read_json_value(path)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "reviewer failure artifact".to_string())
}

fn read_json_value(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).map_err(|e| io_error("read json", path, e))?;
    serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse JSON file {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })
}

fn is_older_than(modified: Option<SystemTime>, minutes: u64) -> bool {
    modified
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|duration| duration.as_secs() > minutes.saturating_mul(60))
        .unwrap_or(false)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn file_name_lowercase(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn system_time_to_rfc3339(time: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339()
}

fn io_error(action: &str, path: &Path, error: std::io::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("{action} {}", path.display())),
    )
}

#[cfg(unix)]
fn disk_summary(path: &Path) -> LoopDiskSummary {
    let c_path = match std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        Ok(path) => path,
        Err(_) => return unavailable_disk_summary(path, "path contains an interior NUL byte"),
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return unavailable_disk_summary(path, "statvfs failed");
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = u128::from(stat.f_frsize.max(1));
    let total = u64::try_from(u128::from(stat.f_blocks).saturating_mul(block_size)).ok();
    let available = u64::try_from(u128::from(stat.f_bavail).saturating_mul(block_size)).ok();
    let used_percent = match (total, available) {
        (Some(total), Some(available)) if total > 0 => {
            Some(((total.saturating_sub(available)) as f64 / total as f64) * 100.0)
        }
        _ => None,
    };
    let warning = match (available, total) {
        (Some(available), Some(total)) if total > 0 && available < total / 10 => {
            Some("archive filesystem has less than 10% free space".to_string())
        }
        (Some(available), _) if available < 5 * 1024 * 1024 * 1024 => {
            Some("archive filesystem has less than 5 GiB free space".to_string())
        }
        _ => None,
    };
    LoopDiskSummary {
        path: path.display().to_string(),
        available_bytes: available,
        total_bytes: total,
        used_percent,
        status: if warning.is_some() { "warning" } else { "ok" }.to_string(),
        warning,
    }
}

#[cfg(not(unix))]
fn disk_summary(path: &Path) -> LoopDiskSummary {
    unavailable_disk_summary(path, "disk probing is not implemented for this platform")
}

fn unavailable_disk_summary(path: &Path, warning: &str) -> LoopDiskSummary {
    LoopDiskSummary {
        path: path.display().to_string(),
        available_bytes: None,
        total_bytes: None,
        used_percent: None,
        status: "unknown".to_string(),
        warning: Some(warning.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::test_support::with_isolated_home;
    use std::time::UNIX_EPOCH;

    #[test]
    fn loop_sync_indexes_archive_health_and_patch_candidates_without_wpgym_assumptions() {
        with_isolated_home(|home| {
            let archive_root = home.path().join("loop-archives");
            let archive = archive_root.join("run-001");
            fs::create_dir_all(&archive).expect("archive dir");
            fs::write(
                archive.join("heartbeat.json"),
                r#"{"status":"ok","updated_at":"2026-05-27T00:00:00Z"}"#,
            )
            .expect("heartbeat");
            fs::write(archive.join("integrated-gap-report.md"), "# Report").expect("report");
            fs::write(
                archive.join("reviewer-candidate.patch"),
                "diff --git a/a b/a\n",
            )
            .expect("patch");
            fs::write(
                archive.join("reviewer-failure.json"),
                r#"{"error":"model returned empty patch"}"#,
            )
            .expect("failure");

            let (output, exit_code) = loop_sync(RunsLoopSyncArgs {
                archive_root: archive_root.clone(),
                component_id: Some("sample-component".to_string()),
                rig: Some("continuous-loop".to_string()),
                labels: vec!["rl".to_string()],
                stale_after_minutes: 120,
                retention_days: 30,
                patch_limit: 5,
                dry_run: false,
            })
            .expect("loop sync");

            assert_eq!(exit_code, 0);
            let RunsOutput::LoopSync(output) = output else {
                panic!("expected loop sync output");
            };

            assert!(!output.dry_run);
            assert!(output.run_id.is_some());
            assert_eq!(output.triage.archive_count, 1);
            assert_eq!(output.triage.report_count, 1);
            assert_eq!(output.triage.patch_count, 1);
            assert_eq!(output.triage.reviewer_failure_count, 1);
            assert_eq!(output.triage.patch_candidates.len(), 1);
            assert!(output
                .triage
                .patch_candidates
                .first()
                .expect("candidate")
                .labels
                .contains(&"candidate".to_string()));
            assert_eq!(output.triage.heartbeat.status, "ok");
            assert!(output.synced_artifacts.len() >= 4);
        });
    }

    #[test]
    fn loop_sync_dry_run_does_not_create_observation_run() {
        with_isolated_home(|home| {
            let archive_root = home.path().join("loop-archives");
            fs::create_dir_all(archive_root.join("archive")).expect("archive dir");

            let (output, _) = loop_sync(RunsLoopSyncArgs {
                archive_root,
                component_id: None,
                rig: None,
                labels: Vec::new(),
                stale_after_minutes: 1,
                retention_days: 30,
                patch_limit: 20,
                dry_run: true,
            })
            .expect("dry run");

            let RunsOutput::LoopSync(output) = output else {
                panic!("expected loop sync output");
            };
            assert!(output.dry_run);
            assert!(output.run_id.is_none());
            assert!(output.synced_artifacts.is_empty());
        });
    }

    #[test]
    fn patch_candidates_include_duplicate_counts() {
        with_isolated_home(|home| {
            let archive_root = home.path().join("loop-archives");
            let archive = archive_root.join("archive");
            fs::create_dir_all(&archive).expect("archive dir");
            fs::write(archive.join("first.patch"), "same diff").expect("patch");
            fs::write(archive.join("second.patch"), "same diff").expect("patch");

            let inventory = inventory_loop_archives(&RunsLoopSyncArgs {
                archive_root,
                component_id: None,
                rig: None,
                labels: Vec::new(),
                stale_after_minutes: 120,
                retention_days: 30,
                patch_limit: 20,
                dry_run: true,
            })
            .expect("inventory");

            assert_eq!(inventory.patch_candidates.len(), 2);
            assert!(inventory
                .patch_candidates
                .iter()
                .all(|candidate| candidate.duplicate_count == 1));
        });
    }

    #[test]
    fn unix_epoch_formats_as_rfc3339() {
        assert_eq!(
            system_time_to_rfc3339(UNIX_EPOCH),
            "1970-01-01T00:00:00+00:00"
        );
    }
}
