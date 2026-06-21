use std::path::PathBuf;

use homeboy::core::observation::runs_service::{
    self, PersistedArtifactCleanupOptions, RunnerDownloadCleanupOptions,
};
use homeboy::core::observation::ArtifactRecord;

use super::{
    CmdResult, RunsArtifactCleanupDownloadsArgs, RunsArtifactCleanupDownloadsOutput,
    RunsArtifactCleanupPersistedArgs, RunsArtifactCleanupPersistedOutput, RunsArtifactGetOutput,
    RunsOutput,
};

pub fn get(artifact: ArtifactRecord, output: Option<PathBuf>) -> CmdResult<RunsOutput> {
    let download = runs_service::download_remote_artifact(artifact, output)?;
    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: download.run_id,
            artifact_id: download.artifact_id,
            output_path: download.output_path.display().to_string(),
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
            artifact_ref: download.artifact_ref,
        }),
        0,
    ))
}

pub fn cleanup_downloads(args: RunsArtifactCleanupDownloadsArgs) -> CmdResult<RunsOutput> {
    let outcome = runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
        apply: args.apply,
        runner: args.runner,
        run_id: args.run_id,
    })?;

    Ok((
        RunsOutput::ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput {
            command: "runs.artifact.cleanup-downloads",
            dry_run: outcome.dry_run,
            root: outcome.root.display().to_string(),
            removed: outcome.removed,
            file_count: outcome.file_count,
            directory_count: outcome.directory_count,
            size_bytes: outcome.size_bytes,
            paths: outcome.paths,
        }),
        0,
    ))
}

pub fn cleanup_persisted(args: RunsArtifactCleanupPersistedArgs) -> CmdResult<RunsOutput> {
    let outcome = runs_service::cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
        apply: args.apply,
        older_than_days: args.older_than_days,
        run_id: args.run_id,
        kind: args.kind,
        artifact_type: args.artifact_type,
        run_kind: args.run_kind,
        component_id: args.component_id,
        limit: args.limit,
    })?;

    Ok((
        RunsOutput::ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput {
            command: "runs.artifact.cleanup-persisted",
            dry_run: outcome.dry_run,
            artifact_root: outcome.artifact_root.display().to_string(),
            older_than_days: outcome.older_than_days,
            inspected_count: outcome.inspected_count,
            planned_record_count: outcome.planned_record_count,
            planned_file_count: outcome.planned_file_count,
            planned_directory_count: outcome.planned_directory_count,
            planned_size_bytes: outcome.planned_size_bytes,
            removed_record_count: outcome.removed_record_count,
            removed_file_count: outcome.removed_file_count,
            removed_directory_count: outcome.removed_directory_count,
            removed_size_bytes: outcome.removed_size_bytes,
            skipped_count: outcome.skipped_count,
            rows: outcome.rows,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::observation::{NewRunRecord, ObservationStore};
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
