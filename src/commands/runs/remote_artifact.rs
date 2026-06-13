use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use chrono::{Duration, Utc};
use homeboy::core::execution_contract::EXECUTION_CONTRACT;
use homeboy::core::observation::{ArtifactCleanupFilter, ArtifactRecord, ObservationStore};
use homeboy::core::runner;

use super::{
    CmdResult, RunsArtifactCleanupDownloadsArgs, RunsArtifactCleanupDownloadsOutput,
    RunsArtifactCleanupPersistedArgs, RunsArtifactCleanupPersistedOutput,
    RunsArtifactCleanupPersistedRow, RunsArtifactGetOutput, RunsOutput,
};

pub fn is_remote_artifact(artifact: &ArtifactRecord) -> bool {
    artifact.artifact_type == "remote_file"
        || runner::is_remote_runner_artifact_path(&artifact.path)
}

pub fn get(artifact: ArtifactRecord, output: Option<PathBuf>) -> CmdResult<RunsOutput> {
    let download = runner::download_remote_artifact(&artifact.path, output)?;
    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: artifact.run_id,
            artifact_id: artifact.id,
            output_path: download.output_path.display().to_string(),
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
        }),
        0,
    ))
}

pub fn cleanup_downloads(args: RunsArtifactCleanupDownloadsArgs) -> CmdResult<RunsOutput> {
    if args.run_id.is_some() && args.runner.is_none() {
        return Err(crate::core::Error::validation_invalid_argument(
            "run_id",
            "--run-id requires --runner so cleanup stays inside one runner cache",
            args.run_id,
            None,
        ));
    }

    let root = runner_download_root(args.runner.as_deref(), args.run_id.as_deref())?;
    let plan = plan_runner_download_cleanup(&root)?;
    if args.apply && root.exists() {
        remove_runner_download_root(&root)?;
    }

    Ok((
        RunsOutput::ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput {
            command: "runs.artifact.cleanup-downloads",
            dry_run: !args.apply,
            root: root.display().to_string(),
            removed: args.apply && !root.exists(),
            file_count: plan.file_count,
            directory_count: plan.directory_count,
            size_bytes: plan.size_bytes,
            paths: plan.paths,
        }),
        0,
    ))
}

pub fn cleanup_persisted(args: RunsArtifactCleanupPersistedArgs) -> CmdResult<RunsOutput> {
    if args.older_than_days < 0 {
        return Err(crate::core::Error::validation_invalid_argument(
            "older_than_days",
            "--older-than-days must be zero or greater",
            Some(args.older_than_days.to_string()),
            None,
        ));
    }

    let artifact_root = homeboy::core::artifact_root()?;
    let created_before = (Utc::now() - Duration::days(args.older_than_days)).to_rfc3339();
    let store = ObservationStore::open_initialized()?;
    let candidates = store.list_artifact_cleanup_candidates(ArtifactCleanupFilter {
        created_before: Some(created_before),
        run_id: args.run_id.clone(),
        kind: args.kind.clone(),
        artifact_type: args.artifact_type.clone(),
        run_kind: args.run_kind.clone(),
        component_id: args.component_id.clone(),
        limit: Some(args.limit),
    })?;

    let mut rows = Vec::new();
    let mut planned_record_count = 0;
    let mut planned_file_count = 0;
    let mut planned_directory_count = 0;
    let mut planned_size_bytes = 0;
    let mut removed_record_count = 0;
    let mut removed_file_count = 0;
    let mut removed_directory_count = 0;
    let mut removed_size_bytes = 0;
    let mut skipped_count = 0;

    for candidate in candidates.iter() {
        let mut row = classify_persisted_artifact(candidate, &artifact_root)?;
        if row.action == "remove" {
            planned_record_count += 1;
            planned_size_bytes += row.size_bytes;
            match row.artifact_type.as_str() {
                "directory" => planned_directory_count += 1,
                _ => planned_file_count += usize::from(row.exists),
            }
            if args.apply {
                apply_persisted_artifact_cleanup(&store, &candidate.artifact, &artifact_root)?;
                removed_record_count += 1;
                removed_size_bytes += row.size_bytes;
                match row.artifact_type.as_str() {
                    "directory" => removed_directory_count += 1,
                    _ => removed_file_count += usize::from(row.exists),
                }
                row.action = "removed".to_string();
            }
        } else {
            skipped_count += 1;
        }
        rows.push(row);
    }

    Ok((
        RunsOutput::ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput {
            command: "runs.artifact.cleanup-persisted",
            dry_run: !args.apply,
            artifact_root: artifact_root.display().to_string(),
            older_than_days: args.older_than_days,
            inspected_count: candidates.len(),
            planned_record_count,
            planned_file_count,
            planned_directory_count,
            planned_size_bytes,
            removed_record_count,
            removed_file_count,
            removed_directory_count,
            removed_size_bytes,
            skipped_count,
            rows,
        }),
        0,
    ))
}

fn classify_persisted_artifact(
    candidate: &homeboy::core::observation::ArtifactCleanupCandidateRecord,
    artifact_root: &Path,
) -> crate::core::Result<RunsArtifactCleanupPersistedRow> {
    let artifact = &candidate.artifact;
    let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
    let mut exists = false;
    let mut size_bytes = 0;
    let (action, reason) = if artifact.artifact_type == "url"
        || runner::is_remote_runner_artifact_path(&artifact.path)
        || EXECUTION_CONTRACT
            .artifacts
            .is_metadata_only_ref(&artifact.path)
    {
        ("skip", "artifact is not local persisted bytes")
    } else if let Some(metadata) = symlink_metadata_if_exists(&path)? {
        exists = true;
        if !path_is_within_root(&path, artifact_root) {
            ("skip", "existing artifact path is outside artifact root")
        } else if metadata.file_type().is_symlink() {
            ("skip", "artifact path is a symlink")
        } else {
            size_bytes = path_size_bytes(&path, &metadata)?;
            ("remove", "artifact bytes and DB row are eligible")
        }
    } else {
        ("remove", "artifact bytes are missing; DB row is stale")
    };

    Ok(RunsArtifactCleanupPersistedRow {
        artifact_id: artifact.id.clone(),
        run_id: artifact.run_id.clone(),
        run_kind: candidate.run_kind.clone(),
        component_id: candidate.component_id.clone(),
        kind: artifact.kind.clone(),
        artifact_type: artifact.artifact_type.clone(),
        path: artifact.path.clone(),
        created_at: artifact.created_at.clone(),
        exists,
        action: action.to_string(),
        reason: reason.to_string(),
        size_bytes,
    })
}

fn apply_persisted_artifact_cleanup(
    store: &ObservationStore,
    artifact: &ArtifactRecord,
    artifact_root: &Path,
) -> crate::core::Result<()> {
    let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
    if let Some(metadata) = symlink_metadata_if_exists(&path)? {
        if metadata.file_type().is_symlink() || !path_is_within_root(&path, artifact_root) {
            return Err(crate::core::Error::validation_invalid_argument(
                "path",
                "artifact path failed cleanup safety revalidation",
                Some(path.display().to_string()),
                None,
            ));
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(|err| persisted_artifact_remove_error(&path, err))?;
        } else {
            fs::remove_file(&path).map_err(|err| persisted_artifact_remove_error(&path, err))?;
        }
    }
    store.delete_artifact_record(&artifact.id)?;
    Ok(())
}

fn persisted_artifact_path_from_record(artifact_root: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        artifact_root.join(path)
    }
}

fn symlink_metadata_if_exists(path: &Path) -> crate::core::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("read persisted artifact {}", path.display())),
        )),
    }
}

fn path_is_within_root(path: &Path, artifact_root: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    let root = fs::canonicalize(artifact_root).unwrap_or_else(|_| artifact_root.to_path_buf());
    let candidate = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    candidate.starts_with(root)
}

fn path_size_bytes(path: &Path, metadata: &fs::Metadata) -> crate::core::Result<u64> {
    if metadata.is_dir() {
        let mut total = 0;
        for entry in
            fs::read_dir(path).map_err(|err| persisted_artifact_read_dir_error(path, err))?
        {
            let entry = entry.map_err(|err| persisted_artifact_read_dir_error(path, err))?;
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
                crate::core::Error::internal_io(
                    err.to_string(),
                    Some(format!("read persisted artifact {}", entry_path.display())),
                )
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            total += path_size_bytes(&entry_path, &metadata)?;
        }
        Ok(total)
    } else {
        Ok(metadata.len())
    }
}

fn persisted_artifact_read_dir_error(path: &Path, err: io::Error) -> crate::core::Error {
    crate::core::Error::internal_io(
        err.to_string(),
        Some(format!(
            "read persisted artifact directory {}",
            path.display()
        )),
    )
}

fn persisted_artifact_remove_error(path: &Path, err: io::Error) -> crate::core::Error {
    crate::core::Error::internal_io(
        err.to_string(),
        Some(format!("remove persisted artifact {}", path.display())),
    )
}

#[derive(Debug, Default)]
struct RunnerDownloadCleanupPreview {
    file_count: usize,
    directory_count: usize,
    size_bytes: u64,
    paths: Vec<String>,
}

fn runner_download_root(
    runner: Option<&str>,
    run_id: Option<&str>,
) -> crate::core::Result<PathBuf> {
    let mut root = homeboy::core::artifact_root()?.join("runner");
    if let Some(runner) = cleanup_path_component("runner", runner)? {
        root = root.join(runner);
    }
    if let Some(run_id) = cleanup_path_component("run_id", run_id)? {
        root = root.join(run_id);
    }
    Ok(root)
}

fn cleanup_path_component(name: &str, value: Option<&str>) -> crate::core::Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(crate::core::Error::validation_invalid_argument(
            name,
            format!("{name} must be a single path component"),
            Some(value.to_string()),
            None,
        ));
    }
    Ok(Some(value.to_string()))
}

fn plan_runner_download_cleanup(root: &Path) -> crate::core::Result<RunnerDownloadCleanupPreview> {
    let mut plan = RunnerDownloadCleanupPreview::default();
    if !root.exists() {
        return Ok(plan);
    }

    let metadata = fs::symlink_metadata(root).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("read runner artifact cache {}", root.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(crate::core::Error::validation_invalid_argument(
            "artifact_root",
            format!(
                "runner artifact cache root must be a real directory: {}",
                root.display()
            ),
            Some(root.display().to_string()),
            None,
        ));
    }

    collect_runner_download_cleanup(root, root, &mut plan)?;
    plan.paths.sort();
    Ok(plan)
}

fn collect_runner_download_cleanup(
    root: &Path,
    path: &Path,
    plan: &mut RunnerDownloadCleanupPreview,
) -> crate::core::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!(
                "read runner artifact cache entry {}",
                path.display()
            )),
        )
    })?;

    if path != root {
        plan.paths.push(relative_cleanup_path(root, path));
    }

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        plan.directory_count += usize::from(path != root);
        for entry in fs::read_dir(path).map_err(|err| runner_cache_directory_error(path, err))? {
            let entry = entry.map_err(|err| runner_cache_directory_error(path, err))?;
            collect_runner_download_cleanup(root, &entry.path(), plan)?;
        }
    } else {
        plan.file_count += 1;
        plan.size_bytes += metadata.len();
    }

    Ok(())
}

fn runner_cache_directory_error(path: &Path, err: io::Error) -> crate::core::Error {
    crate::core::Error::internal_io(
        err.to_string(),
        Some(format!(
            "read runner artifact cache directory {}",
            path.display()
        )),
    )
}

fn remove_runner_download_root(root: &Path) -> crate::core::Result<()> {
    let metadata = fs::symlink_metadata(root).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("read runner artifact cache {}", root.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(crate::core::Error::validation_invalid_argument(
            "artifact_root",
            format!(
                "runner artifact cache root must be a real directory: {}",
                root.display()
            ),
            Some(root.display().to_string()),
            None,
        ));
    }
    fs::remove_dir_all(root).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("remove runner artifact cache {}", root.display())),
        )
    })
}

fn relative_cleanup_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::observation::NewRunRecord;
    use homeboy::test_support::with_isolated_home;

    use super::*;

    fn artifact_root_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn cleanup_downloads_plans_and_removes_runner_cache() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));

            let run_dir = artifact_root.join("runner").join("local").join("run-1");
            fs::create_dir_all(&run_dir).expect("run dir");
            fs::write(run_dir.join("trace.zip"), b"trace").expect("trace");
            fs::write(run_dir.join("report.json"), b"{}").expect("report");

            let dry = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: false,
                runner: Some("local".to_string()),
                run_id: Some("run-1".to_string()),
            })
            .expect("dry-run")
            .0;
            let RunsOutput::ArtifactCleanupDownloads(dry) = dry else {
                panic!("unexpected output");
            };
            assert!(dry.dry_run);
            assert!(!dry.removed);
            assert_eq!(dry.file_count, 2);
            assert_eq!(dry.directory_count, 0);
            assert_eq!(dry.size_bytes, 7);
            assert!(run_dir.exists());

            let applied = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: true,
                runner: Some("local".to_string()),
                run_id: Some("run-1".to_string()),
            })
            .expect("apply")
            .0;
            let RunsOutput::ArtifactCleanupDownloads(applied) = applied else {
                panic!("unexpected output");
            };
            assert!(!applied.dry_run);
            assert!(applied.removed);
            assert_eq!(applied.file_count, 2);
            assert_eq!(applied.size_bytes, 7);
            assert!(!run_dir.exists());
        });
    }

    #[test]
    fn cleanup_persisted_plans_and_removes_local_artifacts() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("bench")
                        .component_id("homeboy")
                        .command("homeboy bench")
                        .metadata(serde_json::json!({ "test": true }))
                        .build(),
                )
                .expect("run");
            let source = home.path().join("bench.json");
            fs::write(&source, b"bench").expect("source");
            let artifact = store
                .record_artifact(&run.id, "summary", &source)
                .expect("artifact");
            let stored_path = PathBuf::from(&artifact.path);
            assert!(stored_path.exists());

            let dry = cleanup_persisted(RunsArtifactCleanupPersistedArgs {
                apply: false,
                older_than_days: 0,
                run_id: Some(run.id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
            })
            .expect("dry-run")
            .0;
            let RunsOutput::ArtifactCleanupPersisted(dry) = dry else {
                panic!("unexpected output");
            };
            assert!(dry.dry_run);
            assert_eq!(dry.planned_record_count, 1);
            assert_eq!(dry.planned_file_count, 1);
            assert_eq!(dry.planned_size_bytes, 5);
            assert!(stored_path.exists());
            assert!(store.get_artifact(&artifact.id).expect("get").is_some());

            let applied = cleanup_persisted(RunsArtifactCleanupPersistedArgs {
                apply: true,
                older_than_days: 0,
                run_id: Some(run.id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
            })
            .expect("apply")
            .0;
            let RunsOutput::ArtifactCleanupPersisted(applied) = applied else {
                panic!("unexpected output");
            };
            assert!(!applied.dry_run);
            assert_eq!(applied.removed_record_count, 1);
            assert_eq!(applied.removed_file_count, 1);
            assert!(!stored_path.exists());
            assert!(store.get_artifact(&artifact.id).expect("get").is_none());
        });
    }

    #[test]
    fn cleanup_downloads_requires_runner_for_run_filter() {
        let result = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
            apply: false,
            runner: None,
            run_id: Some("run-1".to_string()),
        });
        let Err(err) = result else {
            panic!("missing runner should fail");
        };

        assert!(err.to_string().contains("--run-id requires --runner"));
    }

    #[test]
    fn cleanup_downloads_rejects_path_traversal_filters() {
        for (runner, run_id) in [
            (Some("../outside".to_string()), None),
            (Some("local".to_string()), Some("../outside".to_string())),
            (Some("/tmp/outside".to_string()), None),
        ] {
            let result = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: false,
                runner,
                run_id,
            });
            let Err(err) = result else {
                panic!("path traversal should fail");
            };

            assert!(err.to_string().contains("single path component"));
        }
    }
}
