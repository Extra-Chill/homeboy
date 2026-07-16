use super::*;

pub(super) fn adopt_with_store(
    options: WorktreeAdoptOptions,
    store_dir: &Path,
) -> Result<WorktreeAdoptOutput> {
    if options.handle.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "handle",
            "Adopted workspace handle must not be empty",
            Some(options.handle),
            None,
        ));
    }
    let path = PathBuf::from(&options.path).canonicalize().map_err(|err| {
        Error::validation_invalid_argument(
            "path",
            "Adopted workspace path must exist on the controller",
            Some(format!("{} ({err})", options.path)),
            Some(vec![
                "Pass an existing local checkout or workspace path.".to_string()
            ]),
        )
    })?;
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Adopted workspace path must be a directory",
            Some(path.display().to_string()),
            None,
        ));
    }
    let record = AdoptedWorkspaceRecord {
        handle: options.handle,
        path: path.display().to_string(),
        kind: options.kind,
        provenance: options.provenance,
        created_at: chrono::Utc::now().to_rfc3339(),
        state: TaskWorktreeState::Active,
    };
    write_adopted_record(store_dir, &record)?;
    Ok(WorktreeAdoptOutput { record })
}

pub(super) fn cleanup_with_store(
    options: WorktreeCleanupOptions,
    store: &Path,
) -> Result<WorktreeCleanupOutput> {
    let mut candidates = Vec::new();
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for record in list_with_store(store)?.worktrees {
        if record.state != TaskWorktreeState::Active {
            continue;
        }
        if record.cleanup_policy == CleanupPolicy::PreserveOnFailure {
            continue;
        }
        let safety = match safety_report(&record) {
            Ok(safety) => safety,
            Err(error) => {
                skipped.push(WorktreeCleanupSkipped {
                    record,
                    safety: None,
                    reasons: vec![error.message],
                });
                continue;
            }
        };
        let branch_cleanup = branch_cleanup_report(&record)
            .unwrap_or_else(|error| branch_cleanup_unknown(&record, error.message));
        let skip_reasons = cleanup_skip_reasons(&safety, options.force);
        if !skip_reasons.is_empty() {
            skipped.push(WorktreeCleanupSkipped {
                record,
                safety: Some(safety),
                reasons: skip_reasons,
            });
            continue;
        }

        candidates.push(WorktreeCleanupCandidate {
            record: record.clone(),
            safety: safety.clone(),
            branch_cleanup: branch_cleanup.clone(),
        });

        if !options.dry_run {
            removed.push(remove_with_store(
                WorktreeRemoveOptions {
                    id: record.id,
                    force: options.force,
                    cleanup_branch: options.cleanup_branches,
                    allow_unmerged_branch: options.allow_unmerged_branches,
                },
                store,
            )?);
        }
    }
    let branch_delete_candidates = candidates
        .iter()
        .filter(|candidate| candidate.branch_cleanup.safe_delete)
        .count();
    let unmerged_branches = candidates
        .iter()
        .filter(|candidate| candidate.branch_cleanup.status == BranchCleanupStatus::Unmerged)
        .count();
    let branches_deleted = removed
        .iter()
        .filter(|output| output.branch_cleanup.deleted)
        .count();
    let counts = WorktreeCleanupCounts {
        candidates: candidates.len() + skipped.len(),
        removed: removed.len(),
        skipped: skipped.len(),
        branch_delete_candidates,
        branches_deleted,
        unmerged_branches,
    };
    Ok(WorktreeCleanupOutput {
        dry_run: options.dry_run,
        counts,
        candidates,
        removed,
        skipped,
    })
}

fn cleanup_skip_reasons(safety: &WorktreeSafetyReport, force: bool) -> Vec<String> {
    let mut reasons = Vec::new();
    if safety.primary_checkout {
        reasons.push("refuses to remove primary checkout".to_string());
    }
    if !safety.path_contained {
        reasons.push("worktree path is outside the component checkout parent".to_string());
    }
    if !force {
        if safety.dirty {
            reasons.push("dirty worktree".to_string());
        }
        if safety.unpushed_commits > 0 {
            reasons.push(format!("{} unpushed commit(s)", safety.unpushed_commits));
        }
    }
    reasons
}

pub(super) fn create_with_store(
    options: WorktreeCreateOptions,
    store_dir: &Path,
) -> Result<WorktreeCreateOutput> {
    let target = component::resolve_target(TargetSpec {
        component_id: Some(&options.component_id),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    })?;
    let source_checkout = source_checkout_for_worktree(&target)?;

    let parent = source_checkout.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source_checkout.display()
        ))
    })?;
    let id = format!("{}@{}", target.component_id, branch_slug(&options.branch));
    let worktree_path = parent.join(&id);
    if worktree_path.exists() {
        return Err(Error::validation_invalid_argument(
            "branch",
            "Task worktree path already exists",
            Some(worktree_path.display().to_string()),
            Some(vec![
                "Use a unique branch name or remove the existing task worktree".to_string(),
            ]),
        ));
    }

    let worktree_owner = ownership::owner_for_path_or_ancestor(parent)?;
    let base_ref = options.from.unwrap_or_else(|| "HEAD".to_string());
    git::run_git(
        &source_checkout,
        &[
            "worktree",
            "add",
            "-b",
            &options.branch,
            &worktree_path.to_string_lossy(),
            &base_ref,
        ],
        "git worktree add",
    )?;
    ownership::normalize_created_path(&worktree_path, worktree_owner, true, "git worktree add")?;

    let record = TaskWorktreeRecord {
        id,
        component_id: target.component_id,
        source_checkout: source_checkout.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: options.branch,
        base_ref,
        task_url: options.task_url,
        run_id: options.run_id.clone(),
        cleanup_policy: options
            .cleanup_policy
            .unwrap_or_else(|| CleanupPolicy::default_for_run(options.run_id.as_deref())),
        branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
        created_at: chrono::Utc::now().to_rfc3339(),
        state: TaskWorktreeState::Active,
    };
    write_record(store_dir, &record)?;
    Ok(WorktreeCreateOutput { record })
}

pub(super) fn list_with_store(store_dir: &Path) -> Result<WorktreeListOutput> {
    let mut worktrees = Vec::new();
    if !store_dir.exists() {
        return Ok(WorktreeListOutput { worktrees });
    }
    for entry in fs::read_dir(store_dir)
        .map_err(|err| Error::internal_io(err.to_string(), Some(store_dir.display().to_string())))?
    {
        let entry = entry.map_err(|err| Error::internal_io(err.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        worktrees.push(read_record_path(&entry.path())?);
    }
    worktrees.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WorktreeListOutput { worktrees })
}

pub(super) fn status_with_store(id: &str, store_dir: &Path) -> Result<WorktreeStatusOutput> {
    let mut record = read_record(store_dir, id)?;
    repair_record_source_checkout_if_needed(&mut record, store_dir)?;
    let safety = safety_report(&record)?;
    Ok(WorktreeStatusOutput { record, safety })
}

pub(super) fn remove_with_store(
    options: WorktreeRemoveOptions,
    store_dir: &Path,
) -> Result<WorktreeRemoveOutput> {
    let mut record = read_record(store_dir, &options.id)?;
    repair_record_source_checkout_if_needed(&mut record, store_dir)?;
    let safety = safety_report(&record)?;
    if !options.force && !safety.safe {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree is not safe to remove",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }
    if safety.primary_checkout || !safety.path_contained {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree failed hard removal safety gates",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }

    if !safety.worktree_missing {
        let mut args = vec!["worktree", "remove"];
        if options.force {
            args.push("--force");
        }
        args.push(&record.worktree_path);
        git::run_git(
            Path::new(&record.source_checkout),
            &args,
            "git worktree remove",
        )?;
    }
    let mut branch_cleanup = branch_cleanup_report(&record)
        .unwrap_or_else(|error| branch_cleanup_unknown(&record, error.message));
    if options.cleanup_branch {
        branch_cleanup =
            apply_branch_cleanup(&record, branch_cleanup, options.allow_unmerged_branch)?;
    }
    record.state = TaskWorktreeState::Removed;
    write_record(store_dir, &record)?;
    Ok(WorktreeRemoveOutput {
        record,
        safety,
        branch_cleanup,
        removed: true,
    })
}

pub(super) fn branch_cleanup_report(
    record: &TaskWorktreeRecord,
) -> Result<WorktreeBranchCleanupReport> {
    let cleanup_command = format!(
        "homeboy worktree remove {} --cleanup-branch",
        shell_arg(&record.id)
    );
    if record.branch_cleanup_intent == BranchCleanupIntent::Preserve {
        return Ok(WorktreeBranchCleanupReport {
            branch: record.branch.clone(),
            base_ref: record.base_ref.clone(),
            intent: record.branch_cleanup_intent.clone(),
            status: BranchCleanupStatus::Preserved,
            safe_delete: false,
            deleted: false,
            reason: Some("branch cleanup intent preserves this branch".to_string()),
            cleanup_command,
        });
    }
    let source = resolved_source_checkout(record)?;
    let branch = record.branch.as_str();
    let exists = git::run_git(
        &source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        "git show-ref branch",
    )
    .is_ok();
    if !exists {
        return Ok(WorktreeBranchCleanupReport {
            branch: record.branch.clone(),
            base_ref: record.base_ref.clone(),
            intent: record.branch_cleanup_intent.clone(),
            status: BranchCleanupStatus::Missing,
            safe_delete: false,
            deleted: false,
            reason: Some("local branch is already missing".to_string()),
            cleanup_command,
        });
    }
    let base_ref = branch_cleanup_base_ref(record);
    let merged = git::run_git(
        &source,
        &["merge-base", "--is-ancestor", branch, &base_ref],
        "git merge-base branch cleanup",
    )
    .is_ok();
    Ok(WorktreeBranchCleanupReport {
        branch: record.branch.clone(),
        base_ref,
        intent: record.branch_cleanup_intent.clone(),
        status: if merged {
            BranchCleanupStatus::Merged
        } else {
            BranchCleanupStatus::Unmerged
        },
        safe_delete: merged,
        deleted: false,
        reason: if merged {
            Some("branch is merged into the cleanup base ref".to_string())
        } else {
            Some("branch is not merged into the cleanup base ref".to_string())
        },
        cleanup_command,
    })
}

fn apply_branch_cleanup(
    record: &TaskWorktreeRecord,
    mut report: WorktreeBranchCleanupReport,
    allow_unmerged_branch: bool,
) -> Result<WorktreeBranchCleanupReport> {
    if report.status == BranchCleanupStatus::Missing || report.deleted {
        return Ok(report);
    }
    if !report.safe_delete && !allow_unmerged_branch {
        return Ok(report);
    }
    let source = resolved_source_checkout(record)?;
    let delete_flag = if report.safe_delete { "-d" } else { "-D" };
    git::run_git(
        &source,
        &["branch", delete_flag, &record.branch],
        "git branch delete task worktree branch",
    )?;
    report.deleted = true;
    report.status = BranchCleanupStatus::Deleted;
    report.reason = Some(if report.safe_delete {
        "merged branch deleted".to_string()
    } else {
        "unmerged branch deleted by explicit allow flag".to_string()
    });
    Ok(report)
}

fn branch_cleanup_unknown(
    record: &TaskWorktreeRecord,
    reason: String,
) -> WorktreeBranchCleanupReport {
    WorktreeBranchCleanupReport {
        branch: record.branch.clone(),
        base_ref: record.base_ref.clone(),
        intent: record.branch_cleanup_intent.clone(),
        status: BranchCleanupStatus::Unknown,
        safe_delete: false,
        deleted: false,
        reason: Some(reason),
        cleanup_command: format!(
            "homeboy worktree remove {} --cleanup-branch",
            shell_arg(&record.id)
        ),
    }
}

fn branch_cleanup_base_ref(record: &TaskWorktreeRecord) -> String {
    let trimmed = record.base_ref.trim();
    if trimmed.is_empty() || trimmed == "HEAD" {
        return "HEAD".to_string();
    }
    trimmed
        .strip_prefix("origin/")
        .unwrap_or(trimmed)
        .to_string()
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | '@' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) fn safety_report(record: &TaskWorktreeRecord) -> Result<WorktreeSafetyReport> {
    let source = resolved_source_checkout(record)?;
    let parent = source.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source.display()
        ))
    })?;
    let raw_worktree = Path::new(&record.worktree_path);
    let worktree = match raw_worktree.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            normalize_missing_path(raw_worktree)
        }
        Err(err) => {
            return Err(Error::internal_io(
                err.to_string(),
                Some(record.worktree_path.clone()),
            ))
        }
    };
    let worktree_missing = !raw_worktree.exists();
    let primary_checkout = source == worktree;
    let path_contained = worktree.starts_with(parent) && worktree != source;
    let dirty = !worktree_missing && is_dirty(&worktree)?;
    let unpushed_commits = if worktree_missing {
        0
    } else {
        unpushed_commit_count(&worktree, &record.base_ref)?
    };
    let mut reasons = Vec::new();
    if dirty {
        reasons.push("dirty worktree".to_string());
    }
    if unpushed_commits > 0 {
        reasons.push(format!("{unpushed_commits} unpushed commit(s)"));
    }
    if primary_checkout {
        reasons.push("refuses to remove primary checkout".to_string());
    }
    if !path_contained {
        reasons.push("worktree path is outside the component checkout parent".to_string());
    }
    let safe = reasons.is_empty();
    Ok(WorktreeSafetyReport {
        dirty,
        unpushed_commits,
        primary_checkout,
        path_contained,
        worktree_missing,
        safe,
        reasons,
    })
}

pub(super) fn is_dirty(path: &Path) -> Result<bool> {
    Ok(
        !git::run_git(path, &["status", "--porcelain=v1"], "git status")?
            .trim()
            .is_empty(),
    )
}

pub(super) fn unpushed_commit_count(path: &Path, base_ref: &str) -> Result<u32> {
    let upstream = git::run_git(path, &["rev-parse", "--abbrev-ref", "@{u}"], "git upstream");
    let range = if let Ok(upstream) = upstream {
        let upstream = upstream.trim();
        if upstream.is_empty() {
            format!("{base_ref}..HEAD")
        } else {
            format!("{upstream}..HEAD")
        }
    } else {
        format!("{base_ref}..HEAD")
    };
    let count = git::run_git(path, &["rev-list", "--count", &range], "git rev-list")?;
    Ok(count.trim().parse::<u32>().unwrap_or(0))
}

pub(super) fn canonical_existing_path(path: &str) -> Result<PathBuf> {
    Path::new(path)
        .canonicalize()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.to_string())))
}

fn repair_record_source_checkout_if_needed(
    record: &mut TaskWorktreeRecord,
    store_dir: &Path,
) -> Result<()> {
    if Path::new(&record.source_checkout).exists() {
        return Ok(());
    }

    let source = recovered_component_source_checkout(record)?;
    let repaired = source.to_string_lossy().to_string();
    if record.source_checkout != repaired {
        record.source_checkout = repaired;
        write_record(store_dir, record)?;
    }
    Ok(())
}

fn resolved_source_checkout(record: &TaskWorktreeRecord) -> Result<PathBuf> {
    if Path::new(&record.source_checkout).exists() {
        return canonical_existing_path(&record.source_checkout);
    }

    recovered_component_source_checkout(record)
}

fn recovered_component_source_checkout(record: &TaskWorktreeRecord) -> Result<PathBuf> {
    let target = component::resolve_target(TargetSpec {
        component_id: Some(&record.component_id),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    })
    .map_err(|error| missing_source_checkout_error(record, Some(error.message)))?;
    let source = super::queue_ops::source_checkout_for_worktree(&target)
        .map_err(|error| missing_source_checkout_error(record, Some(error.message)))?;
    let worktree = Path::new(&record.worktree_path)
        .canonicalize()
        .unwrap_or_else(|_| normalize_missing_path(Path::new(&record.worktree_path)));

    if source == worktree {
        return Err(missing_source_checkout_error(
            record,
            Some("resolved component checkout is the task worktree itself".to_string()),
        ));
    }

    Ok(source)
}

fn missing_source_checkout_error(
    record: &TaskWorktreeRecord,
    recovery_error: Option<String>,
) -> Error {
    let mut tried = vec![format!(
        "recorded source_checkout: {}",
        record.source_checkout
    )];
    if let Some(recovery_error) = recovery_error {
        tried.push(format!(
            "component checkout resolution for '{}': {recovery_error}",
            record.component_id
        ));
    } else {
        tried.push(format!(
            "component checkout resolution for '{}'",
            record.component_id
        ));
    }

    Error::validation_invalid_argument(
        "source_checkout",
        "Task worktree source checkout is missing and Homeboy could not safely recover a component checkout",
        Some(record.id.clone()),
        Some(tried),
    )
    .with_hint(format!(
        "Restore the source checkout path or update component '{}' to an existing git checkout, then retry.",
        record.component_id
    ))
    .with_hint(format!(
        "If the task worktree is intentionally gone, remove or repair the metadata record for '{}'.",
        record.id
    ))
}

pub(super) fn normalize_missing_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    parent
        .canonicalize()
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn metadata_dir() -> Result<PathBuf> {
    let observation_db = paths::observation_db()?;
    let data_root = observation_db.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "observation database path `{}` has no parent directory",
            observation_db.display()
        ))
    })?;

    Ok(data_root.join("task-worktrees"))
}

pub(super) fn adopted_metadata_dir() -> Result<PathBuf> {
    let observation_db = paths::observation_db()?;
    let data_root = observation_db.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "observation database path `{}` has no parent directory",
            observation_db.display()
        ))
    })?;

    Ok(data_root.join("adopted-workspaces"))
}

pub(super) fn record_path(store_dir: &Path, id: &str) -> PathBuf {
    store_dir.join(format!("{}.json", paths::sanitize_path_segment(id)))
}

pub(super) fn write_record(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
    let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
    fs::create_dir_all(store_dir).map_err(|err| {
        Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|err| Error::internal_json(err.to_string(), Some(record.id.clone())))?;
    let path = record_path(store_dir, &record.id);
    fs::write(&path, format!("{json}\n"))
        .map_err(|err| Error::internal_io(err.to_string(), Some(record.id.clone())))?;
    ownership::normalize_created_path(store_dir, store_owner, false, "write worktree metadata")?;
    ownership::normalize_created_path(&path, store_owner, false, "write worktree metadata")?;
    Ok(())
}

pub(super) fn write_adopted_record(
    store_dir: &Path,
    record: &AdoptedWorkspaceRecord,
) -> Result<()> {
    let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
    fs::create_dir_all(store_dir).map_err(|err| {
        Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|err| Error::internal_json(err.to_string(), Some(record.handle.clone())))?;
    let path = record_path(store_dir, &record.handle);
    fs::write(&path, format!("{json}\n"))
        .map_err(|err| Error::internal_io(err.to_string(), Some(record.handle.clone())))?;
    ownership::normalize_created_path(
        store_dir,
        store_owner,
        false,
        "write adopted workspace metadata",
    )?;
    ownership::normalize_created_path(
        &path,
        store_owner,
        false,
        "write adopted workspace metadata",
    )?;
    Ok(())
}

pub(super) fn read_record(store_dir: &Path, id: &str) -> Result<TaskWorktreeRecord> {
    read_record_path(&record_path(store_dir, id))
}

pub(super) fn read_record_path(path: &Path) -> Result<TaskWorktreeRecord> {
    let raw = fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
}

pub(super) fn read_adopted_record(
    store_dir: &Path,
    handle: &str,
) -> Result<AdoptedWorkspaceRecord> {
    let path = record_path(store_dir, handle);
    let raw = fs::read_to_string(&path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
}

pub(super) fn branch_slug(branch: &str) -> String {
    branch
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}
