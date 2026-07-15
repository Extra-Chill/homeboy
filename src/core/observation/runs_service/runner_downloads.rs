use super::*;

pub fn cleanup_runner_downloads(
    options: RunnerDownloadCleanupOptions,
) -> Result<RunnerDownloadCleanupOutcome> {
    if options.run_id.is_some() && options.runner.is_none() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "--run-id requires --runner so cleanup stays inside one runner cache",
            options.run_id,
            None,
        ));
    }

    let root = runner_download_root(options.runner.as_deref(), options.run_id.as_deref())?;
    let plan = plan_runner_download_cleanup(&root)?;
    if options.apply && root.exists() {
        remove_runner_download_root(&root)?;
    }

    Ok(RunnerDownloadCleanupOutcome {
        dry_run: !options.apply,
        removed: options.apply && !root.exists(),
        root,
        file_count: plan.file_count,
        directory_count: plan.directory_count,
        size_bytes: plan.size_bytes,
        paths: plan.paths,
    })
}

fn runner_download_root(runner: Option<&str>, run_id: Option<&str>) -> Result<PathBuf> {
    let mut root = crate::core::artifacts::root()?.join("runner");
    if let Some(runner) = cleanup_path_component("runner", runner)? {
        root = root.join(runner);
    }
    if let Some(run_id) = cleanup_path_component("run_id", run_id)? {
        root = root.join(run_id);
    }
    Ok(root)
}

fn cleanup_path_component(name: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must be a single path component"),
            Some(value.to_string()),
            None,
        ));
    }
    Ok(Some(value.to_string()))
}

fn plan_runner_download_cleanup(root: &Path) -> Result<RunnerDownloadCleanupPreview> {
    let mut plan = RunnerDownloadCleanupPreview::default();
    if !root.exists() {
        return Ok(plan);
    }

    let metadata = fs::symlink_metadata(root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read runner artifact cache {}", root.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
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
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        Error::internal_io(
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

fn runner_cache_directory_error(path: &Path, err: io::Error) -> Error {
    Error::internal_io(
        err.to_string(),
        Some(format!(
            "read runner artifact cache directory {}",
            path.display()
        )),
    )
}

fn remove_runner_download_root(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read runner artifact cache {}", root.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
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
        Error::internal_io(
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
