use super::*;

pub fn cleanup_persisted_artifacts(
    options: PersistedArtifactCleanupOptions,
) -> Result<PersistedArtifactCleanupOutcome> {
    if options.older_than_days < 0 {
        return Err(Error::validation_invalid_argument(
            "older_than_days",
            "--older-than-days must be zero or greater",
            Some(options.older_than_days.to_string()),
            None,
        ));
    }

    let artifact_root = crate::core::artifacts::root()?;
    let created_before = (Utc::now() - Duration::days(options.older_than_days)).to_rfc3339();
    let store = ObservationStore::open_initialized()?;
    let candidates = store.list_artifact_cleanup_candidates(ArtifactCleanupFilter {
        created_before: Some(created_before),
        run_id: options.run_id.clone(),
        kind: options.kind.clone(),
        artifact_type: options.artifact_type.clone(),
        run_kind: options.run_kind.clone(),
        component_id: options.component_id.clone(),
        limit: Some(options.limit),
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
        let mut row =
            classify_persisted_artifact(candidate, &artifact_root, options.terminal_only)?;
        if row.action == "remove" {
            planned_record_count += 1;
            planned_size_bytes += row.size_bytes;
            match row.artifact_type.as_str() {
                "directory" => planned_directory_count += 1,
                _ => planned_file_count += usize::from(row.exists),
            }
            if options.apply {
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

    Ok(PersistedArtifactCleanupOutcome {
        dry_run: !options.apply,
        artifact_root,
        older_than_days: options.older_than_days,
        totals: CleanupSizeTotals {
            inspected_count: candidates.len(),
            planned_size_bytes,
            removed_size_bytes,
        },
        planned_record_count,
        planned_file_count,
        planned_directory_count,
        removed_record_count,
        removed_file_count,
        removed_directory_count,
        skipped_count,
        rows,
    })
}

fn classify_persisted_artifact(
    candidate: &ArtifactCleanupCandidateRecord,
    artifact_root: &Path,
    terminal_only: bool,
) -> Result<PersistedArtifactCleanupRow> {
    let artifact = &candidate.artifact;
    let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
    let mut exists = false;
    let mut size_bytes = 0;
    let (action, reason) = if terminal_only
        && !RunStatus::from_label(&candidate.run_status).is_some_and(RunStatus::is_terminal)
    {
        (
            "skip",
            "owning run is active or has an unknown lifecycle state",
        )
    } else if artifact.artifact_type == "url"
        || crate::core::runners::is_remote_runner_artifact_path(&artifact.path)
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

    Ok(PersistedArtifactCleanupRow {
        artifact_id: artifact.id.clone(),
        run_id: artifact.run_id.clone(),
        run_kind: candidate.run_kind.clone(),
        run_status: candidate.run_status.clone(),
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
) -> Result<()> {
    let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
    if let Some(metadata) = symlink_metadata_if_exists(&path)? {
        if metadata.file_type().is_symlink() || !path_is_within_root(&path, artifact_root) {
            return Err(Error::validation_invalid_argument(
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

fn symlink_metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::internal_io(
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

fn path_size_bytes(path: &Path, metadata: &fs::Metadata) -> Result<u64> {
    if metadata.is_dir() {
        let mut total = 0;
        for entry in
            fs::read_dir(path).map_err(|err| persisted_artifact_read_dir_error(path, err))?
        {
            let entry = entry.map_err(|err| persisted_artifact_read_dir_error(path, err))?;
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
                Error::internal_io(
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

fn persisted_artifact_read_dir_error(path: &Path, err: io::Error) -> Error {
    Error::internal_io(
        err.to_string(),
        Some(format!(
            "read persisted artifact directory {}",
            path.display()
        )),
    )
}

fn persisted_artifact_remove_error(path: &Path, err: io::Error) -> Error {
    Error::internal_io(
        err.to_string(),
        Some(format!("remove persisted artifact {}", path.display())),
    )
}
