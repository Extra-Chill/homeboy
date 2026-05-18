use std::fs;
use std::path::{Path, PathBuf};

use homeboy::core::observation::ArtifactRecord;
use homeboy::core::runner;

use super::{
    CmdResult, RunsArtifactCleanupDownloadsArgs, RunsArtifactCleanupDownloadsOutput,
    RunsArtifactGetOutput, RunsOutput,
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

#[derive(Debug, Default)]
struct RunnerDownloadCleanupPlan {
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

fn plan_runner_download_cleanup(root: &Path) -> crate::core::Result<RunnerDownloadCleanupPlan> {
    let mut plan = RunnerDownloadCleanupPlan::default();
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
    plan: &mut RunnerDownloadCleanupPlan,
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
        for entry in fs::read_dir(path).map_err(|err| {
            crate::core::Error::internal_io(
                err.to_string(),
                Some(format!(
                    "read runner artifact cache directory {}",
                    path.display()
                )),
            )
        })? {
            let entry = entry.map_err(|err| {
                crate::core::Error::internal_io(
                    err.to_string(),
                    Some(format!(
                        "read runner artifact cache directory {}",
                        path.display()
                    )),
                )
            })?;
            collect_runner_download_cleanup(root, &entry.path(), plan)?;
        }
    } else {
        plan.file_count += 1;
        plan.size_bytes += metadata.len();
    }

    Ok(())
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

    use super::*;

    #[test]
    fn cleanup_downloads_plans_and_removes_runner_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("artifacts");
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

        homeboy::core::set_artifact_root_override(None);
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
