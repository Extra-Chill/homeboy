//! Cook destination provisioning and source-reference adapters.

use super::run::*;

/// Converge a Cook promotion destination before compiling a task plan. This is
/// controller-owned so local and Lab dispatch use the same managed checkout.
/// Check source history before any durable admission or capacity wait. Preview
/// only plans the fetch; admission performs it once against the source remote.
pub(crate) fn preflight_cook_source_history(
    args: &AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
    preview: bool,
) -> homeboy::core::Result<bool> {
    if homeboy::core::parsed_command_preflight::captured_result().is_some_and(|result| {
        result.placement.selected == homeboy_lab_contract::EffectiveExecutionPlacement::Local
    }) {
        return Ok(false);
    }
    let source = if let Some(repository) = repository {
        PathBuf::from(&repository.source_checkout)
    } else if let Some(path) = args.dispatch.cwd.as_deref().or_else(|| {
        args.to_worktree
            .as_deref()
            .filter(|path| Path::new(path).is_dir())
    }) {
        PathBuf::from(path)
    } else {
        let Some(repo) = cook_provision_repository(args) else {
            return Ok(false);
        };
        let target =
            homeboy::core::component::resolve_target(homeboy::core::component::TargetSpec {
                component_id: Some(&repo),
                allow_synthetic: true,
                accept_bare_directory: true,
                ..Default::default()
            })?;
        PathBuf::from(target.git_root.as_deref().unwrap_or(&target.source_path))
    };
    if preview {
        Ok(homeboy::core::git::shallow_source_remote(&source)?.is_some())
    } else {
        homeboy::core::git::deepen_shallow_source(&source)?;
        Ok(false)
    }
}

pub(crate) fn provision_cook_destination(
    args: &AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
    preview: bool,
) -> homeboy::core::Result<Value> {
    let planned_unshallow = preflight_cook_source_history(args, repository, preview)?;
    let handle = args.to_worktree.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--to-worktree is required before provisioning a Cook destination".to_string(),
        ])
    })?;
    let existing = if let Some(cwd) = args.dispatch.cwd.as_deref() {
        Some((PathBuf::from(cwd), Some("explicit_cwd".to_string())))
    } else if Path::new(handle).is_dir() {
        Some((
            PathBuf::from(handle),
            Some("direct_task_worktree".to_string()),
        ))
    } else {
        homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(handle)?
            .map(|target| (target.path, target.source_kind))
    };
    if let Some((path, kind)) = existing {
        validate_cook_worktree_root(&path, handle)?;
        let path = std::fs::canonicalize(&path).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "to_worktree",
                format!("cannot use Cook destination: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        if preview || args.dispatch.cwd.is_some() {
            validate_cook_destination_identity(args, &path)?;
        }
        if args.dispatch.cwd.is_some() {
            validate_cook_cwd_destination_identity(&path, handle)?;
        }
        if !preview {
            validate_cook_base_before_provisioning(args, repository)?;
        }
        return Ok(serde_json::json!({
            "action": if preview && planned_unshallow { "planned_unshallow" } else if preview { "planned_reuse" } else { "existing" },
            "kind": if preview { Some("preview_local") } else { kind.as_deref() },
            "handle": handle,
            "path": path,
        }));
    }
    if !preview {
        validate_cook_base_before_provisioning(args, repository)?;
    }
    let intent = cook_provider_provision_intent(args, handle)?;
    let plan = preflight_missing_cook_provider_workspace(&intent, repository)?;
    if preview {
        let (homeboy::core::worktree_provider::WorktreeProvisionPlan::Admitted(destination)
        | homeboy::core::worktree_provider::WorktreeProvisionPlan::Planned(destination)) = plan;
        return Ok(serde_json::json!({
            "action": if planned_unshallow { "planned_unshallow" } else { "planned_create" }, "kind": "native",
            "handle": destination.ownership.handle, "path": destination.ownership.path,
            "branch": destination.ownership.branch,
            "intent": { "handle": intent.handle, "repo": intent.repo, "base": intent.base,
                "head": intent.head, "task_url": intent.task_url },
        }));
    }
    Ok(serde_json::json!({
        "action": "lookup_pending", "kind": "native", "handle": intent.handle,
        "provision_intent": {
            "repo": intent.repo, "base": intent.base, "head": intent.head, "task_url": intent.task_url,
        },
        "lifecycle_intent": { "purpose": "agent_task_cook", "cleanup_policy": "remove_on_success" },
    }))
}

/// Resolve the native provider identity without creating a worktree. Preview
/// and execution both use this check. Repository, base and head identify the
/// worktree; a task URL is recorded when given but is not needed to create it.
pub(super) fn preflight_missing_cook_provider_workspace(
    intent: &homeboy::core::worktree_provider::WorktreeProvisionIntent,
    repository: Option<&ResolvedCookRepository>,
) -> homeboy::core::Result<homeboy::core::worktree_provider::WorktreeProvisionPlan> {
    match repository {
        Some(repo) => homeboy::core::worktree_provider::plan_worktree_provision(
            intent,
            (&repo.component.id, &repo.source_checkout),
        ),
        None => {
            let target =
                homeboy::core::component::resolve_target(homeboy::core::component::TargetSpec {
                    component_id: Some(&intent.repo),
                    allow_synthetic: true,
                    accept_bare_directory: true,
                    ..Default::default()
                })?;
            let source = target.git_root.as_deref().unwrap_or(&target.source_path);
            homeboy::core::worktree_provider::plan_worktree_provision(
                intent,
                (&target.component_id, source),
            )
        }
    }
}

pub(super) fn cook_provider_provision_intent(
    args: &AgentTaskCookArgs,
    handle: &str,
) -> homeboy::core::Result<homeboy::core::worktree_provider::WorktreeProvisionIntent> {
    let (repo, head) = cook_missing_destination_requirements(args)?;
    Ok(homeboy::core::worktree_provider::WorktreeProvisionIntent {
        handle: handle.to_string(),
        repo,
        base: args
            .base
            .clone()
            .expect("Cook base is resolved before provider preflight"),
        head,
        task_url: args.dispatch.task_url.clone(),
    })
}

/// Report all creation inputs together, independently of base inference. A task
/// issue can supply the branch; PR refs have already been resolved by the caller.
pub(super) fn cook_missing_destination_requirements(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<(String, String)> {
    let repo = cook_provision_repository(args);
    let head = args.head.clone().or_else(|| {
        args.dispatch
            .task_url
            .as_deref()
            .and_then(|url| derived_cook_branch(url).ok())
    });
    let mut missing = Vec::new();
    if repo.is_none() {
        missing.push(
            "--repo <repo> is required to create a missing --to-worktree destination".to_string(),
        );
    }
    if head.is_none() {
        missing.push(
            "--head <branch> is required to create a missing --to-worktree destination".to_string(),
        );
    }
    if !missing.is_empty() {
        return Err(homeboy::core::Error::validation_missing_argument(missing));
    }
    Ok((
        repo.expect("repository checked"),
        head.expect("branch checked"),
    ))
}

/// Provider worktrees are rooted at a canonical repository primary, while Cook
/// executes the resolved component within that checkout.
pub(super) fn cook_provision_repository(args: &AgentTaskCookArgs) -> Option<String> {
    args.repository_identity
        .as_ref()
        .and_then(|identity| identity.get("repository_path"))
        .and_then(Value::as_str)
        .filter(|repository| !repository.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            args.repository_identity
                .as_ref()
                .and_then(|identity| identity.get("component_id"))
                .and_then(Value::as_str)
                .filter(|component| !component.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            args.repository_identity
                .as_ref()
                .and_then(|identity| identity.get("repository_name"))
                .and_then(Value::as_str)
                .filter(|repository| !repository.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| args.dispatch.repo.clone())
}

/// An explicit CWD is the Cook workspace authority. A co-supplied destination
/// can only name that same existing worktree; never consult a provider merely
/// to rediscover a path the operator already supplied.
pub(super) fn validate_cook_cwd_destination_identity(
    cwd: &Path,
    to_worktree: &str,
) -> homeboy::core::Result<()> {
    let destination = if Path::new(to_worktree).is_dir() {
        std::fs::canonicalize(to_worktree)
    } else if let Some(target) =
        homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(to_worktree)?
    {
        std::fs::canonicalize(target.path)
    } else {
        validate_logical_worktree_handle_path_relationship(cwd, to_worktree)?;
        Ok(cwd.to_path_buf())
    }
    .map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "to_worktree",
            format!("cannot use --to-worktree with --cwd: {error}"),
            Some(to_worktree.to_string()),
            None,
        )
    })?;
    if destination != cwd {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "to_worktree",
            "--cwd and --to-worktree must resolve to the same linked task worktree",
            Some(to_worktree.to_string()),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn validate_logical_worktree_handle_path_relationship(
    cwd: &Path,
    handle: &str,
) -> homeboy::core::Result<()> {
    let basename = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cwd",
                "--cwd must have a UTF-8 worktree basename",
                Some(cwd.display().to_string()),
                None,
            )
        })?;
    if handle == basename {
        return Ok(());
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "to_worktree",
        "--cwd and --to-worktree must name the same linked task worktree",
        Some(handle.to_string()),
        None,
    ))
}

#[derive(Clone)]
pub(crate) struct ResolvedCookRepository {
    pub(crate) component: homeboy::core::component::Component,
    pub(super) root: PathBuf,
    pub(super) remote: String,
    pub(super) source_checkout: PathBuf,
}

/// Preview and execution infer the lint gate only after PR/default base
/// resolution, so the gate is scoped to the Cook's final base.
pub(crate) fn resolve_cook_destination_with_component_lint_gate(
    args: AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
    retained_gate_contract: bool,
) -> homeboy::core::Result<AgentTaskCookArgs> {
    let mut args = resolve_cook_destination(args, repository)?;
    apply_component_lint_gate(
        &mut args,
        retained_gate_contract,
        repository.map(|repo| &repo.component),
    )?;
    Ok(args)
}

pub(crate) fn resolve_cook_destination(
    mut args: AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
) -> homeboy::core::Result<AgentTaskCookArgs> {
    if let Some(cwd) = args.dispatch.cwd.as_deref() {
        let path = std::fs::canonicalize(cwd).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "cwd",
                format!("cannot use --cwd checkout: {error}"),
                Some(cwd.to_string()),
                None,
            )
        })?;
        // Check registry admission by exact filesystem identity, but never turn
        // the operator's checkout into a handle to be resolved again in replay.
        homeboy::core::worktree_provider::resolve_native_worktree_mutation_target_by_path(&path)?;
        if let Some(destination) = args.to_worktree.as_deref() {
            validate_cook_cwd_destination_identity(&path, destination)?;
        }
        let path = path.display().to_string();
        args.dispatch.cwd = Some(path.clone());
        args.to_worktree = Some(path);
        resolve_cook_issue(&mut args)?;
        resolve_cook_pull_request(&mut args)?;
        resolve_cook_base(&mut args, repository)?;
        return Ok(args);
    }
    if let Some(to_worktree) = args.to_worktree.as_deref() {
        let path = Path::new(to_worktree);
        if path.is_dir() {
            if let Some(target) =
                homeboy::core::worktree_provider::resolve_native_worktree_mutation_target_by_path(
                    path,
                )?
            {
                args.to_worktree = Some(target.handle);
            }
        }
    }
    resolve_cook_issue(&mut args)?;
    resolve_cook_pull_request(&mut args)?;
    if let Some(handle) = args.to_worktree.as_deref() {
        if !Path::new(handle).is_dir()
            && homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(handle)?
                .is_none()
        {
            let (_, head) = cook_missing_destination_requirements(&args)?;
            args.head = Some(head);
        }
        resolve_cook_base(&mut args, repository)?;
        return Ok(args);
    }
    let repo = args.dispatch.repo.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--repo <repo> is required when --to-worktree is omitted".to_string(),
        ])
    })?;
    // The task URL names the branch only when --head is omitted.
    let task_url = args
        .dispatch
        .task_url
        .as_deref()
        .map(normalize_cook_task_url);
    if task_url.is_none() && args.head.is_none() {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--task-url <url> or --head <branch> is required when --to-worktree is omitted"
                .to_string(),
        ]));
    }
    args.dispatch.task_url = task_url.clone();
    // Resolve the requested branch before task discovery. An explicit --head is
    // authoritative through candidate reuse, provisioning, and finalization.
    let handle_repository = cook_component_id(&args).unwrap_or(repo).to_string();
    // Admission resolved the repository once (#16365); only an unregistered
    // checkout falls back to the requested path.
    let requested = cook_provision_repository(&args);
    let source_checkout = repository
        .map(|repository| repository.source_checkout.as_path())
        .or_else(|| {
            requested
                .as_deref()
                .map(Path::new)
                .filter(|path| path.is_dir())
        });
    let head = match args.head.clone() {
        Some(head) => head,
        None => fresh_derived_cook_branch(
            &handle_repository,
            &derived_cook_branch(
                task_url
                    .as_deref()
                    .expect("task URL checked when --head is omitted"),
            )?,
            source_checkout,
        )?,
    };
    if args.issue_resolution.is_some() {
        args.issue_resolution.as_mut().unwrap()["head"] = json!(head);
    }
    let handle = homeboy::core::worktree_provider::native_handle_for_branch(
        requested.as_deref().unwrap_or(repo),
        &head,
    )?;
    args.to_worktree = Some(handle.clone());
    let derived = args.head.is_none();
    if derived {
        args.head = Some(head.clone());
    }
    resolve_cook_base(&mut args, repository)?;
    Ok(args)
}

pub(super) fn resolve_cook_base(
    args: &mut AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
) -> homeboy::core::Result<()> {
    let workspace = args
        .dispatch
        .workspace
        .as_deref()
        .or(args.dispatch.cwd.as_deref())
        .map(Path::new);
    let component = repository.map(|repo| Path::new(&repo.component.local_path));
    let destination = match args.to_worktree.as_deref() {
        Some(path) if Path::new(path).is_dir() => Some(PathBuf::from(path)),
        Some(handle) => {
            homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(handle)?
                .map(|target| target.path)
        }
        None => None,
    };
    let destination = destination.as_deref();
    let resolution = if let Some(base) = &args.base {
        super::default_branch::DefaultBranchResolution {
            base: base.clone(),
            from: format!("origin/{base}"),
            sha: None,
            source: "explicit".to_string(),
            evidence_path: None,
            precedence: vec!["explicit_base"],
        }
    } else {
        super::default_branch::resolve_default_branch(
            DefaultBranchRequest {
                explicit_base: None,
                explicit_from: None,
                workspace,
                component,
                destination,
            },
            repository.map(|repo| {
                (
                    Path::new(&repo.component.local_path),
                    repo.root.as_path(),
                    repo.remote.as_str(),
                )
            }),
        )?
    };
    args.base = Some(resolution.base.clone());
    args.base_resolution = Some(serde_json::to_value(resolution).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "serialize Cook default-branch resolution: {error}"
        ))
    })?);
    if args.head.as_deref() == args.base.as_deref() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "head",
            "Cook PR head must differ from its base",
            args.head.clone(),
            None,
        ));
    }
    Ok(())
}

/// Run one `gh` lookup that resolves the Cook's `task_url`. GitHub server and
/// network faults (5xx, TLS or connection timeouts) are retried through the
/// shared GitHub retry. Only "not found" is the caller's bad `task_url`; any
/// other failure that outlasts the retry means GitHub was unreachable, so it is
/// reported as retryable rather than as invalid input.
fn task_url_github_lookup(
    mut command: impl FnMut() -> std::process::Command,
    what: &str,
    url: &str,
) -> homeboy::core::Result<std::process::Output> {
    let output = homeboy::core::git::retry_transient_github_failures(|| {
        command().output().map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(format!("gh {what}")))
        })
    })?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if ["Could not resolve", "Not Found", "HTTP 404"]
        .iter()
        .any(|missing| stderr.contains(missing))
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            format!("could not resolve {what}: {stderr}"),
            Some(url.to_string()),
            None,
        ));
    }
    Err(homeboy::core::Error::new(
        homeboy::core::ErrorCode::RemoteCommandFailed,
        format!("GitHub was unreachable while resolving {what}; retry the Cook: {stderr}"),
        serde_json::json!({ "command": format!("gh {what}"), "task_url": url, "stderr": stderr }),
    )
    .with_retryable(true))
}

/// Open pull requests GitHub links to close the issue: a closing keyword or a
/// Development-sidebar link. A pull request that only mentions the issue is
/// not one, nor is one already merged or closed.
pub(super) fn issue_closing_pull_requests(
    owner: &str,
    repo: &str,
    number: &str,
    url: &str,
) -> homeboy::core::Result<Vec<String>> {
    const QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){issue(number:$number){closedByPullRequestsReferences(first:20,includeClosedPrs:false){nodes{url state}}}}}";
    let output = task_url_github_lookup(
        || {
            let mut command = std::process::Command::new("gh");
            command.args([
                "api",
                "graphql",
                "-f",
                &format!("query={QUERY}"),
                "-f",
                &format!("owner={owner}"),
                "-f",
                &format!("repo={repo}"),
                "-F",
                &format!("number={number}"),
                "--jq",
                ".data.repository.issue.closedByPullRequestsReferences.nodes",
            ]);
            command
        },
        "issue's linked PRs",
        url,
    )?;
    // `includeClosedPrs:false` still returns merged PRs; only an open PR is a
    // candidate this Cook can rework.
    #[derive(serde::Deserialize)]
    struct Node {
        url: String,
        state: String,
    }
    let nodes: Vec<Node> = serde_json::from_slice(&output.stdout)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    Ok(nodes
        .into_iter()
        .filter(|node| node.state == "OPEN")
        .map(|node| node.url)
        .collect())
}

/// Resolve linked PRs from the issue, not from a guessed branch name.
pub(super) fn resolve_cook_issue(args: &mut AgentTaskCookArgs) -> homeboy::core::Result<()> {
    let Some(url) = args
        .dispatch
        .task_url
        .as_deref()
        .filter(|url| url.starts_with("https://github.com/") && url.contains("/issues/"))
    else {
        return Ok(());
    };
    let url = normalize_cook_task_url(url);
    derived_cook_branch(&url)?;
    let path = url.strip_prefix("https://github.com/").ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "expected a GitHub issue URL",
            Some(url.clone()),
            None,
        )
    })?;
    let mut parts = path.splitn(4, '/');
    let (Some(owner), Some(repo), Some(_), Some(number)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "expected a GitHub issue URL",
            Some(url.clone()),
            None,
        ));
    };
    let prs: std::collections::BTreeSet<String> =
        issue_closing_pull_requests(owner, repo, number, &url)?
            .into_iter()
            .collect();
    if prs.len() > 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            format!(
                "issue has several open linked PRs; choose one with --task-url <PR URL>: {}",
                prs.into_iter().collect::<Vec<_>>().join(", ")
            ),
            Some(url),
            None,
        ));
    }
    if let Some(pr) = prs.into_iter().next() {
        args.issue_resolution = Some(json!({"route": "pr_rework", "issue_url": url, "pr_url": pr}));
        args.dispatch.task_url = Some(pr);
    } else {
        args.issue_resolution = Some(json!({"route": "fresh_branch", "issue_url": url}));
    }
    Ok(())
}

/// Handles are identities, not directories; missing destinations must not become
/// subprocess working directories or remote lookup paths.
pub(super) fn cook_pull_request_checkout(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<Option<PathBuf>> {
    if let Some(workspace) = args.dispatch.workspace.as_deref() {
        if Path::new(workspace).is_dir() {
            return Ok(Some(PathBuf::from(workspace)));
        }
        if let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(workspace)?
        {
            let path = PathBuf::from(record.path());
            if path.is_dir() {
                return Ok(Some(path));
            }
        }
    }
    Ok(args
        .dispatch
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            cook_provision_repository(args)
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
        }))
}

pub(super) fn resolve_cook_pull_request(args: &mut AgentTaskCookArgs) -> homeboy::core::Result<()> {
    let Some(url) = args
        .dispatch
        .task_url
        .as_deref()
        .filter(|url| url.contains("/pull/"))
    else {
        return Ok(());
    };
    let path = cook_pull_request_checkout(args)?;
    // A pull request in another repository is context for the task, not the
    // pull request this Cook updates, so its base and head do not apply.
    // Compare repository names so a fork-first checkout still matches.
    let repository_name = |url: &str| {
        homeboy::core::git::release_download::parse_github_url(url)
            .map(|repo| repo.repo.to_ascii_lowercase())
    };
    // The resolved repository's remote names the repository this Cook updates;
    // without one, only a supplied checkout (never the process cwd) attests it.
    let checkout = args
        .repository_identity
        .as_ref()
        .and_then(|identity| identity.get("remote_identity"))
        .and_then(Value::as_str)
        .and_then(|remote| remote.trim_end_matches(".git").rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            path.as_deref()
                .and_then(homeboy::core::git::remote_origin_url)
                .as_deref()
                .and_then(repository_name)
        });
    if let (Some(task), Some(checkout)) = (repository_name(url), checkout) {
        if task != checkout {
            return Ok(());
        }
    }
    let (base, head) = cook_pull_request_refs(path.as_deref(), url)?;
    for (flag, explicit, canonical) in [("base", &args.base, &base), ("head", &args.head, &head)] {
        if explicit.as_deref().is_some_and(|value| value != canonical) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                flag,
                format!("explicit Cook --{flag} must match the existing pull request {flag}"),
                explicit.clone(),
                None,
            ));
        }
    }
    // Without a supplied checkout, the PR head determines the provider handle.
    if args.dispatch.workspace.is_none() && args.dispatch.cwd.is_none() {
        if let Some(repo) = cook_provision_repository(args) {
            let handle = homeboy::core::worktree_provider::native_handle_for_branch(&repo, &head)?;
            if args
                .to_worktree
                .as_deref()
                .is_some_and(|value| value != handle)
            {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "to_worktree",
                    "explicit Cook --to-worktree must match the existing pull request head",
                    args.to_worktree.clone(),
                    None,
                ));
            }
            args.to_worktree = Some(handle);
        }
    }
    args.base = Some(base);
    args.head = Some(head);
    Ok(())
}

pub(super) fn cook_pull_request_refs(
    path: Option<&Path>,
    url: &str,
) -> homeboy::core::Result<(String, String)> {
    let output = task_url_github_lookup(
        || {
            let mut command = std::process::Command::new("gh");
            command.args(["pr", "view", url, "--json", "baseRefName,headRefName"]);
            // Full GitHub URLs identify their repository without a checkout.
            // Even for relative PR identifiers, never pass a missing path to
            // process spawning.
            if !url.starts_with("https://github.com/") {
                if let Some(path) = path.filter(|path| path.is_dir()) {
                    command.current_dir(path);
                }
            }
            command
        },
        "canonical pull request refs",
        url,
    )?;
    let refs: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    let base = refs["baseRefName"]
        .as_str()
        .filter(|value| !value.trim().is_empty());
    let head = refs["headRefName"]
        .as_str()
        .filter(|value| !value.trim().is_empty());
    match (base, head) {
        (Some(base), Some(head)) => Ok((base.to_string(), head.to_string())),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "pull request lookup returned no canonical base/head refs",
            Some(url.to_string()),
            None,
        )),
    }
}

pub(super) fn validate_cook_base_before_provisioning(
    args: &AgentTaskCookArgs,
    repository: Option<&ResolvedCookRepository>,
) -> homeboy::core::Result<()> {
    let Some(base) = args.base.as_deref() else {
        return Ok(());
    };
    let path = args
        .dispatch
        .workspace
        .as_deref()
        .or(args.dispatch.cwd.as_deref())
        .map(PathBuf::from)
        .or_else(|| {
            args.to_worktree
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
        })
        .or_else(|| repository.map(|repo| PathBuf::from(&repo.component.local_path)));
    let Some(path) = path else {
        return Ok(());
    };
    let remote = repository
        .filter(|repo| Path::new(&repo.component.local_path) == path || repo.root == path)
        .map(|repo| repo.remote.clone())
        .unwrap_or_else(|| homeboy::core::git::resolve_default_remote(&path));
    let remote_base = format!("{remote}/{base}");
    if homeboy::core::git::output_optional(
        &path,
        &["rev-parse", "--verify", "--quiet", &remote_base],
    )
    .is_some()
        || homeboy::core::git::output_optional(&path, &["rev-parse", "--verify", "--quiet", base])
            .is_some()
    {
        return Ok(());
    }
    let replay_argv = homeboy::core::git::default_branch_name(&path)
        .map(|corrected| corrected_cook_base_replay_argv(args, &corrected));
    let mut error = homeboy::core::Error::validation_invalid_argument(
        "base",
        format!("resolved Cook base `{base}` is unavailable before worktree provisioning"),
        Some(base.to_string()),
        replay_argv.as_ref().map(|argv| {
            vec![format!(
                "Replay with the repository default base: {}",
                quote_args(argv)
            )]
        }),
    );
    if let Some(argv) = replay_argv {
        error.details["correction_argv"] = serde_json::json!(argv);
    }
    Err(error)
}

/// Build the correction as typed argv first. The rendered shell command is a
/// display projection only, so branch and worktree values never become syntax.
pub(crate) fn corrected_cook_base_replay_argv(
    args: &AgentTaskCookArgs,
    corrected_base: &str,
) -> Vec<String> {
    let mut corrected = args.clone();
    corrected.base = Some(corrected_base.to_string());
    cook_replay_argv(&corrected)
}

/// Preview must report the same fail-closed first-provider admission when the
/// resolved local checkout can be inspected without mutating it.
pub(super) fn project_preview_dirty_admission(provision: &mut Value) {
    let Some(path) = provision.get("path").and_then(Value::as_str) else {
        return;
    };
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(path)
        .output();
    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let mut tracked = 0;
    let mut staged = 0;
    let mut untracked = 0;
    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == b'\0') {
        if entry.len() < 2 {
            continue;
        }
        if entry.len() > 3 {
            files.push(String::from_utf8_lossy(&entry[3..]).into_owned());
        }
        if entry.starts_with(b"??") {
            untracked += 1;
        } else {
            staged += usize::from(entry[0] != b' ');
            tracked += usize::from(entry[1] != b' ');
        }
    }
    if tracked + staged + untracked > 0 {
        provision["admission"] = serde_json::json!({
            "status": "would_refuse_dirty_candidate",
            "reason": "Cook requires attributable changes or a clean destination before provider execution",
            "files": files,
            "recovery_command": "git stash push --include-untracked",
            "changes": { "tracked": tracked, "staged": staged, "untracked": untracked },
        });
    }
}
