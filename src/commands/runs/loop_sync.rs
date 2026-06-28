use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use homeboy::core::observation::{persist_loop_inventory_run, ArtifactRecord, NewRunRecord};
use homeboy::core::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

mod types;

use super::{disk, CmdResult, RunsOutput};
use types::*;
pub use types::{RunsLoopSyncArgs, RunsLoopSyncOutput};

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
    let indexed_artifacts = files_for_artifact_index(inventory)
        .into_iter()
        .map(|file| {
            let kind = classify_file(&file).to_string();
            (file, kind)
        })
        .collect::<Vec<_>>();

    let (run_id, artifacts) = persist_loop_inventory_run(
        loop_run_record(inventory, triage, args),
        &inventory.archives,
        &indexed_artifacts,
    )?;
    Ok((Some(run_id), artifacts))
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
        disk: disk::disk_budget(
            &inventory.archive_root,
            "archive",
            "disk budget probing is not implemented for this platform",
        ),
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
