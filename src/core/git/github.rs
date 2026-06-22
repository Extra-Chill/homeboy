//! Component-aware GitHub primitives: issue and PR CRUD via the `gh` CLI.
//!
//! Shells out to `gh` (no new deps), mirroring the existing pattern used by
//! `core/release/executor::run_github_release`. All operations are scoped to a
//! component ID — the component's `remote_url` (or `git remote get-url origin`
//! fallback) resolves the GitHub owner/repo automatically.
//!
//! # Why this lives in `core/git`
//!
//! These operations are component-scoped git-graph operations, same shape as
//! `git commit`, `git push`, `git tag`. Grouping them under `git` keeps the
//! CLI surface coherent (`homeboy git issue create`, `homeboy git pr create`)
//! and reuses the existing `resolve_target` component → path resolution.
//!
//! # Error model
//!
//! When `gh` is missing, not authenticated, or fails, these functions return
//! a structured error with recovery hints. Callers get a real failure instead
//! of a silent skip — different from `run_github_release`, which soft-fails
//! because the tag is already pushed by that point.

use std::path::Path;
use std::process::Command;

use crate::core::component;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};

use super::gh_client::GhClient;
pub use super::github_types::{
    GithubFindItem, GithubFindOutput, GithubIssueOutput, GithubPrCheckRollup, GithubPrFleetItem,
    GithubPrFleetOutput, GithubPrFleetSummary, GithubPrOutput, GithubPrReadinessOutput,
    GithubPrView, IssueCloseOptions, IssueCloseReason, IssueCommentOptions, IssueCreateOptions,
    IssueEditOptions, IssueFindOptions, IssueState, PrCreateOptions, PrEditOptions, PrFindOptions,
    PrFleetOptions, PrMergeOptions, PrMergeReadiness, PrMergeabilityGitEvidence,
    PrMergeabilityGithubEvidence, PrMergeabilityReconcileOptions, PrMergeabilityReconcileOutput,
    PrReadinessBlocker, PrState,
};
use super::{resolve_target, run_git, run_git_output};

// ---------------------------------------------------------------------------
// Public API — issue
// ---------------------------------------------------------------------------

/// Create a new issue on the component's GitHub repository.
pub fn issue_create(
    component_id: Option<&str>,
    options: IssueCreateOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "title",
            "Issue title is required",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "create".into(),
        "-R".into(),
        repo_flag.clone(),
        "--title".into(),
        options.title.clone(),
        "--body".into(),
        options.body.clone(),
    ];
    for label in &options.labels {
        args.push("--label".into());
        args.push(label.clone());
    }

    let output = run_gh(&args)?;
    let url = output.trim().to_string();
    let number = parse_issue_number_from_url(&url);

    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.create".to_string(),
        success: true,
        number,
        url: Some(url),
        title: Some(options.title),
        state: Some("open".to_string()),
    })
}

/// Post a comment on an existing issue.
pub fn issue_comment(
    component_id: Option<&str>,
    options: IssueCommentOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "issue".into(),
        "comment".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        "--body".into(),
        options.body.clone(),
    ];

    let output = run_gh(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.comment".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: None,
        state: None,
    })
}

/// Close an existing issue with a typed reason.
///
/// `gh issue close --reason` accepts `completed | not planned | duplicate`.
/// We expose the two semantically-meaningful values via [`IssueCloseReason`];
/// `duplicate` is a special-case of "not planned" and not modeled here. Use
/// [`IssueCloseOptions::comment`] to leave a closing comment in the same
/// invocation (mirrors `gh issue close --comment`).
pub fn issue_close(
    component_id: Option<&str>,
    options: IssueCloseOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "close".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        "--reason".into(),
        options.reason.as_gh_flag().to_string(),
    ];
    if let Some(comment) = &options.comment {
        args.push("--comment".into());
        args.push(comment.clone());
    }

    let _ = run_gh(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.close".to_string(),
        success: true,
        number: Some(options.number),
        url: None,
        title: None,
        state: Some("closed".to_string()),
    })
}

/// Edit an existing issue's title, body, or labels.
///
/// At least one of `title`, `body`, `add_labels`, or `remove_labels` must be
/// provided. Mirrors `gh issue edit <n> [--title ...] [--body ...]
/// [--add-label ...] [--remove-label ...]`. Used by `homeboy issues reconcile`
/// to refresh the body of existing issues (open OR closed) so the latest
/// finding count and run link stay visible without duplicating the issue.
pub fn issue_edit(
    component_id: Option<&str>,
    options: IssueEditOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.is_none()
        && options.body.is_none()
        && options.add_labels.is_empty()
        && options.remove_labels.is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "title/body/labels",
            "At least one of --title, --body, --add-label, or --remove-label must be provided",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "edit".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    if let Some(title) = &options.title {
        args.push("--title".into());
        args.push(title.clone());
    }
    if let Some(body) = &options.body {
        args.push("--body".into());
        args.push(body.clone());
    }
    for label in &options.add_labels {
        args.push("--add-label".into());
        args.push(label.clone());
    }
    for label in &options.remove_labels {
        args.push("--remove-label".into());
        args.push(label.clone());
    }

    let output = run_gh(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.edit".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: options.title,
        state: None,
    })
}

/// Find issues matching the given filter. Useful for dedup before creating.
///
/// Uses `gh issue list --json number,title,body,url,state,stateReason,closedAt,labels`
/// and filters locally (title and label conjunctions are simpler to enforce
/// client-side than via the gh search syntax).
pub fn issue_find(
    component_id: Option<&str>,
    options: IssueFindOptions,
) -> Result<GithubFindOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let limit = if options.limit == 0 {
        30
    } else {
        options.limit
    };
    let mut args: Vec<String> = vec![
        "issue".into(),
        "list".into(),
        "-R".into(),
        repo_flag,
        "--state".into(),
        options.state.as_gh_flag().to_string(),
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,body,url,state,stateReason,closedAt,labels".into(),
    ];
    // Pass labels through gh to narrow the server-side result set; we still
    // enforce the exact label-set conjunction locally in case gh changes the
    // semantics of --label (currently: all-of).
    for label in &options.labels {
        args.push("--label".into());
        args.push(label.clone());
    }

    let raw = run_gh(&args)?;
    let items = parse_issue_list_json(&raw, &options)?;

    Ok(GithubFindOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.find".to_string(),
        success: true,
        items,
    })
}

// ---------------------------------------------------------------------------
// Public API — pull request
// ---------------------------------------------------------------------------

/// Open a new pull request.
pub fn pr_create(component_id: Option<&str>, options: PrCreateOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "title",
            "PR title is required",
            None,
            None,
        ));
    }
    if options.base.trim().is_empty() || options.head.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "base/head",
            "PR base and head branches are required",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "create".into(),
        "-R".into(),
        repo_flag.clone(),
        "--base".into(),
        options.base.clone(),
        "--head".into(),
        options.head.clone(),
        "--title".into(),
        options.title.clone(),
        "--body".into(),
        options.body.clone(),
    ];
    if options.draft {
        args.push("--draft".into());
    }

    let output = run_gh(&args)?;
    let url = output.trim().to_string();
    let number = parse_issue_number_from_url(&url);

    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.create".to_string(),
        success: true,
        number,
        url: Some(url),
        title: Some(options.title),
        state: Some("open".to_string()),
        base: Some(options.base),
        head: Some(options.head),
        ..Default::default()
    })
}

/// Edit an existing pull request's title and/or body.
pub fn pr_edit(component_id: Option<&str>, options: PrEditOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.is_none() && options.body.is_none() {
        return Err(Error::validation_invalid_argument(
            "title/body",
            "At least one of --title or --body must be provided",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "edit".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    if let Some(title) = &options.title {
        args.push("--title".into());
        args.push(title.clone());
    }
    if let Some(body) = &options.body {
        args.push("--body".into());
        args.push(body.clone());
    }

    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.edit".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: options.title,
        ..Default::default()
    })
}

/// Find PRs matching the given filter.
pub fn pr_find(component_id: Option<&str>, options: PrFindOptions) -> Result<GithubFindOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let limit = if options.limit == 0 {
        30
    } else {
        options.limit
    };
    let mut args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "-R".into(),
        repo_flag,
        "--state".into(),
        options.state.as_gh_flag().to_string(),
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,url,state,baseRefName,headRefName".into(),
    ];
    if let Some(base) = &options.base {
        args.push("--base".into());
        args.push(base.clone());
    }
    if let Some(head) = &options.head {
        args.push("--head".into());
        args.push(head.clone());
    }

    let raw = run_gh(&args)?;
    let items = parse_pr_list_json(&raw)?;

    Ok(GithubFindOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.find".to_string(),
        success: true,
        items,
    })
}

/// Find PRs that contain a commit SHA.
///
/// GitHub indexes commit SHAs in PR search, which makes this the shared helper
/// for read-only stack/branch inspection flows that need to decorate commits.
pub fn pr_find_by_commit(
    repo_path: &Path,
    sha: &str,
    repo: Option<&str>,
    limit: usize,
) -> Result<Vec<GithubFindItem>> {
    ensure_gh_ready()?;

    let mut args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "--search".into(),
        sha.to_string(),
        "--state".into(),
        "all".into(),
        "--json".into(),
        "number,state,title,url".into(),
        "--limit".into(),
        limit.to_string(),
    ];
    if let Some(repo) = repo {
        args.push("-R".into());
        args.push(repo.to_string());
    }

    let output = Command::new("gh")
        .args(&args)
        .current_dir(repo_path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr list --search: {} (is `gh` installed and authenticated?)",
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr list --search {}: {}",
            sha,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pr_list_json(&stdout)
}

/// Fetch metadata for one PR.
pub fn pr_view(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<GithubPrView> {
    let (id, repo) = resolve_component_github(component_id, path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "-R".into(),
        repo_flag,
        "--json".into(),
        "author,baseRefName,headRefName,headRepository,title,url,state,isDraft,mergedAt,headRefOid,reviewDecision,mergeStateStatus,statusCheckRollup".into(),
    ];
    let raw = run_gh(&args)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse gh pr view JSON: {}", e),
            Some(raw.clone()),
        )
    })?;
    let author = parsed
        .pointer("/author/login")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let base = parsed
        .get("baseRefName")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let head = parsed
        .get("headRefName")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let head_repository = parsed
        .pointer("/headRepository/nameWithOwner")
        .or_else(|| parsed.pointer("/headRepository/name"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let state = string_value(&parsed, "state").unwrap_or_default();
    let url = string_value(&parsed, "url").unwrap_or_default();
    let title = string_value(&parsed, "title");
    let draft = parsed
        .get("isDraft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let head_sha = string_value(&parsed, "headRefOid");
    let merged_at = string_value(&parsed, "mergedAt");
    let review_decision = string_value(&parsed, "reviewDecision");
    let merge_state = string_value(&parsed, "mergeStateStatus");
    let status_check_rollup = parsed
        .get("statusCheckRollup")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let (ci_state, ci_summary, ci_next_action) = classify_pr_ci(
        &state,
        merged_at.as_deref(),
        merge_state.as_deref(),
        status_check_rollup,
    );

    Ok(GithubPrView {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        number,
        url,
        title,
        state,
        draft,
        author,
        base,
        head,
        head_repository,
        head_sha,
        merged_at,
        review_decision,
        merge_state,
        ci_state,
        ci_summary,
        ci_next_action,
    })
}

/// Explain whether a PR is ready to merge without attempting a merge.
pub fn pr_readiness(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<GithubPrReadinessOutput> {
    let pr = pr_view(component_id, number, path)?;
    let readiness = interpret_pr_merge_readiness(
        pr.merge_state.as_deref(),
        &pr.ci_state,
        &pr.ci_summary,
        pr.review_decision.as_deref(),
        pr.draft,
    );

    Ok(GithubPrReadinessOutput {
        component_id: pr.component_id,
        owner: pr.owner,
        repo: pr.repo,
        action: "pr.readiness".to_string(),
        success: true,
        number: pr.number,
        url: pr.url,
        title: pr.title,
        state: pr.state,
        draft: pr.draft,
        review_decision: pr.review_decision,
        ci_state: pr.ci_state,
        ci_summary: pr.ci_summary,
        readiness,
    })
}

/// Report and optionally land a fleet of PRs.
pub fn pr_fleet(
    component_id: Option<&str>,
    options: PrFleetOptions,
) -> Result<GithubPrFleetOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let merge_method = validate_pr_merge_method(&options.merge_method)?;

    let mut items = Vec::new();
    for input in &options.refs {
        let parsed = match parse_pr_fleet_ref(input, &repo.owner, &repo.repo) {
            Ok(number) => number,
            Err(error) => {
                items.push(pr_fleet_error_item(input, None, error));
                continue;
            }
        };

        let mut view = match pr_view(Some(&id), parsed, options.path.clone()) {
            Ok(view) => view,
            Err(error) => {
                items.push(pr_fleet_error_item(input, Some(parsed), error.to_string()));
                continue;
            }
        };
        let mut updated = false;

        if options.update_branches && pr_fleet_should_update_branch(&view) {
            let args: Vec<String> = vec![
                "pr".into(),
                "update-branch".into(),
                parsed.to_string(),
                "-R".into(),
                repo_flag.clone(),
            ];
            match run_gh(&args) {
                Ok(_) => {
                    updated = true;
                    view = pr_view(Some(&id), parsed, options.path.clone())?;
                }
                Err(error) => {
                    let mut item = pr_fleet_item(input, &view, GithubPrCheckRollup::default());
                    item.required_action = "update_branch_failed".to_string();
                    item.error = Some(error.to_string());
                    items.push(item);
                    continue;
                }
            }
        }

        let rollup = pr_fleet_check_rollup(&repo_flag, parsed)?;
        let mut item = pr_fleet_item(input, &view, rollup);
        item.updated = updated;

        if options.apply && item.mergeable {
            let args: Vec<String> = vec![
                "pr".into(),
                "merge".into(),
                parsed.to_string(),
                "-R".into(),
                repo_flag.clone(),
                format!("--{}", merge_method),
            ];
            match run_gh(&args) {
                Ok(_) => {
                    item.merged = true;
                    item.required_action = "merged".to_string();
                }
                Err(error) => {
                    item.required_action = "merge_failed".to_string();
                    item.error = Some(error.to_string());
                }
            }
        }

        items.push(item);
    }

    let summary = summarize_pr_fleet(&items);
    let success = summary.errors == 0;
    Ok(GithubPrFleetOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.fleet".to_string(),
        success,
        apply: options.apply,
        update_branches: options.update_branches,
        summary,
        items,
    })
}

/// Compare GitHub's PR mergeability state with local `git merge-tree` evidence.
pub fn pr_reconcile_mergeability(
    component_id: Option<&str>,
    options: PrMergeabilityReconcileOptions,
) -> Result<PrMergeabilityReconcileOutput> {
    let view = pr_view(component_id, options.number, options.path.clone())?;
    let (_id, repo_path) = resolve_target(component_id, options.path.as_deref())?;
    let repo_path = Path::new(&repo_path);

    let base_ref = format!("origin/{}", view.base);
    let head_ref = format!("pull/{}/head", view.number);
    let base_sha = fetch_ref_sha(repo_path, &view.base)?;
    let head_sha = fetch_ref_sha(repo_path, &head_ref)?;
    let merge_tree = run_git_output(
        repo_path,
        &["merge-tree", "--write-tree", &base_sha, &head_sha],
        "git merge-tree",
    )?;
    let merge_tree_stdout = String::from_utf8_lossy(&merge_tree.stdout)
        .trim()
        .to_string();
    let merge_tree_stderr = String::from_utf8_lossy(&merge_tree.stderr)
        .trim()
        .to_string();
    let merge_tree_clean = merge_tree.status.success();
    let head_matches_github = view
        .head_sha
        .as_ref()
        .map(|github_sha| github_sha.eq_ignore_ascii_case(&head_sha));
    let github_merge_state = view.merge_state.as_deref().unwrap_or_default();
    let (classification, recommended_action) =
        classify_mergeability_reconcile(merge_tree_clean, github_merge_state, head_matches_github);

    Ok(PrMergeabilityReconcileOutput {
        component_id: view.component_id,
        owner: view.owner,
        repo: view.repo,
        action: "pr.reconcile_mergeability".to_string(),
        number: view.number,
        classification: classification.to_string(),
        recommended_action: recommended_action.to_string(),
        github: PrMergeabilityGithubEvidence {
            state: view.state,
            base: view.base,
            head: view.head,
            head_repository: view.head_repository,
            head_sha: view.head_sha,
            merge_state: view.merge_state,
            ci_state: view.ci_state,
            ci_summary: view.ci_summary,
        },
        git: PrMergeabilityGitEvidence {
            base_ref,
            base_sha,
            head_ref,
            head_sha,
            merge_tree_clean,
            merge_tree_exit_code: merge_tree.status.code(),
            merge_tree_stdout,
            merge_tree_stderr,
            head_matches_github,
        },
    })
}

fn fetch_ref_sha(repo_path: &Path, remote_ref: &str) -> Result<String> {
    run_git(
        repo_path,
        &["fetch", "--quiet", "origin", remote_ref],
        "git fetch",
    )?;
    Ok(
        run_git(repo_path, &["rev-parse", "FETCH_HEAD"], "git rev-parse")?
            .trim()
            .to_string(),
    )
}

fn classify_mergeability_reconcile(
    merge_tree_clean: bool,
    github_merge_state: &str,
    head_matches_github: Option<bool>,
) -> (&'static str, &'static str) {
    if head_matches_github == Some(false) {
        return ("github_stale", "wait");
    }
    if !merge_tree_clean {
        return ("real_conflict", "resolve_conflicts");
    }

    match github_merge_state.to_ascii_uppercase().as_str() {
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => ("clean", "proceed"),
        "BEHIND" => ("needs_update", "update_branch"),
        "DIRTY" | "UNKNOWN" | "" => ("github_stale", "wait"),
        _ => ("needs_update", "rebase_or_replace"),
    }
}

/// List changed files for one PR.
pub fn pr_files(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<Vec<String>> {
    let (_id, repo) = resolve_component_github(component_id, path.as_deref())?;
    ensure_gh_ready()?;
    let args: Vec<String> = vec![
        "api".into(),
        "--paginate".into(),
        format!("repos/{}/{}/pulls/{}/files", repo.owner, repo.repo, number),
        "--jq".into(),
        ".[].filename".into(),
    ];
    let raw = run_gh(&args)?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

/// Merge a PR with an explicit method.
pub fn pr_merge(component_id: Option<&str>, options: PrMergeOptions) -> Result<GithubPrOutput> {
    let method = validate_pr_merge_method(&options.method)?;
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "merge".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        format!("--{}", method),
    ];
    if options.delete_branch {
        args.push("--delete-branch".into());
    }
    run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.merge".to_string(),
        success: true,
        number: Some(options.number),
        state: Some("merged".to_string()),
        ..Default::default()
    })
}

fn validate_pr_merge_method(method: &str) -> Result<String> {
    match method {
        "merge" | "squash" | "rebase" => Ok(method.to_string()),
        other => Err(Error::validation_invalid_argument(
            "merge_method",
            format!("Unsupported merge method '{}'", other),
            Some("Use merge, squash, or rebase".to_string()),
            None,
        )),
    }
}

fn parse_pr_fleet_ref(input: &str, owner: &str, repo: &str) -> std::result::Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty PR reference".to_string());
    }
    if let Ok(number) = trimmed.parse::<u64>() {
        return Ok(number);
    }

    let path = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .and_then(|rest| rest.split_once('/').map(|(_, path)| path))
        .unwrap_or(trimmed);
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    match parts.as_slice() {
        [url_owner, url_repo, "pull", number] if *url_owner == owner && *url_repo == repo => {
            number
                .parse::<u64>()
                .map_err(|_| format!("invalid PR number in reference `{input}`"))
        }
        [url_owner, url_repo, "pull", _] => Err(format!(
            "PR reference `{input}` targets {url_owner}/{url_repo}, expected {owner}/{repo}"
        )),
        _ => Err(format!(
            "invalid PR reference `{input}`; use a PR number or https://github.com/{owner}/{repo}/pull/<number>"
        )),
    }
}

fn pr_fleet_should_update_branch(view: &GithubPrView) -> bool {
    view.state == "OPEN" && view.merge_state.as_deref() == Some("BEHIND")
}

fn pr_fleet_check_rollup(repo_flag: &str, number: u64) -> Result<GithubPrCheckRollup> {
    let args: Vec<String> = vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "-R".into(),
        repo_flag.to_string(),
        "--json".into(),
        "statusCheckRollup".into(),
    ];
    let raw = run_gh(&args)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse gh pr view statusCheckRollup JSON: {e}"),
            Some(raw.clone()),
        )
    })?;
    let checks = parsed
        .get("statusCheckRollup")
        .and_then(|value| value.as_array())
        .map(|value| value.as_slice())
        .unwrap_or(&[]);
    Ok(summarize_check_rollup(checks))
}

fn summarize_check_rollup(checks: &[serde_json::Value]) -> GithubPrCheckRollup {
    let mut rollup = GithubPrCheckRollup {
        total: checks.len(),
        ..Default::default()
    };
    for check in checks {
        let status = check.get("status").and_then(serde_json::Value::as_str);
        let conclusion = check
            .get("conclusion")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (status, conclusion) {
            (
                _,
                Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE"),
            ) => rollup.failed += 1,
            (Some("COMPLETED"), Some("SUCCESS" | "SKIPPED" | "NEUTRAL")) => rollup.passed += 1,
            (Some("COMPLETED"), Some(_)) | (Some("COMPLETED"), None) => rollup.unknown += 1,
            _ => rollup.pending += 1,
        }
    }
    rollup
}

fn pr_fleet_item(
    input: &str,
    view: &GithubPrView,
    check_rollup: GithubPrCheckRollup,
) -> GithubPrFleetItem {
    let stale_base = view.merge_state.as_deref() == Some("BEHIND");
    let conflicts = view.merge_state.as_deref() == Some("DIRTY");
    let mergeable = view.state == "OPEN"
        && view.merge_state.as_deref() == Some("CLEAN")
        && view.ci_state == "terminal_green";
    let required_action = if view.state != "OPEN" {
        "noop_closed"
    } else if conflicts {
        "resolve_conflicts"
    } else if stale_base {
        "update_branch"
    } else if view.ci_state == "pending" || view.ci_state == "no_checks" {
        "wait_for_checks"
    } else if view.ci_state != "terminal_green" {
        "fix_checks"
    } else if mergeable {
        "merge"
    } else {
        "review_required"
    };

    GithubPrFleetItem {
        input: input.to_string(),
        number: Some(view.number),
        url: Some(format!(
            "https://github.com/{}/{}/pull/{}",
            view.owner, view.repo, view.number
        )),
        state: Some(view.state.clone()),
        base: Some(view.base.clone()),
        head: Some(view.head.clone()),
        head_sha: view.head_sha.clone(),
        merge_state: view.merge_state.clone(),
        ci_state: Some(view.ci_state.clone()),
        ci_summary: Some(view.ci_summary.clone()),
        review_decision: view.review_decision.clone(),
        check_rollup,
        stale_base,
        conflicts,
        mergeable,
        required_action: required_action.to_string(),
        updated: false,
        merged: false,
        error: None,
    }
}

fn pr_fleet_error_item(input: &str, number: Option<u64>, error: String) -> GithubPrFleetItem {
    GithubPrFleetItem {
        input: input.to_string(),
        number,
        url: None,
        state: None,
        base: None,
        head: None,
        head_sha: None,
        merge_state: None,
        ci_state: None,
        ci_summary: None,
        review_decision: None,
        check_rollup: GithubPrCheckRollup::default(),
        stale_base: false,
        conflicts: false,
        mergeable: false,
        required_action: "error".to_string(),
        updated: false,
        merged: false,
        error: Some(error),
    }
}

fn summarize_pr_fleet(items: &[GithubPrFleetItem]) -> GithubPrFleetSummary {
    GithubPrFleetSummary {
        total: items.len(),
        mergeable: items.iter().filter(|item| item.mergeable).count(),
        merged: items.iter().filter(|item| item.merged).count(),
        updated: items.iter().filter(|item| item.updated).count(),
        blocked: items
            .iter()
            .filter(|item| !item.mergeable && item.error.is_none())
            .count(),
        errors: items.iter().filter(|item| item.error.is_some()).count(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve a component ID to its GitHub owner/repo via `remote_url` (or git fallback).
///
/// `path_override` lets callers point at an unregistered checkout (e.g. a CI
/// runner workspace with a portable `homeboy.json` but no global component
/// registry entry). When set, the component is discovered from the portable
/// config at that path instead of the global registry.
pub(super) fn resolve_component_github(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<(String, GitHubRepo)> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let comp = component::resolve_effective(Some(&id), path_override, None)?;

    let remote_url = comp
        .remote_url
        .clone()
        .or_else(|| detect_remote_url(Path::new(&path)))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "remote_url",
                format!(
                    "Component '{}' has no GitHub remote (remote_url not set and `git remote get-url origin` failed)",
                    id
                ),
                None,
                Some(vec![
                    "Set it: homeboy component set <id> --json '{\"remote_url\":\"https://github.com/<owner>/<repo>\"}'".to_string(),
                    "Or configure a git remote in the component's local_path".to_string(),
                    "Or pass --path <workspace> to discover from a portable homeboy.json".to_string(),
                ]),
            )
        })?;

    let repo = parse_github_url(&remote_url).ok_or_else(|| {
        Error::validation_invalid_argument(
            "remote_url",
            format!(
                "Remote URL '{}' is not a GitHub URL (only github.com is supported)",
                remote_url
            ),
            None,
            Some(vec![
                "Use an HTTPS (https://github.com/owner/repo) or SSH (git@github.com:owner/repo) URL".to_string(),
            ]),
        )
    })?;

    Ok((id, repo))
}

/// Run `gh <args>` swallowing stdout/stderr, return whether it exited successfully.
/// Used for probe-style `gh` invocations that only care about the exit code
/// (e.g. `gh --version`, `gh auth status`, `gh release view`).
///
/// Public so other modules can consolidate on one probe helper instead of
/// reimplementing the same `Command::new + null stdio + status` pattern.
pub fn gh_probe_succeeds(args: &[&str]) -> bool {
    Command::new("gh")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve a GitHub token for scripts that require `GH_TOKEN` explicitly.
///
/// Prefer the caller's environment, then fall back to the authenticated GitHub
/// CLI token so extension scripts do not fail late after Homeboy has already
/// verified that `gh` is usable.
pub fn github_token_from_env_or_gh() -> Option<String> {
    select_github_token(
        std::env::var("GH_TOKEN").ok(),
        std::env::var("GITHUB_TOKEN").ok(),
        gh_auth_token,
    )
}

fn select_github_token(
    gh_token: Option<String>,
    github_token: Option<String>,
    gh_auth_token: impl FnOnce() -> Option<String>,
) -> Option<String> {
    gh_token
        .and_then(non_empty_token)
        .or_else(|| github_token.and_then(non_empty_token))
        .or_else(gh_auth_token)
}

fn non_empty_token(token: String) -> Option<String> {
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn gh_auth_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    non_empty_token(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Error out if `gh` is missing or unauthenticated. Unlike `run_github_release`
/// (which soft-fails because the tag is already pushed), primitive operations
/// have no already-committed side effect to preserve — fail loudly.
pub(super) fn ensure_gh_ready() -> Result<()> {
    let host = std::env::var("GH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "github.com".to_string());
    GhClient::for_host(host).ensure_ready()
}

/// Run `gh <args>` and return stdout on success, or a structured error on
/// failure (with stderr captured in the error message).
pub(super) fn run_gh(args: &[String]) -> Result<String> {
    let host = std::env::var("GH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "github.com".to_string());
    GhClient::for_host(host).run(args)
}

fn parse_issue_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

fn string_value(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn classify_pr_ci(
    pr_state: &str,
    merged_at: Option<&str>,
    merge_state: Option<&str>,
    checks: &[serde_json::Value],
) -> (String, String, String) {
    if checks.is_empty() {
        return (
            "no_checks".to_string(),
            "GitHub reported no status checks for this PR head; next action: merge-ready"
                .to_string(),
            "merge_ready".to_string(),
        );
    }

    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut queued = 0usize;
    let mut running = 0usize;
    let mut pending = 0usize;
    let mut unknown = 0usize;
    let mut rerunnable = 0usize;
    let mut required = 0usize;
    let mut optional = 0usize;
    let mut failed_details = Vec::new();
    let mut pending_details = Vec::new();

    for check in checks {
        let name = check_name(check);
        let workflow = string_field(check, &["workflowName", "workflow_name"]);
        let status = check
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let conclusion = check
            .get("conclusion")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(is_required) = bool_field(check, &["isRequired", "required"]) {
            if is_required {
                required += 1;
            } else {
                optional += 1;
            }
        }

        match (status, conclusion) {
            (_, Some("FAILURE" | "ACTION_REQUIRED")) => {
                failed += 1;
                failed_details.push(check_detail(check, &name));
            }
            (_, Some("CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE")) => {
                failed += 1;
                rerunnable += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL")) => {
                passed += 1;
            }
            (Some("COMPLETED"), Some("SKIPPED")) => {
                skipped += 1;
            }
            (Some("COMPLETED"), Some(_)) => {
                unknown += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("COMPLETED"), None) => {
                unknown += 1;
                failed_details.push(check_detail(check, &name));
            }
            (Some("QUEUED" | "REQUESTED" | "WAITING"), _) => {
                queued += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
            (Some("IN_PROGRESS"), _) => {
                running += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
            _ => {
                pending += 1;
                pending_details.push(check_pending_detail(check, &name, workflow.as_deref()));
            }
        }
    }

    let blocked = failed + unknown;
    let waiting = queued + running + pending;
    let state = if failed > 0 || unknown > 0 {
        "terminal_failed"
    } else if waiting > 0 && (pr_state == "MERGED" || merged_at.is_some()) {
        "stale"
    } else if waiting > 0 {
        "pending"
    } else {
        "terminal_green"
    };

    let next_action = if blocked > 0 && failed == rerunnable && unknown == 0 {
        "rerun"
    } else if blocked > 0 {
        "inspect_failed_logs"
    } else if matches!(merge_state, Some("BEHIND")) {
        "update_branch"
    } else if waiting > 0 {
        "wait"
    } else {
        "merge_ready"
    };

    let mut parts = vec![format!(
        "{} reported check(s): {} passed, {} failed/unknown, {} queued, {} running, {} pending, {} skipped",
        checks.len(), passed, blocked, queued, running, pending, skipped
    )];
    if required > 0 || optional > 0 {
        parts.push(format!("{} required, {} optional", required, optional));
    } else {
        parts.push("required/optional split unavailable".to_string());
    }
    if let Some(oldest) = pending_details
        .iter()
        .filter_map(|detail| detail.started_at.as_deref())
        .min()
    {
        parts.push(format!("oldest pending since {}", oldest));
    }
    if !pending_details.is_empty() {
        parts.push(format!(
            "waiting: {}",
            pending_details
                .iter()
                .take(3)
                .map(PendingCheckDetail::label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !failed_details.is_empty() {
        parts.push(format!(
            "failed logs: {}",
            failed_details
                .iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let action_label = if next_action == "merge_ready" {
        "merge-ready".to_string()
    } else {
        next_action.replace('_', " ")
    };
    parts.push(format!("next action: {}", action_label));

    (state.to_string(), parts.join("; "), next_action.to_string())
}

struct PendingCheckDetail {
    name: String,
    workflow: Option<String>,
    started_at: Option<String>,
}

impl PendingCheckDetail {
    fn label(&self) -> String {
        match (&self.workflow, &self.started_at) {
            (Some(workflow), Some(started_at)) => {
                format!("{} ({}, since {})", self.name, workflow, started_at)
            }
            (Some(workflow), None) => format!("{} ({})", self.name, workflow),
            (None, Some(started_at)) => format!("{} (since {})", self.name, started_at),
            (None, None) => self.name.clone(),
        }
    }
}

fn check_pending_detail(
    check: &serde_json::Value,
    name: &str,
    workflow: Option<&str>,
) -> PendingCheckDetail {
    PendingCheckDetail {
        name: name.to_string(),
        workflow: workflow.map(str::to_string),
        started_at: string_field(check, &["startedAt", "started_at", "queuedAt", "queued_at"]),
    }
}

fn check_detail(check: &serde_json::Value, name: &str) -> String {
    match string_field(
        check,
        &[
            "detailsUrl",
            "details_url",
            "targetUrl",
            "target_url",
            "url",
        ],
    ) {
        Some(url) => format!("{} ({})", name, url),
        None => name.to_string(),
    }
}

fn check_name(check: &serde_json::Value) -> String {
    string_field(check, &["name", "context", "workflowName", "workflow_name"])
        .unwrap_or_else(|| "unnamed check".to_string())
}

fn string_field(check: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| check.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(check: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| check.get(*key).and_then(serde_json::Value::as_bool))
}

fn interpret_pr_merge_readiness(
    raw_merge_state: Option<&str>,
    ci_state: &str,
    ci_summary: &str,
    review_decision: Option<&str>,
    draft: bool,
) -> PrMergeReadiness {
    let normalized_merge_state = raw_merge_state
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase);
    let raw = normalized_merge_state.as_deref();
    let mut blockers = Vec::new();

    if draft {
        blockers.push(readiness_blocker(
            "draft",
            "PR is still a draft.",
            "Mark the PR ready for review before merging.",
        ));
    }

    if review_decision == Some("REVIEW_REQUIRED") {
        blockers.push(readiness_blocker(
            "review_required",
            "GitHub reports that review is required.",
            "Request or wait for the required approving review.",
        ));
    }

    let (interpreted_state, check_guidance, conflict_guidance) = match raw {
        Some("CLEAN") if ci_state == "terminal_green" => (
            "mergeable_now",
            "Required checks reported success for the current PR head.",
            "No conflict guidance; GitHub reports the PR branch is clean.",
        ),
        Some("CLEAN") if ci_state == "no_checks" => {
            blockers.push(readiness_blocker(
                "checks_not_reported",
                "GitHub reports CLEAN but statusCheckRollup is empty.",
                "Wait for checks to appear on the current head before treating the PR as mergeable.",
            ));
            (
                "waiting_on_required_checks",
                "GitHub has not reported checks for this head yet; this can happen immediately after a push or rebase.",
                "No conflict guidance; GitHub reports the PR branch is clean.",
            )
        }
        Some("CLEAN") | Some("BLOCKED") if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "required_checks_pending",
                "Required checks are still pending or GitHub has not finished branch-protection evaluation.",
                "Wait for required checks to complete, then re-run readiness.",
            ));
            (
                "waiting_on_required_checks",
                "Wait for pending required checks to complete.",
                "No conflict guidance unless GitHub later reports DIRTY or BEHIND.",
            )
        }
        Some("BLOCKED") if ci_state == "terminal_failed" => {
            blockers.push(readiness_blocker(
                "required_checks_failed",
                "Required checks failed or branch protection blocks merge.",
                "Open the PR checks view, fix failing required checks, then re-run readiness.",
            ));
            (
                "failing_required_checks",
                "Fix failing required checks before merging.",
                "No conflict guidance unless GitHub also reports DIRTY or BEHIND.",
            )
        }
        Some("UNSTABLE") if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "optional_checks_pending",
                "GitHub reports UNSTABLE while checks are pending.",
                "Wait for optional or non-blocking checks to finish if your workflow requires them.",
            ));
            (
                "waiting_on_optional_checks",
                "The PR may be mergeable by GitHub policy, but optional checks have not settled.",
                "No conflict guidance; UNSTABLE is a check signal, not a conflict signal.",
            )
        }
        Some("UNSTABLE") => {
            blockers.push(readiness_blocker(
                "optional_checks_unstable",
                "GitHub reports UNSTABLE; non-required checks are failing or inconclusive.",
                "Inspect the check run details and decide whether optional failures are acceptable.",
            ));
            (
                "failing_optional_checks",
                "Optional or non-required checks are failing or inconclusive.",
                "No conflict guidance; UNSTABLE is a check signal, not a conflict signal.",
            )
        }
        Some("DIRTY") => {
            blockers.push(readiness_blocker(
                "merge_conflicts",
                "GitHub reports merge conflicts with the base branch.",
                "Rebase or merge the base branch locally, resolve conflicts, push, then re-run readiness.",
            ));
            (
                "conflicted",
                "Check status is secondary until conflicts are resolved.",
                "Resolve merge conflicts against the base branch before merging.",
            )
        }
        Some("BEHIND") => {
            blockers.push(readiness_blocker(
                "branch_behind",
                "The PR branch is behind the base branch and must be updated.",
                "Update the branch from the base branch, push, then re-run readiness.",
            ));
            (
                "conflicted",
                "Checks may need to run again after the branch is updated.",
                "Update the PR branch with the base branch before merging.",
            )
        }
        Some("UNKNOWN") | None => {
            blockers.push(readiness_blocker(
                "mergeability_unknown",
                "GitHub has not computed mergeability for the current PR head yet.",
                "Wait briefly and re-run readiness; do not attempt a merge just to discover state.",
            ));
            (
                "unknown",
                "Check state alone is insufficient while mergeability is UNKNOWN.",
                "Conflict state is unknown until GitHub recomputes mergeability.",
            )
        }
        Some("HAS_HOOKS") => {
            blockers.push(readiness_blocker(
                "merge_hooks",
                "GitHub reports merge hooks must run before mergeability is final.",
                "Wait for repository hooks or branch rules to settle, then re-run readiness.",
            ));
            (
                "unknown",
                "Check state is not enough while merge hooks are pending.",
                "Conflict state is not final until hooks complete.",
            )
        }
        Some("BLOCKED") => {
            blockers.push(readiness_blocker(
                "branch_protection_blocked",
                "GitHub branch protection blocks merge.",
                "Inspect required reviews, required checks, conversations, and branch rules in GitHub.",
            ));
            (
                "failing_required_checks",
                "Branch protection is blocking merge; inspect required checks and rules.",
                "No conflict guidance unless GitHub also reports DIRTY or BEHIND.",
            )
        }
        Some(_) if ci_state == "terminal_failed" => {
            blockers.push(readiness_blocker(
                "checks_failed",
                "One or more checks failed or reported an unknown conclusion.",
                "Open the PR checks view, fix failures, then re-run readiness.",
            ));
            (
                "failing_required_checks",
                "Fix failing checks before merging.",
                "No conflict guidance unless GitHub reports DIRTY or BEHIND.",
            )
        }
        Some(_) if ci_state == "pending" => {
            blockers.push(readiness_blocker(
                "checks_pending",
                "One or more checks are still pending.",
                "Wait for checks to finish, then re-run readiness.",
            ));
            (
                "waiting_on_required_checks",
                "Wait for pending checks to complete.",
                "No conflict guidance unless GitHub reports DIRTY or BEHIND.",
            )
        }
        _ => (
            "unknown",
            "Homeboy does not recognize this merge/check combination yet.",
            "Inspect GitHub's PR merge box and raw mergeStateStatus.",
        ),
    };

    if ci_state == "stale" {
        blockers.push(readiness_blocker(
            "stale_check_rollup",
            "GitHub check rollup appears stale for a merged or recently changed PR.",
            "Refresh GitHub state and re-run readiness before using this as merge evidence.",
        ));
    }

    let interpreted_state = if interpreted_state == "mergeable_now" && !blockers.is_empty() {
        "failing_required_checks"
    } else {
        interpreted_state
    };
    let mergeable = interpreted_state == "mergeable_now" && blockers.is_empty();
    let check_guidance = format!("{} {}", check_guidance, ci_summary)
        .trim()
        .to_string();

    PrMergeReadiness {
        raw_merge_state: normalized_merge_state,
        interpreted_state: interpreted_state.to_string(),
        mergeable,
        blockers,
        check_guidance,
        conflict_guidance: conflict_guidance.to_string(),
    }
}

fn readiness_blocker(kind: &str, message: &str, guidance: &str) -> PrReadinessBlocker {
    PrReadinessBlocker {
        kind: kind.to_string(),
        message: message.to_string(),
        guidance: guidance.to_string(),
    }
}

fn parse_issue_list_json(raw: &str, options: &IssueFindOptions) -> Result<Vec<GithubFindItem>> {
    #[derive(serde::Deserialize)]
    struct RawIssue {
        number: u64,
        title: String,
        #[serde(default)]
        body: Option<String>,
        url: String,
        state: String,
        #[serde(default, rename = "stateReason")]
        state_reason: Option<String>,
        #[serde(default, rename = "closedAt")]
        closed_at: Option<String>,
        #[serde(default)]
        labels: Vec<RawLabel>,
    }
    #[derive(serde::Deserialize)]
    struct RawLabel {
        name: String,
    }

    let parsed: Vec<RawIssue> = serde_json::from_str(raw.trim())
        .map_err(|e| Error::internal_json(e.to_string(), Some("gh issue list".into())))?;

    let out = parsed
        .into_iter()
        .filter(|i| match &options.title {
            Some(t) => &i.title == t,
            None => true,
        })
        .filter(|i| {
            options
                .labels
                .iter()
                .all(|needle| i.labels.iter().any(|l| &l.name == needle))
        })
        .map(|i| GithubFindItem {
            number: i.number,
            title: i.title,
            body: i.body.unwrap_or_default(),
            url: i.url,
            state: i.state,
            state_reason: i.state_reason.unwrap_or_default(),
            closed_at: i.closed_at.unwrap_or_default(),
            labels: i.labels.into_iter().map(|l| l.name).collect(),
        })
        .collect();
    Ok(out)
}

fn parse_pr_list_json(raw: &str) -> Result<Vec<GithubFindItem>> {
    #[derive(serde::Deserialize)]
    struct RawPr {
        number: u64,
        title: String,
        url: String,
        state: String,
    }

    let parsed: Vec<RawPr> = serde_json::from_str(raw.trim())
        .map_err(|e| Error::internal_json(e.to_string(), Some("gh pr list".into())))?;
    Ok(parsed
        .into_iter()
        .map(|p| GithubFindItem {
            number: p.number,
            title: p.title,
            body: String::new(),
            url: p.url,
            state: p.state,
            state_reason: String::new(),
            closed_at: String::new(),
            labels: Vec::new(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Tests — pure parsing helpers (no gh shelling)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_token_prefers_gh_token_env() {
        let token = select_github_token(
            Some(" env-gh-token \n".to_string()),
            Some("github-token".to_string()),
            || Some("cli-token".to_string()),
        );

        assert_eq!(token.as_deref(), Some("env-gh-token"));
    }

    #[test]
    fn github_token_falls_back_to_github_token_env() {
        let token = select_github_token(
            Some("  ".to_string()),
            Some("github-token".to_string()),
            || Some("cli-token".to_string()),
        );

        assert_eq!(token.as_deref(), Some("github-token"));
    }

    #[test]
    fn github_token_falls_back_to_gh_auth_token() {
        let token = select_github_token(None, None, || Some("cli-token".to_string()));

        assert_eq!(token.as_deref(), Some("cli-token"));
    }

    #[test]
    fn parse_issue_number_from_issue_url() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/42"),
            Some(42)
        );
    }

    #[test]
    fn parse_issue_number_from_pr_url() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/pull/1337"),
            Some(1337)
        );
    }

    #[test]
    fn parse_issue_number_handles_trailing_slash() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/42/"),
            Some(42)
        );
    }

    #[test]
    fn parse_issue_number_none_for_non_numeric() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/not-a-number"),
            None
        );
    }

    #[test]
    fn parse_pr_fleet_ref_accepts_number() {
        assert_eq!(parse_pr_fleet_ref("42", "owner", "repo").unwrap(), 42);
    }

    #[test]
    fn parse_pr_fleet_ref_accepts_matching_url() {
        assert_eq!(
            parse_pr_fleet_ref("https://github.com/owner/repo/pull/42", "owner", "repo").unwrap(),
            42
        );
    }

    #[test]
    fn parse_pr_fleet_ref_rejects_wrong_repo_url() {
        let error = parse_pr_fleet_ref("https://github.com/other/repo/pull/42", "owner", "repo")
            .unwrap_err();

        assert!(error.contains("expected owner/repo"));
    }

    #[test]
    fn summarize_check_rollup_counts_status_groups() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"FAILURE"},
            {"status":"IN_PROGRESS","conclusion":""},
            {"status":"COMPLETED","conclusion":"BOGUS"}
        ]);
        let rollup = summarize_check_rollup(checks.as_array().unwrap());

        assert_eq!(rollup.total, 4);
        assert_eq!(rollup.passed, 1);
        assert_eq!(rollup.failed, 1);
        assert_eq!(rollup.pending, 1);
        assert_eq!(rollup.unknown, 1);
    }

    #[test]
    fn pr_fleet_item_requires_clean_green_before_merge() {
        let view = GithubPrView {
            component_id: "homeboy".into(),
            owner: "Extra-Chill".into(),
            repo: "homeboy".into(),
            number: 42,
            url: "https://github.com/Extra-Chill/homeboy/pull/42".into(),
            title: Some("Ready".into()),
            state: "OPEN".into(),
            draft: false,
            author: None,
            base: "main".into(),
            head: "feature".into(),
            head_repository: None,
            head_sha: Some("abc".into()),
            merged_at: None,
            review_decision: None,
            merge_state: Some("CLEAN".into()),
            ci_state: "terminal_green".into(),
            ci_summary: "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending".into(),
            ci_next_action: "merge_ready".into(),
        };

        let item = pr_fleet_item("42", &view, GithubPrCheckRollup::default());

        assert!(item.mergeable);
        assert_eq!(item.required_action, "merge");
    }

    #[test]
    fn pr_fleet_item_reports_stale_base_action() {
        let view = GithubPrView {
            component_id: "homeboy".into(),
            owner: "Extra-Chill".into(),
            repo: "homeboy".into(),
            number: 42,
            url: "https://github.com/Extra-Chill/homeboy/pull/42".into(),
            title: Some("Stale base".into()),
            state: "OPEN".into(),
            draft: false,
            author: None,
            base: "main".into(),
            head: "feature".into(),
            head_repository: None,
            head_sha: Some("abc".into()),
            merged_at: None,
            review_decision: None,
            merge_state: Some("BEHIND".into()),
            ci_state: "terminal_green".into(),
            ci_summary: "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending".into(),
            ci_next_action: "merge_ready".into(),
        };

        let item = pr_fleet_item("42", &view, GithubPrCheckRollup::default());

        assert!(!item.mergeable);
        assert!(item.stale_base);
        assert_eq!(item.required_action, "update_branch");
    }

    #[test]
    fn classify_pr_ci_distinguishes_terminal_green() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"SKIPPED"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_green");
        assert!(summary.contains("1 passed"));
        assert!(summary.contains("1 skipped"));
        assert_eq!(next_action, "merge_ready");
    }

    #[test]
    fn classify_pr_ci_distinguishes_terminal_failed() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"FAILURE","name":"homeboy / Test","detailsUrl":"https://example.test/logs"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_failed");
        assert!(summary.contains("1 failed/unknown"));
        assert!(summary.contains("homeboy / Test (https://example.test/logs)"));
        assert_eq!(next_action, "inspect_failed_logs");
    }

    #[test]
    fn classify_pr_ci_distinguishes_pending_open_pr() {
        let checks = serde_json::json!([
            {"status":"QUEUED","conclusion":"","name":"homeboy / Build","workflowName":"CI","startedAt":"2026-06-22T01:00:00Z"},
            {"status":"IN_PROGRESS","conclusion":"","name":"homeboy / Test","workflowName":"CI","startedAt":"2026-06-22T01:01:00Z"},
            {"status":"PENDING","conclusion":"","name":"required/context"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "pending");
        assert!(summary.contains("1 queued"));
        assert!(summary.contains("1 running"));
        assert!(summary.contains("1 pending"));
        assert!(summary.contains("oldest pending since 2026-06-22T01:00:00Z"));
        assert!(summary.contains("homeboy / Build (CI, since 2026-06-22T01:00:00Z)"));
        assert_eq!(next_action, "wait");
    }

    #[test]
    fn classify_pr_ci_marks_merged_pending_checks_as_stale() {
        let checks = serde_json::json!([
            {"name":"homeboy / Test","status":"IN_PROGRESS","conclusion":""}
        ]);
        let (state, summary, next_action) = classify_pr_ci(
            "MERGED",
            Some("2026-06-15T12:47:01Z"),
            None,
            checks.as_array().unwrap(),
        );

        assert_eq!(state, "stale");
        assert!(summary.contains("1 running"));
        assert_eq!(next_action, "wait");
    }

    #[test]
    fn classify_pr_ci_recommends_update_branch_when_behind() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, Some("BEHIND"), checks.as_array().unwrap());

        assert_eq!(state, "terminal_green");
        assert!(summary.contains("next action: update branch"));
        assert_eq!(next_action, "update_branch");
    }

    #[test]
    fn classify_pr_ci_recommends_rerun_for_cancelled_checks() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"CANCELLED","name":"homeboy / Lint","detailsUrl":"https://example.test/lint"}
        ]);
        let (state, summary, next_action) =
            classify_pr_ci("OPEN", None, None, checks.as_array().unwrap());

        assert_eq!(state, "terminal_failed");
        assert!(summary.contains("next action: rerun"));
        assert_eq!(next_action, "rerun");
    }

    #[test]
    fn readiness_explains_unknown_without_merge_probe() {
        let readiness = interpret_pr_merge_readiness(
            Some("UNKNOWN"),
            "terminal_green",
            "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.raw_merge_state.as_deref(), Some("UNKNOWN"));
        assert_eq!(readiness.interpreted_state, "unknown");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "mergeability_unknown");
        assert!(readiness.blockers[0]
            .guidance
            .contains("do not attempt a merge"));
    }

    #[test]
    fn readiness_explains_unstable_as_optional_checks() {
        let readiness = interpret_pr_merge_readiness(
            Some("UNSTABLE"),
            "terminal_failed",
            "2 check(s): 1 terminal-green, 1 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "failing_optional_checks");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "optional_checks_unstable");
        assert!(readiness
            .conflict_guidance
            .contains("not a conflict signal"));
    }

    #[test]
    fn readiness_treats_clean_without_checks_as_required_wait() {
        let readiness = interpret_pr_merge_readiness(
            Some("CLEAN"),
            "no_checks",
            "GitHub reported no status checks for this PR head.",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "waiting_on_required_checks");
        assert!(!readiness.mergeable);
        assert_eq!(readiness.blockers[0].kind, "checks_not_reported");
    }

    #[test]
    fn readiness_allows_clean_green_non_draft_pr() {
        let readiness = interpret_pr_merge_readiness(
            Some("CLEAN"),
            "terminal_green",
            "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending",
            Some("APPROVED"),
            false,
        );

        assert_eq!(readiness.interpreted_state, "mergeable_now");
        assert!(readiness.mergeable);
        assert!(readiness.blockers.is_empty());
    }

    #[test]
    fn parse_issue_list_filters_by_title() {
        let raw = r#"[
            {"number":1,"title":"bug: one","url":"u1","state":"open","labels":[]},
            {"number":2,"title":"bug: two","url":"u2","state":"open","labels":[]}
        ]"#;
        let opts = IssueFindOptions {
            title: Some("bug: two".into()),
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 2);
    }

    #[test]
    fn parse_issue_list_requires_all_labels() {
        let raw = r#"[
            {"number":1,"title":"a","url":"u1","state":"open","labels":[{"name":"ci-failure"}]},
            {"number":2,"title":"b","url":"u2","state":"open","labels":[{"name":"ci-failure"},{"name":"autofix"}]}
        ]"#;
        let opts = IssueFindOptions {
            labels: vec!["ci-failure".into(), "autofix".into()],
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 2);
    }

    #[test]
    fn parse_pr_list_extracts_all_entries() {
        let raw = r#"[
            {"number":10,"title":"feat: x","url":"u10","state":"OPEN"},
            {"number":11,"title":"chore: y","url":"u11","state":"OPEN"}
        ]"#;
        let items = parse_pr_list_json(raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 10);
        assert_eq!(items[1].state, "OPEN");
    }

    #[test]
    fn test_pr_files() {
        let owner = "Extra-Chill";
        let repo = "homeboy";
        let number = 42_u64;
        assert_eq!(
            format!("repos/{}/{}/pulls/{}/files", owner, repo, number),
            "repos/Extra-Chill/homeboy/pulls/42/files"
        );
    }

    #[test]
    fn test_pr_view() {
        let raw = r#"{
            "author":{"login":"homeboy-ci[bot]"},
            "baseRefName":"main",
            "headRefName":"ci/autofix/homeboy/main",
            "headRepository":{"nameWithOwner":"Extra-Chill/homeboy"}
        }"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.pointer("/author/login").and_then(|v| v.as_str()),
            Some("homeboy-ci[bot]")
        );
        assert_eq!(
            parsed.get("baseRefName").and_then(|v| v.as_str()),
            Some("main")
        );
        assert_eq!(
            parsed
                .pointer("/headRepository/nameWithOwner")
                .and_then(|v| v.as_str()),
            Some("Extra-Chill/homeboy")
        );
    }

    #[test]
    fn test_pr_merge() {
        let result = pr_merge(
            Some("missing-component"),
            PrMergeOptions {
                method: "explode".into(),
                number: 1,
                ..Default::default()
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn issue_state_gh_flag() {
        assert_eq!(IssueState::Open.as_gh_flag(), "open");
        assert_eq!(IssueState::Closed.as_gh_flag(), "closed");
        assert_eq!(IssueState::All.as_gh_flag(), "all");
    }

    #[test]
    fn pr_state_gh_flag() {
        assert_eq!(PrState::Open.as_gh_flag(), "open");
        assert_eq!(PrState::Merged.as_gh_flag(), "merged");
    }

    #[test]
    fn issue_close_reason_gh_flag() {
        assert_eq!(IssueCloseReason::Completed.as_gh_flag(), "completed");
        assert_eq!(IssueCloseReason::NotPlanned.as_gh_flag(), "not planned");
    }

    #[test]
    fn parse_issue_list_extracts_state_reason_and_closed_at() {
        // gh issue list --json includes stateReason + closedAt fields when
        // requested. Closed-completed, closed-not_planned, and open issues
        // are represented in this fixture.
        let raw = r#"[
            {
                "number": 100,
                "title": "audit: thing in repo (3)",
                "url": "https://github.com/o/r/issues/100",
                "state": "OPEN",
                "stateReason": null,
                "closedAt": null,
                "labels": [{"name":"audit"}]
            },
            {
                "number": 101,
                "title": "audit: other in repo (5)",
                "url": "https://github.com/o/r/issues/101",
                "state": "CLOSED",
                "stateReason": "completed",
                "closedAt": "2026-04-25T12:00:00Z",
                "labels": [{"name":"audit"}]
            },
            {
                "number": 102,
                "title": "audit: muted in repo (12)",
                "url": "https://github.com/o/r/issues/102",
                "state": "CLOSED",
                "stateReason": "not_planned",
                "closedAt": "2026-04-26T03:00:00Z",
                "labels": [{"name":"audit"},{"name":"wontfix"}]
            }
        ]"#;
        let opts = IssueFindOptions {
            state: IssueState::All,
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 3);

        // Open issue: empty state_reason and closed_at, single label.
        assert_eq!(items[0].number, 100);
        assert_eq!(items[0].state, "OPEN");
        assert_eq!(items[0].state_reason, "");
        assert_eq!(items[0].closed_at, "");
        assert_eq!(items[0].labels, vec!["audit".to_string()]);

        // Closed completed: state_reason populated, closed_at populated.
        assert_eq!(items[1].number, 101);
        assert_eq!(items[1].state, "CLOSED");
        assert_eq!(items[1].state_reason, "completed");
        assert_eq!(items[1].closed_at, "2026-04-25T12:00:00Z");

        // Closed not_planned with suppression label.
        assert_eq!(items[2].number, 102);
        assert_eq!(items[2].state_reason, "not_planned");
        assert_eq!(
            items[2].labels,
            vec!["audit".to_string(), "wontfix".to_string()]
        );
    }

    #[test]
    fn parse_issue_list_handles_missing_optional_fields() {
        // Older gh versions or projects without state-reason support emit
        // payloads without those fields. Default-deserialize to empty.
        let raw = r#"[
            {"number":1,"title":"x","url":"u","state":"open","labels":[]}
        ]"#;
        let opts = IssueFindOptions::default();
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state_reason, "");
        assert_eq!(items[0].closed_at, "");
        assert!(items[0].labels.is_empty());
    }
}
