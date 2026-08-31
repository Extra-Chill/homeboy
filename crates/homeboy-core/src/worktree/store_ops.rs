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

pub(super) fn import_with_store(
    options: WorktreeImportOptions,
    store_dir: &Path,
) -> Result<WorktreeImportOutput> {
    with_task_worktree_registry_write_lock(|| {
        if options.branch.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "branch",
                "Imported worktree branch must not be empty",
                None,
                None,
            ));
        }
        if options.base_ref.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "base_ref",
                "Imported worktree base ref must not be empty",
                None,
                None,
            ));
        }
        if let Some(created_at) = &options.created_at {
            chrono::DateTime::parse_from_rfc3339(created_at).map_err(|error| {
                Error::validation_invalid_argument(
                    "created_at",
                    "Imported worktree creation timestamp must be RFC 3339",
                    Some(format!("{created_at} ({error})")),
                    None,
                )
            })?;
        }
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
        let path = PathBuf::from(&options.path)
            .canonicalize()
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "path",
                    "Imported worktree path must exist on the controller",
                    Some(format!("{} ({error})", options.path)),
                    None,
                )
            })?;
        let expected_handle = format!("{}@{}", target.component_id, branch_slug(&options.branch));
        if options.handle != expected_handle {
            return Err(Error::validation_invalid_argument(
                "handle",
                "Imported worktree handle must exactly match its component and branch",
                Some(options.handle),
                Some(vec![format!("expected: {expected_handle}")]),
            ));
        }
        let expected_path = source_checkout
            .parent()
            .ok_or_else(|| Error::internal_unexpected("source checkout has no parent"))?
            .join(&expected_handle);
        if path == source_checkout || path != normalize_missing_path(&expected_path) {
            return Err(Error::validation_invalid_argument(
                "path",
                "Imported worktree must be the exact non-primary path for its native handle",
                Some(path.display().to_string()),
                Some(vec![format!("expected: {}", expected_path.display())]),
            ));
        }
        let registration = branch_worktree_registrations(&source_checkout, &options.branch)?
            .into_iter()
            .find(|registration| registration.path == path && !registration.prunable)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "path",
                    "Imported path is not the live exact Git worktree registration for its branch",
                    Some(path.display().to_string()),
                    None,
                )
            })?;
        verify_linked_worktree_identity(&source_checkout, &path, &options.branch)?;

        let identity = task_worktree_workspace_identity(&target.component_id, &expected_handle)?;
        if record_path(store_dir, &expected_handle).exists() {
            let record = read_record(store_dir, &expected_handle)?;
            let exact_replay = record.component_id == target.component_id
                && normalize_missing_path(Path::new(&record.source_checkout)) == source_checkout
                && normalize_missing_path(Path::new(&record.worktree_path)) == path
                && record.branch == options.branch
                && record.base_ref == options.base_ref
                && record.task_url == options.task_url
                && record.run_id == options.owner_run_ref
                && record.cleanup_policy == options.cleanup_policy
                && options
                    .created_at
                    .as_ref()
                    .is_none_or(|created_at| &record.created_at == created_at)
                && record.state == TaskWorktreeState::Active
                && record.terminal_disposition.is_none()
                && record.effective_workspace_identity()? == identity;
            if !exact_replay {
                return Err(Error::validation_invalid_argument(
                    "handle",
                    "Imported worktree conflicts with the existing native lifecycle record",
                    Some(expected_handle),
                    None,
                ));
            }
            return Ok(WorktreeImportOutput {
                record,
                imported: false,
            });
        }

        let record = TaskWorktreeRecord {
            id: expected_handle,
            component_id: target.component_id,
            source_checkout: source_checkout.display().to_string(),
            worktree_path: path.display().to_string(),
            branch: options.branch,
            base_ref: options.base_ref,
            workspace_identity: Some(identity),
            task_url: options.task_url,
            run_id: options.owner_run_ref,
            cleanup_policy: options.cleanup_policy,
            terminal_disposition: None,
            branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
            created_at: options
                .created_at
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            state: TaskWorktreeState::Active,
            lifecycle_revision: 0,
            terminal_workspace_authority: None,
        };
        let _ = registration;
        write_record_unlocked(store_dir, &record)?;
        Ok(WorktreeImportOutput {
            record,
            imported: true,
        })
    })
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
            match remove_with_store(
                WorktreeRemoveOptions {
                    id: record.id.clone(),
                    force: options.force,
                    cleanup_branch: options.cleanup_branches,
                    allow_unmerged_branch: options.allow_unmerged_branches,
                },
                store,
            ) {
                Ok(output) => removed.push(output),
                Err(error) => skipped.push(WorktreeCleanupSkipped {
                    record,
                    safety: Some(safety),
                    reasons: vec![error.message],
                }),
            }
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
    let reconciliation_blockers = skipped
        .iter()
        .filter(|skipped| {
            skipped
                .safety
                .as_ref()
                .is_some_and(|safety| safety.worktree_missing)
        })
        .count();
    let counts = WorktreeCleanupCounts {
        candidates: candidates.len(),
        removed: removed.len(),
        skipped: skipped.len(),
        reconciliation_blockers,
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
    if safety.worktree_missing {
        reasons.push("missing active worktree requires `worktree inventory --apply` reconciliation authority".to_string());
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

/// Pin the repository-local commit identity in a freshly materialized
/// worktree to whatever the target remote's host policy declares, instead of
/// leaving it to fall back to whatever ambient `~/.gitconfig` identity the
/// provisioning environment happens to carry (#13647). Linked worktrees share
/// their source checkout's repository config, so this also corrects the
/// identity used by any sibling worktree of the same remote going forward.
///
/// A no-op when no policy is configured for the remote's host — Homeboy does
/// not invent an identity, it only pins one that is already declared.
fn pin_worktree_identity(worktree_path: &Path) -> Result<()> {
    let path = worktree_path.to_string_lossy();
    if let Some(identity) = git::resolve_host_identity_policy(&path)? {
        git::configure_identity(&path, &identity)?;
    }
    Ok(())
}

pub(super) fn create_with_store(
    options: WorktreeCreateOptions,
    store_dir: &Path,
) -> Result<WorktreeCreateOutput> {
    with_task_worktree_registry_write_lock(|| create_with_store_unlocked(options, store_dir))
}

fn create_with_store_unlocked(
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
    let existing = record_path(store_dir, &id)
        .exists()
        .then(|| read_record(store_dir, &id))
        .transpose()?;
    let identity = task_worktree_workspace_identity(&target.component_id, &id)?;
    if let Some(record) = &existing {
        verify_create_record_identity(
            record,
            &target.component_id,
            &source_checkout,
            &worktree_path,
            &options.branch,
            &identity,
        )?;
    }
    let desired_path = normalize_missing_path(&worktree_path);
    let registrations = branch_worktree_registrations(&source_checkout, &options.branch)?;
    if let Some(registration) = registrations
        .iter()
        .find(|registration| registration.path != desired_path)
    {
        return Err(branch_ownership_error(&options.branch, registration));
    }
    let handoff_base_ref = existing
        .as_ref()
        .map(|record| record.base_ref.as_str())
        .or(options.from.as_deref())
        .unwrap_or("HEAD");
    let handoff_observation = options
        .require_handoff_freshness
        .then(|| prepare_handoff_freshness(&source_checkout, handoff_base_ref))
        .transpose()?;
    if worktree_path.exists() {
        let record = existing.ok_or_else(|| {
            Error::validation_invalid_argument(
                "branch",
                "Task worktree path exists without a matching active Homeboy record",
                Some(worktree_path.display().to_string()),
                Some(vec![
                    "Inspect the existing checkout and adopt or remove it explicitly.".to_string(),
                ]),
            )
        })?;
        let _registration = registrations
            .iter()
            .find(|registration| registration.path == desired_path)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "branch",
                    "Task worktree path is not registered for the declared branch",
                    Some(worktree_path.display().to_string()),
                    None,
                )
            })?;
        verify_linked_worktree_identity(&source_checkout, &worktree_path, &options.branch)?;
        return Ok(WorktreeCreateOutput {
            handoff_freshness: handoff_observation
                .map(|observation| complete_handoff_freshness(&record, observation))
                .transpose()?,
            record,
            reconciliation: None,
        });
    }

    if let Some(record) = existing {
        if let Some(registration) = registrations
            .iter()
            .find(|registration| registration.path == desired_path)
        {
            if !registration.prunable {
                return Err(Error::validation_invalid_argument(
                    "branch",
                    "Task worktree registration is missing its path but is not safely prunable",
                    Some(registration.path.display().to_string()),
                    Some(vec![
                        "Inspect the Git worktree registration before retrying.".to_string(),
                    ]),
                ));
            }
            if !remove_exact_stale_worktree_registration(&source_checkout, &desired_path)? {
                return Err(Error::validation_invalid_argument(
                    "branch",
                    "Git stale task worktree registration changed before exact cleanup",
                    Some(worktree_path.display().to_string()),
                    None,
                ));
            }
        }
        if !branch_exists(&source_checkout, &options.branch)? {
            return Err(Error::validation_invalid_argument(
                "branch",
                "The declared branch is missing for the active task worktree record",
                Some(options.branch.clone()),
                None,
            ));
        }
        let previous = create_evidence(&record, "missing".to_string())?;
        let worktree_owner = ownership::owner_for_path_or_ancestor(parent)?;
        git::run_git(
            &source_checkout,
            &[
                "worktree",
                "add",
                &worktree_path.to_string_lossy(),
                &options.branch,
            ],
            "git worktree add restore",
        )?;
        ownership::normalize_created_path(
            &worktree_path,
            worktree_owner,
            true,
            "git worktree add",
        )?;
        pin_worktree_identity(&worktree_path)?;
        let current = create_evidence(&record, "registered".to_string())?;
        return Ok(WorktreeCreateOutput {
            handoff_freshness: handoff_observation
                .map(|observation| complete_handoff_freshness(&record, observation))
                .transpose()?,
            record,
            reconciliation: Some(WorktreeCreateReconciliation {
                action: WorktreeCreateAction::Restored,
                previous,
                current,
            }),
        });
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
    pin_worktree_identity(&worktree_path)?;

    let mut record = TaskWorktreeRecord {
        id,
        component_id: target.component_id,
        source_checkout: source_checkout.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: options.branch,
        base_ref,
        workspace_identity: None,
        task_url: options.task_url,
        run_id: options.run_id.clone(),
        cleanup_policy: options
            .cleanup_policy
            .unwrap_or_else(|| CleanupPolicy::default_for_run(options.run_id.as_deref())),
        terminal_disposition: None,
        branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
        created_at: chrono::Utc::now().to_rfc3339(),
        state: TaskWorktreeState::Active,
        lifecycle_revision: 0,
        terminal_workspace_authority: None,
    };
    record.workspace_identity = Some(record.effective_workspace_identity()?);
    write_record_unlocked(store_dir, &record)?;
    Ok(WorktreeCreateOutput {
        handoff_freshness: handoff_observation
            .map(|observation| complete_handoff_freshness(&record, observation))
            .transpose()?,
        record,
        reconciliation: None,
    })
}

#[derive(Debug)]
struct PendingHandoffFreshness {
    resolved_base_ref: String,
    resolved_base_sha: String,
    remote_default_ref: String,
    remote_default_sha: String,
}

fn prepare_handoff_freshness(source: &Path, base_ref: &str) -> Result<PendingHandoffFreshness> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    git::run_git_with_env_timeout(
        source,
        &["fetch", "origin"],
        "git fetch origin for worktree handoff",
        &[],
        TIMEOUT,
    )?;
    let advertised = git::run_git_with_env_timeout(
        source,
        &["ls-remote", "--symref", "origin", "HEAD"],
        "git ls-remote origin HEAD for worktree handoff",
        &[],
        TIMEOUT,
    )?;
    let remote_head = advertised.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")
            .and_then(|value| value.strip_suffix("\tHEAD"))
    });
    let advertised_sha = advertised.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == "HEAD"
            && (40..=64).contains(&sha.len())
            && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| sha.to_ascii_lowercase())
    });
    let (remote_head, advertised_sha) = remote_head.zip(advertised_sha).ok_or_else(|| {
        Error::validation_invalid_argument(
            "handoff_freshness",
            "Remote origin did not advertise an unambiguous default branch and commit",
            None,
            Some(vec![
                "Configure origin/HEAD or provide a reachable remote.".to_string()
            ]),
        )
    })?;
    let remote_default_ref = format!("refs/remotes/origin/{remote_head}");
    let remote_default_sha = git::run_git_with_env_timeout(
        source,
        &[
            "rev-parse",
            "--verify",
            &format!("{remote_default_ref}^{{commit}}"),
        ],
        "resolve fetched remote default for worktree handoff",
        &[],
        TIMEOUT,
    )?
    .trim()
    .to_ascii_lowercase();
    if remote_default_sha != advertised_sha {
        return Err(Error::validation_invalid_argument(
            "handoff_freshness",
            "Remote default changed during worktree handoff verification",
            Some(remote_default_ref),
            Some(vec!["Retry to verify one remote advertisement.".to_string()]),
        ));
    }
    let resolved_base_sha = git::run_git_with_env_timeout(
        source,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
        "resolve worktree handoff base",
        &[],
        TIMEOUT,
    )?
    .trim()
    .to_ascii_lowercase();
    Ok(PendingHandoffFreshness {
        resolved_base_ref: base_ref.to_string(),
        resolved_base_sha,
        remote_default_ref,
        remote_default_sha,
    })
}

fn complete_handoff_freshness(
    record: &TaskWorktreeRecord,
    observation: PendingHandoffFreshness,
) -> Result<WorktreeHandoffFreshness> {
    let worktree_sha = git::run_git(
        Path::new(&record.worktree_path),
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolve worktree handoff HEAD",
    )?
    .trim()
    .to_ascii_lowercase();
    Ok(WorktreeHandoffFreshness {
        status: "verified".to_string(),
        proof: WorktreeHandoffFreshnessProof {
            schema: "homeboy/worktree-handoff-freshness/v1".to_string(),
            proof_id: uuid::Uuid::new_v4().to_string(),
            handle: record.id.clone(),
            worktree_sha,
            resolved_base_ref: observation.resolved_base_ref,
            resolved_base_sha: observation.resolved_base_sha,
            remote_default_ref: observation.remote_default_ref,
            remote_default_sha: observation.remote_default_sha.clone(),
            remote_default_advertised_sha: observation.remote_default_sha,
            verified_at: chrono::Utc::now().to_rfc3339(),
        },
    })
}

#[derive(Debug)]
struct BranchWorktreeRegistration {
    path: PathBuf,
    prunable: bool,
    status: String,
}

fn branch_worktree_registrations(
    source: &Path,
    branch: &str,
) -> Result<Vec<BranchWorktreeRegistration>> {
    let output = git::run_git(
        source,
        &["worktree", "list", "--porcelain"],
        "git worktree list",
    )?;
    let mut registrations = Vec::new();
    for block in output.split("\n\n") {
        let mut path = None;
        let mut registered_branch = None;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(normalize_missing_path(Path::new(value)));
            } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
                registered_branch = Some(value);
            } else if line.starts_with("prunable") {
                prunable = true;
            }
        }
        if registered_branch == Some(branch) {
            if let Some(path) = path {
                registrations.push(BranchWorktreeRegistration {
                    path,
                    prunable,
                    status: if prunable { "prunable" } else { "registered" }.to_string(),
                });
            }
        }
    }
    Ok(registrations)
}

fn branch_exists(source: &Path, branch: &str) -> Result<bool> {
    Ok(git::run_git(
        source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        "git show-ref task branch",
    )
    .is_ok())
}

fn verify_linked_worktree_identity(source: &Path, worktree: &Path, branch: &str) -> Result<()> {
    let common_dir = git_common_dir(source)?;
    let pointer = fs::read_to_string(worktree.join(".git")).map_err(|_| {
        linked_worktree_identity_error(worktree, "worktree .git pointer is unavailable")
    })?;
    let registration = pointer
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .and_then(|pointer| resolve_gitdir_pointer(worktree, pointer))
        .ok_or_else(|| {
            linked_worktree_identity_error(
                worktree,
                "worktree .git is not a linked-worktree pointer",
            )
        })?;
    let registrations = common_dir.join("worktrees");
    if registration.parent() != Some(registrations.as_path())
        || !registration.is_dir()
        || fs::read_to_string(registration.join("gitdir"))
            .ok()
            .is_none_or(|pointer| {
                resolve_gitdir_pointer(&registration, pointer.trim())
                    != worktree.join(".git").canonicalize().ok()
            })
    {
        return Err(linked_worktree_identity_error(
            worktree,
            "linked-worktree registration does not point back to the declared path",
        ));
    }
    let current_branch = git::run_git(worktree, &["branch", "--show-current"], "git branch")?;
    let head = git::run_git(worktree, &["rev-parse", "HEAD"], "git rev-parse HEAD")?;
    let branch_head = git::run_git(
        source,
        &["rev-parse", &format!("refs/heads/{branch}")],
        "git rev-parse branch",
    )?;
    if current_branch.trim() != branch || head.trim() != branch_head.trim() {
        return Err(linked_worktree_identity_error(
            worktree,
            "linked-worktree branch or HEAD does not match the declared branch",
        ));
    }
    Ok(())
}

fn resolve_gitdir_pointer(base: &Path, pointer: &str) -> Option<PathBuf> {
    let pointer = resolve_gitdir_pointer_path(base, pointer);
    pointer.canonicalize().ok()
}

fn resolve_gitdir_pointer_path(base: &Path, pointer: &str) -> PathBuf {
    let pointer = PathBuf::from(pointer);
    let pointer = if pointer.is_absolute() {
        pointer
    } else {
        base.join(pointer)
    };
    let mut normalized = PathBuf::new();
    for component in pointer.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn remove_exact_stale_worktree_registration(source: &Path, worktree: &Path) -> Result<bool> {
    if worktree.exists() {
        return Ok(false);
    }
    let expected_gitdir = resolve_gitdir_pointer_path(worktree, ".git");
    let registrations = git_common_dir(source)?.join("worktrees");
    let entries = fs::read_dir(&registrations).map_err(|error| {
        Error::internal_io(error.to_string(), Some(registrations.display().to_string()))
    })?;
    for entry in entries {
        let directory = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(registrations.display().to_string()))
            })?
            .path();
        let matches = fs::read_to_string(directory.join("gitdir"))
            .ok()
            .is_some_and(|pointer| {
                resolve_gitdir_pointer_path(&directory, pointer.trim()) == expected_gitdir
            });
        if !matches {
            continue;
        }
        if directory.join("locked").exists()
            || worktree.exists()
            || fs::read_to_string(directory.join("gitdir"))
                .ok()
                .is_none_or(|pointer| {
                    resolve_gitdir_pointer_path(&directory, pointer.trim()) != expected_gitdir
                })
        {
            return Ok(false);
        }
        fs::remove_dir_all(&directory).map_err(|error| {
            Error::internal_io(error.to_string(), Some(directory.display().to_string()))
        })?;
        return Ok(true);
    }
    Ok(false)
}

fn git_common_dir(source: &Path) -> Result<PathBuf> {
    let raw = git::run_git(source, &["rev-parse", "--git-common-dir"], "git common dir")?;
    let path = PathBuf::from(raw.trim());
    let path = if path.is_absolute() {
        path
    } else {
        source.join(path)
    };
    path.canonicalize()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn linked_worktree_identity_error(worktree: &Path, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "branch",
        "Existing task worktree does not prove linked-worktree identity",
        Some(worktree.display().to_string()),
        Some(vec![reason.to_string()]),
    )
}

fn verify_create_record_identity(
    record: &TaskWorktreeRecord,
    component_id: &str,
    source_checkout: &Path,
    worktree_path: &Path,
    branch: &str,
    workspace_identity: &WorkspaceIdentity,
) -> Result<()> {
    let expected_path = normalize_missing_path(worktree_path);
    let record_path = normalize_missing_path(Path::new(&record.worktree_path));
    let record_source = Path::new(&record.source_checkout).canonicalize().ok();
    if record.state != TaskWorktreeState::Active
        || record.component_id != component_id
        || record.branch != branch
        || record_path != expected_path
        || record_source.as_deref() != Some(source_checkout)
        || record.effective_workspace_identity()? != *workspace_identity
    {
        return Err(Error::validation_invalid_argument(
            "branch",
            "Existing task-worktree record does not prove the declared worktree identity",
            Some(record.id.clone()),
            Some(vec!["Record component, source checkout, contained path, branch, state, and workspace identity must match exactly.".to_string()]),
        ));
    }
    Ok(())
}

fn branch_ownership_error(branch: &str, registration: &BranchWorktreeRegistration) -> Error {
    Error::validation_invalid_argument(
        "branch",
        "Declared branch is already claimed by another Git worktree path",
        Some(branch.to_string()),
        Some(vec![
            format!("owner_path={}", registration.path.display()),
            format!("registration={}", registration.status),
        ]),
    )
}

fn create_evidence(
    record: &TaskWorktreeRecord,
    git_registration: String,
) -> Result<WorktreeCreateEvidence> {
    Ok(WorktreeCreateEvidence {
        task_worktree_id: record.id.clone(),
        component_id: record.component_id.clone(),
        source_checkout: record.source_checkout.clone(),
        worktree_path: record.worktree_path.clone(),
        branch: record.branch.clone(),
        workspace_identity: record.effective_workspace_identity()?,
        git_registration,
    })
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

pub(super) fn inventory_with_store_and_authority(
    options: WorktreeInventoryOptions,
    store_dir: &Path,
    adopted_store_dir: &Path,
    authority: &dyn WorktreeReconciliationAuthority,
) -> Result<WorktreeInventoryOutput> {
    let limit = options.limit.max(1);
    let worktrees = list_with_store(store_dir)?.worktrees;
    let total = worktrees.len();
    let mut worktrees = worktrees.into_iter().filter(|record| {
        options
            .cursor
            .as_ref()
            .is_none_or(|cursor| record.id.as_str() > cursor.as_str())
    });
    let records_page: Vec<_> = worktrees.by_ref().take(limit).collect();
    let next_cursor = worktrees.next().map(|_| {
        records_page
            .last()
            .expect("a page with a following record is non-empty")
            .id
            .clone()
    });
    let truncated = next_cursor.is_some();
    let mut records = Vec::new();

    for mut record in records_page {
        let mut path_exists = Path::new(&record.worktree_path).exists();
        let mut missing_active = if record.state == TaskWorktreeState::Active && !path_exists {
            Some(missing_active_worktree(&record))
        } else {
            None
        };
        let reconciliation = if options.apply && missing_active.is_some() {
            let authority_snapshot = authority.acquire(&record)?;
            if let WorktreeLivenessAuthority::Terminal { claim, .. } = &authority_snapshot {
                let validation = claim
                    .verify_shape(chrono::Utc::now().timestamp_millis().max(0) as u64)
                    .and_then(|_| authority.validate(&record, claim));
                let valid = match validation {
                    Ok(valid) => valid,
                    Err(error) => {
                        // A validation transport failure still owns a fence.
                        // Release it before returning a typed, non-mutating refusal.
                        let release = authority.release(claim);
                        records.push(WorktreeInventoryRecord {
                            record,
                            path_exists,
                            missing_active,
                            reconciliation: Some(WorktreeReconciliationResult {
                                action: WorktreeReconciliationAction::Refused,
                                provenance: "workspace reconciliation claim validation".to_string(),
                                reason: Some(error.message),
                            }),
                        });
                        release?;
                        continue;
                    }
                };
                if !valid {
                    let release = authority.release(claim);
                    records.push(WorktreeInventoryRecord {
                        record,
                        path_exists,
                        missing_active,
                        reconciliation: Some(WorktreeReconciliationResult {
                            action: WorktreeReconciliationAction::Refused,
                            provenance: "workspace reconciliation claim validation".to_string(),
                            reason: Some("workspace owner rejected, expired, or does not support the reconciliation claim".to_string()),
                        }),
                    });
                    release?;
                    continue;
                }
            }
            let reconciliation = reconcile_missing_record_with_store(
                store_dir,
                &record,
                &authority_snapshot,
                authority,
            );
            if let WorktreeLivenessAuthority::Terminal { claim, .. } = &authority_snapshot {
                authority.release(claim)?;
            }
            let (reread, reread_path_exists, result) = reconciliation?;
            record = reread;
            path_exists = reread_path_exists;
            missing_active = (record.state == TaskWorktreeState::Active && !path_exists)
                .then(|| missing_active_worktree(&record));
            Some(result)
        } else {
            None
        };
        records.push(WorktreeInventoryRecord {
            record,
            path_exists,
            missing_active,
            reconciliation,
        });
    }

    // Cross-tab reflects returned post-apply records rather than their
    // pre-apply snapshots.
    let cross_tab = records
        .iter()
        .fold(WorktreeInventoryCrossTab::default(), |mut tab, item| {
            match (&item.record.state, item.path_exists) {
                (TaskWorktreeState::Active, true) => tab.active_path_present += 1,
                (TaskWorktreeState::Active, false) => tab.active_path_missing += 1,
                (TaskWorktreeState::Removed, true) => tab.removed_path_present += 1,
                (TaskWorktreeState::Removed, false) => tab.removed_path_missing += 1,
            }
            tab
        });
    let adopted = list_adopted_with_store(adopted_store_dir)?;
    let adopted_total = adopted.len();
    let mut adopted = adopted.into_iter().filter(|record| {
        options
            .adopted_cursor
            .as_ref()
            .is_none_or(|cursor| record.handle.as_str() > cursor.as_str())
    });
    let adopted_records: Vec<_> = adopted
        .by_ref()
        .take(limit)
        .map(|record| AdoptedWorkspaceInventoryRecord {
            path_exists: Path::new(&record.path).exists(),
            continuation: format!(
                "Restore or re-adopt workspace handle `{}` before use.",
                record.handle
            ),
            reason: MissingActiveWorktreeReason::AdoptedWorkspace,
            record,
        })
        .collect();
    let adopted_next_cursor = adopted.next().map(|_| {
        adopted_records
            .last()
            .expect("a page with a following record is non-empty")
            .record
            .handle
            .clone()
    });
    let adopted_truncated = adopted_next_cursor.is_some();

    Ok(WorktreeInventoryOutput {
        schema: "homeboy/worktree-inventory/v1",
        authorization: if options.apply {
            WorktreeInventoryAuthorization::ExplicitApply
        } else {
            WorktreeInventoryAuthorization::Preview
        },
        apply_refusal: None,
        cursor: options.cursor,
        next_cursor,
        limit,
        total,
        truncated,
        cross_tab_scope: "task_worktree_page",
        cross_tab,
        records,
        adopted: WorktreeAdoptedInventoryPage {
            cursor: options.adopted_cursor,
            next_cursor: adopted_next_cursor,
            total: adopted_total,
            truncated: adopted_truncated,
            records: adopted_records,
        },
    })
}

fn missing_active_worktree(record: &TaskWorktreeRecord) -> MissingActiveWorktree {
    if record.cleanup_policy == CleanupPolicy::PreserveOnFailure {
        return MissingActiveWorktree {
            reason: MissingActiveWorktreeReason::PreserveOnFailure,
            local_evidence: local_inventory_evidence(record),
            continuation: format!(
                "Inspect preserved task worktree `{}` and explicitly remove it when terminal.",
                record.id
            ),
        };
    }
    let evidence = local_inventory_evidence(record);
    let reason = if !evidence.source_checkout_exists {
        MissingActiveWorktreeReason::SourceCheckoutUnavailable
    } else if evidence.source_dirty == Some(true) {
        MissingActiveWorktreeReason::SourceDirty
    } else if evidence
        .unpushed_branch_commits
        .is_some_and(|count| count > 0)
    {
        MissingActiveWorktreeReason::UnpushedBranch
    } else if evidence.unavailable_reason.is_some() {
        MissingActiveWorktreeReason::BranchEvidenceUnavailable
    } else {
        MissingActiveWorktreeReason::RequiresAuthoritativeLiveness
    };
    MissingActiveWorktree {
        reason,
        local_evidence: evidence,
        continuation: format!(
            "Preserve `{}`. `worktree inventory --apply` is refused until Homeboy has a leased local-and-offloaded liveness and workspace-evidence primitive.",
            record.id
        ),
    }
}

fn reconcile_missing_record_with_store(
    store_dir: &Path,
    expected: &TaskWorktreeRecord,
    authority_snapshot: &WorktreeLivenessAuthority,
    authority: &dyn WorktreeReconciliationAuthority,
) -> Result<(TaskWorktreeRecord, bool, WorktreeReconciliationResult)> {
    with_task_worktree_registry_write_lock(|| {
        // Re-read under the exclusive registry lease. Any concurrent publisher must
        // finish before this snapshot is evaluated and conditionally written.
        let mut record = read_record(store_dir, &expected.id)?;
        if record.state != TaskWorktreeState::Active
            || record.id != expected.id
            || record.worktree_path != expected.worktree_path
            || record.effective_workspace_identity()? != expected.effective_workspace_identity()?
            || record.lifecycle_revision != expected.lifecycle_revision
            || record.run_id != expected.run_id
            || Path::new(&record.worktree_path).exists()
        {
            let path_exists = Path::new(&record.worktree_path).exists();
            return Ok((
                record,
                path_exists,
                WorktreeReconciliationResult {
                    action: WorktreeReconciliationAction::Preserved,
                    provenance: "leased manifest re-read".to_string(),
                    reason: Some(
                        "manifest state or workspace path changed before apply".to_string(),
                    ),
                },
            ));
        }
        let evidence = local_inventory_evidence(&record);
        let shared_active_owner = list_with_store(store_dir)?
            .worktrees
            .into_iter()
            .any(|other| {
                other.id != record.id
                    && other.state == TaskWorktreeState::Active
                    && other.worktree_path == record.worktree_path
            });
        if shared_active_owner {
            return Ok((
                record,
                false,
                WorktreeReconciliationResult {
                    action: WorktreeReconciliationAction::Preserved,
                    provenance: "leased manifest re-read".to_string(),
                    reason: Some(
                        "another active task-worktree manifest owns this workspace path"
                            .to_string(),
                    ),
                },
            ));
        }
        let local_safe = record.cleanup_policy != CleanupPolicy::PreserveOnFailure
            && evidence.source_checkout_exists
            && evidence.source_dirty == Some(false)
            && evidence.unpushed_branch_commits == Some(0)
            && evidence.unavailable_reason.is_none();
        if !local_safe {
            return Ok((
                record,
                false,
                WorktreeReconciliationResult {
                    action: WorktreeReconciliationAction::Preserved,
                    provenance: "leased manifest re-read plus local git evidence".to_string(),
                    reason: Some(
                        "local dirty or task-branch evidence is not safely terminal".to_string(),
                    ),
                },
            ));
        }
        match authority_snapshot {
            WorktreeLivenessAuthority::Terminal { claim, provenance } => {
                let identity = record.effective_workspace_identity()?;
                if claim.workspace != identity
                    || (authority.requires_terminal_workspace_authority_proof()
                        && !record
                            .terminal_workspace_authority
                            .as_ref()
                            .is_some_and(|proof| {
                                proof.exact_for(&record, record.run_id.as_deref())
                            }))
                    || !authority.ready_to_commit(claim)
                {
                    return Ok((record, false, WorktreeReconciliationResult {
                        action: WorktreeReconciliationAction::Refused,
                        provenance: "leased manifest re-read".to_string(),
                        reason: Some("workspace identity changed or the local reconciliation claim budget expired before commit".to_string()),
                    }));
                }
                record.state = TaskWorktreeState::Removed;
                record.lifecycle_revision =
                    record.lifecycle_revision.checked_add(1).ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "lifecycle_revision",
                            "task worktree lifecycle revision overflowed during reconciliation",
                            Some(record.id.clone()),
                            None,
                        )
                    })?;
                write_record_unlocked(store_dir, &record)?;
                Ok((
                    record,
                    false,
                    WorktreeReconciliationResult {
                        action: WorktreeReconciliationAction::Reconciled,
                        provenance: format!("leased manifest re-read; {provenance}"),
                        reason: None,
                    },
                ))
            }
            WorktreeLivenessAuthority::Live { provenance } => Ok((
                record,
                false,
                WorktreeReconciliationResult {
                    action: WorktreeReconciliationAction::Preserved,
                    provenance: provenance.clone(),
                    reason: Some("authoritative run is live".to_string()),
                },
            )),
            WorktreeLivenessAuthority::Incomplete { reason } => Ok((
                record,
                false,
                WorktreeReconciliationResult {
                    action: WorktreeReconciliationAction::Refused,
                    provenance: "leased manifest re-read".to_string(),
                    reason: Some(format!("liveness authority is incomplete: {reason}")),
                },
            )),
        }
    })
}

fn local_inventory_evidence(record: &TaskWorktreeRecord) -> WorktreeInventoryLocalEvidence {
    let source = Path::new(&record.source_checkout);
    if !source.is_dir() {
        return WorktreeInventoryLocalEvidence {
            source_checkout_exists: false,
            source_dirty: None,
            unpushed_branch_commits: None,
            unavailable_reason: Some("recorded source checkout is unavailable".to_string()),
        };
    }
    let source_dirty = is_dirty(source).ok();
    let unpushed_branch_commits =
        unpushed_branch_commit_count(source, &record.branch, &record.base_ref).ok();
    let unavailable_reason = match (&source_dirty, &unpushed_branch_commits) {
        (None, _) => Some("could not inspect source checkout dirtiness".to_string()),
        (_, None) => Some("could not inspect task branch push state".to_string()),
        _ => None,
    };
    WorktreeInventoryLocalEvidence {
        source_checkout_exists: true,
        source_dirty,
        unpushed_branch_commits,
        unavailable_reason,
    }
}

fn unpushed_branch_commit_count(source: &Path, branch: &str, base_ref: &str) -> Result<u32> {
    git::run_git(
        source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        "git show-ref task branch",
    )?;
    let upstream = git::run_git(
        source,
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ],
        "git task branch upstream",
    );
    let range = match upstream {
        Ok(upstream) if !upstream.trim().is_empty() => format!("{}..{branch}", upstream.trim()),
        _ => format!("{base_ref}..{branch}"),
    };
    let count = git::run_git(
        source,
        &["rev-list", "--count", &range],
        "git task branch rev-list",
    )?;
    count.trim().parse::<u32>().map_err(|error| {
        Error::internal_unexpected(format!("invalid task branch commit count: {error}"))
    })
}

pub(super) fn list_adopted_with_store(store_dir: &Path) -> Result<Vec<AdoptedWorkspaceRecord>> {
    if !store_dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(store_dir)
        .map_err(|err| Error::internal_io(err.to_string(), Some(store_dir.display().to_string())))?
    {
        let entry = entry.map_err(|err| Error::internal_io(err.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
            records.push(read_adopted_record_path(&entry.path())?);
        }
    }
    records.sort_by(|left, right| left.handle.cmp(&right.handle));
    Ok(records)
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
    // Homeboy has already verified this branch is contained in its configured cleanup base.
    // Use that proof instead of Git's separate upstream-based `-d` heuristic.
    let delete_flag = "-D";
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
    Ok(metadata_dir_in_root(&data_root_of(
        &paths::observation_db()?,
    )?))
}

/// [`metadata_dir`] below an explicitly injected data root.
pub(super) fn metadata_dir_in_root(data_root: &Path) -> PathBuf {
    data_root.join("task-worktrees")
}

pub(super) fn adopted_metadata_dir() -> Result<PathBuf> {
    Ok(adopted_metadata_dir_in_root(&data_root_of(
        &paths::observation_db()?,
    )?))
}

/// [`adopted_metadata_dir`] below an explicitly injected data root.
pub(super) fn adopted_metadata_dir_in_root(data_root: &Path) -> PathBuf {
    data_root.join("adopted-workspaces")
}

/// The data root that owns an observation database file.
///
/// Both registries are siblings of the SQLite store, so they are derived from
/// one resolution rather than each re-resolving the data root.
fn data_root_of(observation_db: &Path) -> Result<PathBuf> {
    observation_db
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "observation database path `{}` has no parent directory",
                observation_db.display()
            ))
        })
}

pub(super) fn record_path(store_dir: &Path, id: &str) -> PathBuf {
    store_dir.join(format!("{}.json", paths::sanitize_path_segment(id)))
}

pub(super) fn write_record(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
    record.effective_workspace_identity()?;
    with_task_worktree_registry_write_lock(|| write_record_unlocked(store_dir, record))
}

pub(super) fn write_record_unlocked(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
    let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
    fs::create_dir_all(store_dir).map_err(|err| {
        Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|err| Error::internal_json(err.to_string(), Some(record.id.clone())))?;
    let path = record_path(store_dir, &record.id);
    crate::io::write_output_file_atomically(
        &path,
        format!("{json}\n"),
        crate::io::OutputWriteOptions::file(),
    )
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

pub(super) fn read_adopted_record_path(path: &Path) -> Result<AdoptedWorkspaceRecord> {
    let raw = fs::read_to_string(path)
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
