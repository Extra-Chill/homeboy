use super::*;

pub(super) fn source_checkout_for_worktree(target: &component::ResolvedTarget) -> Result<PathBuf> {
    if let Some(git_root) = &target.git_root {
        return git_root.canonicalize().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(target.source_path.display().to_string()),
            )
        });
    }

    if let Some(checkout) = lab_runner_workspace_checkout(&target.component_id)? {
        return Ok(checkout);
    }

    Err(Error::validation_invalid_argument(
        "component",
        "Component local_path is not inside a git checkout",
        Some(target.component_id.clone()),
        Some(vec!["Register a git-backed component checkout".to_string()]),
    ))
}

fn lab_runner_workspace_checkout(component_id: &str) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("current_dir".to_string())))?;
    let Some(runner_root) = runner_workspace_root_from_lab_snapshot(&cwd) else {
        return Ok(None);
    };
    let candidate = runner_root.join(component_id);
    let Some(git_root) = component::resolution::detect_git_root(&candidate) else {
        return Ok(None);
    };
    if git_root
        != candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone())
    {
        return Ok(None);
    }
    let Some(discovered) = component::discover_from_portable(&git_root) else {
        return Ok(None);
    };
    if discovered.id != component_id {
        return Ok(None);
    }
    git_root
        .canonicalize()
        .map(Some)
        .map_err(|err| Error::internal_io(err.to_string(), Some(candidate.display().to_string())))
}

fn runner_workspace_root_from_lab_snapshot(cwd: &Path) -> Option<PathBuf> {
    for ancestor in cwd.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("_lab_workspaces") {
            return ancestor.parent().map(Path::to_path_buf);
        }
    }
    None
}

pub(super) fn queue_row(
    branch: &str,
    handle: String,
    command: Vec<String>,
    status: WorktreeQueueCreateStatus,
) -> WorktreeQueueCreateRow {
    WorktreeQueueCreateRow {
        branch: branch.to_string(),
        handle,
        status,
        command,
        retry_after_seconds: None,
        active_lock_holder: None,
        path: None,
        error: None,
    }
}

pub(super) fn worktree_create_command(
    options: &WorktreeQueueCreateOptions,
    branch: &str,
) -> Vec<String> {
    let mut args = vec![
        "homeboy".to_string(),
        "worktree".to_string(),
        "create".to_string(),
        options.repo.clone(),
        "--branch".to_string(),
        branch.to_string(),
        "--from".to_string(),
        options.from.clone(),
    ];
    if let Some(task_url) = &options.task_url {
        args.push("--task-url".to_string());
        args.push(task_url.clone());
    }
    args
}

pub(super) fn worktree_handle(repo: &str, branch: &str) -> String {
    format!("{}@{}", repo, branch_slug(branch))
}
