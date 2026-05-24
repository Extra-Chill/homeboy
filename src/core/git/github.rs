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

use serde::Serialize;
use std::path::Path;
use std::process::Command;

use crate::core::component;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};

use super::github_comment_sections::{
    comment_matches_key, extract_footer, extract_header, merge_section, parse_comment_sections,
    render_comment,
};
use super::resolve_target;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Result of a GitHub issue operation (create, comment, find-one).
#[derive(Debug, Clone, Serialize)]
pub struct GithubIssueOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Result of a GitHub PR operation (create, edit, find-one, comment).
#[derive(Debug, Clone, Default, Serialize)]
pub struct GithubPrOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Canonical comment id (sectioned flow). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<u64>,
    /// Non-fatal warnings. Currently used for duplicate-comment deletes that
    /// failed during race consolidation — the canonical comment was still
    /// updated successfully, so we report the stuck ids and exit 0.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubPrView {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub author: Option<String>,
    pub base: String,
    pub head: String,
    pub head_repository: Option<String>,
}

/// Result of a find-many operation (list of matches).
#[derive(Debug, Clone, Serialize)]
pub struct GithubFindOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    pub items: Vec<GithubFindItem>,
}

/// Minimal identifier for a found issue or PR.
#[derive(Debug, Clone, Serialize)]
pub struct GithubFindItem {
    pub number: u64,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub url: String,
    pub state: String,
    /// GitHub `stateReason` (issues only). One of `completed`, `not_planned`,
    /// `reopened`, or `null`. Empty string when absent or for PRs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state_reason: String,
    /// GitHub `closedAt` ISO-8601 timestamp (issues only). Empty when absent
    /// (open issues) or for PRs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub closed_at: String,
    /// Labels attached to the issue/PR. Used for label-based suppression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Parameters for creating a new issue.
#[derive(Debug, Clone, Default)]
pub struct IssueCreateOptions {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    /// Optional workspace path. When set, the component is discovered from
    /// `<path>/homeboy.json` instead of the global registry — required for
    /// CI runners and other unregistered-checkout contexts.
    pub path: Option<String>,
}

/// Parameters for filtering issues.
#[derive(Debug, Clone, Default)]
pub struct IssueFindOptions {
    /// Exact title match (case-sensitive).
    pub title: Option<String>,
    /// All labels must be present.
    pub labels: Vec<String>,
    /// `open` (default), `closed`, or `all`.
    pub state: IssueState,
    /// Cap the number of returned items. Defaults to 30.
    pub limit: usize,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueState {
    #[default]
    Open,
    Closed,
    All,
}

impl IssueState {
    fn as_gh_flag(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
            IssueState::All => "all",
        }
    }
}

/// Parameters for creating a new PR.
#[derive(Debug, Clone, Default)]
pub struct PrCreateOptions {
    pub base: String,
    pub head: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for editing an existing PR.
#[derive(Debug, Clone, Default)]
pub struct PrEditOptions {
    pub number: u64,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for filtering PRs.
#[derive(Debug, Clone, Default)]
pub struct PrFindOptions {
    pub base: Option<String>,
    pub head: Option<String>,
    pub state: PrState,
    pub limit: usize,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PrMergeOptions {
    pub number: u64,
    pub method: String,
    pub delete_branch: bool,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl PrState {
    fn as_gh_flag(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
            PrState::All => "all",
        }
    }
}

/// Parameters for posting a (potentially sticky) PR comment.
///
/// Three shapes are supported, selected by `mode`:
/// - [`PrCommentMode::Fresh`] — plain append, no marker, no find-or-update.
/// - [`PrCommentMode::StickyWholeBody`] — single-section sticky (PR #1334
///   semantics): prepend `<!-- homeboy:key=<key> -->` marker and update the
///   one matching comment in place.
/// - [`PrCommentMode::Sectioned`] — multi-section aggregation: a single shared
///   comment carries `<!-- homeboy:comment-key=<outer> -->` and N section
///   blocks delimited by `<!-- homeboy:section-key=<inner>:start|end -->`.
///   Each invocation replaces its own inner section and leaves the others
///   untouched. Handles race consolidation when parallel jobs raced to create
///   the shared comment.
#[derive(Debug, Clone)]
pub struct PrCommentOptions {
    pub number: u64,
    pub body: String,
    pub mode: PrCommentMode,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

impl Default for PrCommentOptions {
    fn default() -> Self {
        Self {
            number: 0,
            body: String::new(),
            mode: PrCommentMode::Fresh,
            path: None,
        }
    }
}

/// Which comment-posting flow to run. Mutually exclusive shapes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PrCommentMode {
    /// Plain append. No marker. No find-or-update.
    #[default]
    Fresh,
    /// Single-section sticky comment (PR #1334). The `body` is treated as the
    /// whole comment body; the marker `<!-- homeboy:key=<key> -->` is prepended.
    StickyWholeBody { key: String },
    /// Multi-section aggregated sticky comment. `body` is ONE section's body;
    /// it is merged under `section_key` into the comment carrying `comment_key`.
    Sectioned {
        /// Outer marker (one shared comment per PR per outer key).
        comment_key: String,
        /// Inner marker (one section per inner key within the shared comment).
        section_key: String,
        /// Optional header line written just after the outer marker on fresh
        /// comments (e.g. `## Homeboy Results — \`<component>\``). Preserved
        /// from existing comments on merge.
        header: Option<String>,
        /// Optional footer block written after the last section on fresh
        /// comments (e.g. a `<details><summary>Tooling versions</summary>`
        /// block). Preserved from existing comments on merge when the caller
        /// does not pass one explicitly; overwritten when the caller does.
        footer: Option<String>,
        /// Optional explicit section ordering. Sections listed here come first
        /// in the given order; any other sections are appended alphabetically.
        /// `None` = pure alphabetical.
        section_order: Option<Vec<String>>,
    },
}

/// Parameters for closing an existing issue with a reason.
///
/// `reason` defaults to `Completed` (the GitHub-native signal for "the
/// underlying problem was resolved"). `NotPlanned` is preserved across CI
/// runs by `homeboy issues reconcile` as the "do not re-file" signal.
#[derive(Debug, Clone, Default)]
pub struct IssueCloseOptions {
    pub number: u64,
    /// Close-reason. `Completed` (default) is the GitHub-native signal for
    /// "the underlying problem was resolved." `NotPlanned` is the GitHub-native
    /// signal for "we've decided not to fix this" — used by `homeboy issues
    /// reconcile` to suppress re-filing on subsequent runs.
    pub reason: IssueCloseReason,
    /// Optional closing comment posted before the state transition. Useful
    /// for explaining why the issue is being closed (e.g. "All findings have
    /// been resolved" / "Closed as duplicate of #N" / "Closed as upstream bug").
    pub comment: Option<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueCloseReason {
    #[default]
    Completed,
    NotPlanned,
}

impl IssueCloseReason {
    /// Render for the `gh issue close --reason` flag. GitHub's CLI uses the
    /// space-form (`"not planned"`) but the underlying GraphQL `state_reason`
    /// uses the underscore form (`not_planned`). `IssueState::Reason` parsing
    /// reads the underscore form from `gh ... --json stateReason`.
    fn as_gh_flag(self) -> &'static str {
        match self {
            IssueCloseReason::Completed => "completed",
            IssueCloseReason::NotPlanned => "not planned",
        }
    }
}

/// Parameters for editing an existing issue.
#[derive(Debug, Clone, Default)]
pub struct IssueEditOptions {
    pub number: u64,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Labels to add. Mirrors `gh issue edit --add-label` (repeatable).
    pub add_labels: Vec<String>,
    /// Labels to remove. Mirrors `gh issue edit --remove-label`.
    pub remove_labels: Vec<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for posting a comment on an existing issue.
#[derive(Debug, Clone, Default)]
pub struct IssueCommentOptions {
    pub number: u64,
    pub body: String,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

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
        "author,baseRefName,headRefName,headRepository".into(),
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

    Ok(GithubPrView {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        number,
        author,
        base,
        head,
        head_repository,
    })
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

/// Post a comment on a PR.
///
/// Dispatches on [`PrCommentOptions::mode`]:
/// - [`PrCommentMode::Fresh`] — plain append, no marker.
/// - [`PrCommentMode::StickyWholeBody`] — find-or-update the one comment
///   tagged `<!-- homeboy:key=<key> -->` (single-section sticky, PR #1334).
/// - [`PrCommentMode::Sectioned`] — multi-section aggregation: merge this
///   invocation's section under `section_key` into the shared comment tagged
///   `<!-- homeboy:comment-key=<comment_key> -->`.
pub fn pr_comment(component_id: Option<&str>, options: PrCommentOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    match options.mode.clone() {
        PrCommentMode::Fresh => pr_comment_fresh(id, repo, options),
        PrCommentMode::StickyWholeBody { key } => {
            pr_comment_sticky_whole(id, repo, options.number, options.body, key)
        }
        PrCommentMode::Sectioned {
            comment_key,
            section_key,
            header,
            footer,
            section_order,
        } => pr_comment_sectioned(
            id,
            repo,
            options.number,
            options.body,
            comment_key,
            section_key,
            header,
            footer,
            section_order,
        ),
    }
}

/// Plain append flow. Shared by `Fresh` mode and the "no existing comment"
/// branch of the sticky flow.
fn pr_comment_fresh(
    id: String,
    repo: GitHubRepo,
    options: PrCommentOptions,
) -> Result<GithubPrOutput> {
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "pr".into(),
        "comment".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        "--body".into(),
        options.body,
    ];
    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        ..Default::default()
    })
}

/// Sticky single-section flow (PR #1334 semantics).
fn pr_comment_sticky_whole(
    id: String,
    repo: GitHubRepo,
    pr_number: u64,
    body: String,
    key: String,
) -> Result<GithubPrOutput> {
    let full_body = format!("{}\n{}", marker_for_key(&key), body);

    if let Some(existing_id) = find_sticky_comment_id(&repo, pr_number, &key)? {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, existing_id
            ),
            "--method".into(),
            "PATCH".into(),
            "-f".into(),
            format!("body={}", full_body),
        ];
        run_gh(&args)?;
        return Ok(GithubPrOutput {
            component_id: id,
            owner: repo.owner,
            repo: repo.repo,
            action: "pr.comment.update".to_string(),
            success: true,
            number: Some(pr_number),
            comment_id: Some(existing_id),
            ..Default::default()
        });
    }

    // Fall through to fresh-comment.
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "pr".into(),
        "comment".into(),
        pr_number.to_string(),
        "-R".into(),
        repo_flag,
        "--body".into(),
        full_body,
    ];
    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(pr_number),
        url: Some(output.trim().to_string()),
        ..Default::default()
    })
}

/// Sectioned-comment flow.
///
/// Flow:
/// 1. List the PR's issue-comments, filter to those carrying the comment-key
///    marker.
/// 2. If none: create one with a single section block.
/// 3. If one: parse existing sections, merge this invocation's section, render.
///    Byte-compare to the existing body — if equal, emit `pr.comment.section.noop`
///    and skip the PATCH. Otherwise PATCH.
/// 4. If many (race): pick lowest id as canonical, merge sections from ALL
///    matching comments (current invocation wins last for duplicate keys),
///    PATCH canonical, DELETE the rest. Failed DELETEs become warnings, not
///    hard errors — the next invocation will consolidate.
#[allow(clippy::too_many_arguments)]
fn pr_comment_sectioned(
    id: String,
    repo: GitHubRepo,
    pr_number: u64,
    section_body: String,
    comment_key: String,
    section_key: String,
    header: Option<String>,
    footer: Option<String>,
    section_order: Option<Vec<String>>,
) -> Result<GithubPrOutput> {
    // 1. Fetch every matching comment (with bodies) — we need the bodies to
    //    merge the race case. `gh api --paginate` returns a stream of JSON
    //    arrays concatenated with no separator; we parse each array
    //    independently and flatten the results.
    let matches = list_matching_comments(&repo, pr_number, &comment_key)?;

    if matches.is_empty() {
        // 2. No existing comment — create fresh with a single section.
        let mut sections: Vec<(String, String)> = Vec::new();
        sections.push((section_key.clone(), section_body.clone()));
        let rendered = render_comment(
            &comment_key,
            header.as_deref(),
            &sections,
            section_order.as_deref(),
            footer.as_deref(),
        );
        let repo_flag = format!("{}/{}", repo.owner, repo.repo);
        let args: Vec<String> = vec![
            "pr".into(),
            "comment".into(),
            pr_number.to_string(),
            "-R".into(),
            repo_flag,
            "--body".into(),
            rendered,
        ];
        let output = run_gh(&args)?;
        return Ok(GithubPrOutput {
            component_id: id,
            owner: repo.owner,
            repo: repo.repo,
            action: "pr.comment.section.create".to_string(),
            success: true,
            number: Some(pr_number),
            url: Some(output.trim().to_string()),
            ..Default::default()
        });
    }

    // 3/4. Canonical = lowest id. Merge sections from every matching comment
    //      (ascending id order so later comments override earlier ones), then
    //      overwrite this invocation's section last.
    let mut matches = matches;
    matches.sort_by_key(|m| m.id);
    let canonical_id = matches[0].id;
    let canonical_body = matches[0].body.clone();

    let mut merged: Vec<(String, String)> = Vec::new();
    let mut discovered_header: Option<String> = header.clone();
    let mut discovered_footer: Option<String> = footer.clone();
    for comment in &matches {
        let parsed = parse_comment_sections(&comment.body);
        for (k, v) in parsed {
            merged = merge_section(merged, &k, v);
        }
        // First comment wins the header / footer (in ascending id order =
        // lowest id), but only if caller didn't pass one explicitly.
        if discovered_header.is_none() {
            discovered_header = extract_header(&comment.body);
        }
        if discovered_footer.is_none() {
            discovered_footer = extract_footer(&comment.body);
        }
    }
    // Current invocation wins last.
    merged = merge_section(merged, &section_key, section_body);

    let rendered = render_comment(
        &comment_key,
        discovered_header.as_deref(),
        &merged,
        section_order.as_deref(),
        discovered_footer.as_deref(),
    );

    // Idempotency: byte-compare rendered to canonical's existing body.
    let patch_needed = rendered.trim_end() != canonical_body.trim_end();
    let mut warnings: Vec<String> = Vec::new();

    if patch_needed {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, canonical_id
            ),
            "--method".into(),
            "PATCH".into(),
            "-f".into(),
            format!("body={}", rendered),
        ];
        run_gh(&args)?;
    }

    // Delete duplicates (best-effort — warn instead of failing).
    for comment in matches.iter().skip(1) {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, comment.id
            ),
            "--method".into(),
            "DELETE".into(),
        ];
        if run_gh(&args).is_err() {
            warnings.push(format!(
                "failed to delete duplicate comment id={} — next invocation will retry",
                comment.id
            ));
        }
    }

    let action = if patch_needed {
        "pr.comment.section.update"
    } else {
        "pr.comment.section.noop"
    };

    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: action.to_string(),
        success: true,
        number: Some(pr_number),
        comment_id: Some(canonical_id),
        warnings,
        ..Default::default()
    })
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
fn resolve_component_github(
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
                    "Set it: homeboy component set <id> -- --remote_url https://github.com/<owner>/<repo>".to_string(),
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

/// Error out if `gh` is missing or unauthenticated. Unlike `run_github_release`
/// (which soft-fails because the tag is already pushed), primitive operations
/// have no already-committed side effect to preserve — fail loudly.
fn ensure_gh_ready() -> Result<()> {
    if !gh_probe_succeeds(&["--version"]) {
        return Err(Error::internal_io(
            "`gh` CLI not found on PATH".to_string(),
            Some("gh".to_string()),
        )
        .with_hint("Install the GitHub CLI: https://cli.github.com"));
    }

    if !gh_probe_succeeds(&["auth", "status", "--hostname", "github.com"]) {
        return Err(Error::internal_io(
            "`gh` is not authenticated for github.com".to_string(),
            Some("gh auth status".to_string()),
        )
        .with_hint("Authenticate with: gh auth login"));
    }

    Ok(())
}

/// Run `gh <args>` and return stdout on success, or a structured error on
/// failure (with stderr captured in the error message).
fn run_gh(args: &[String]) -> Result<String> {
    let output = Command::new("gh")
        .args(args.iter().map(|s| s.as_str()))
        .output()
        .map_err(|e| {
            Error::internal_io(format!("Failed to invoke gh: {}", e), Some("gh".into()))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let combined = if stderr.is_empty() { stdout } else { stderr };
        return Err(Error::git_command_failed(format!(
            "gh {} failed: {}",
            args.first().map(|s| s.as_str()).unwrap_or(""),
            combined
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_issue_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

fn marker_for_key(key: &str) -> String {
    format!("<!-- homeboy:key={} -->", key)
}

/// Search a PR's issue-comments for one carrying our sticky marker.
fn find_sticky_comment_id(repo: &GitHubRepo, pr_number: u64, key: &str) -> Result<Option<u64>> {
    let marker = marker_for_key(key);
    let args: Vec<String> = vec![
        "api".into(),
        format!(
            "repos/{}/{}/issues/{}/comments?per_page=100",
            repo.owner, repo.repo, pr_number
        ),
        "--paginate".into(),
        "--jq".into(),
        format!(".[] | select(.body | contains(\"{}\")) | .id", marker),
    ];
    let raw = run_gh(&args)?;
    Ok(raw.lines().next().and_then(|l| l.trim().parse().ok()))
}

/// Minimal shape for a fetched PR comment (id + body).
struct FetchedComment {
    id: u64,
    body: String,
}

/// List all PR issue-comments that carry the given comment-key marker. Returns
/// `(id, body)` pairs so the merge step can parse each body.
fn list_matching_comments(
    repo: &GitHubRepo,
    pr_number: u64,
    comment_key: &str,
) -> Result<Vec<FetchedComment>> {
    let args: Vec<String> = vec![
        "api".into(),
        format!(
            "repos/{}/{}/issues/{}/comments?per_page=100",
            repo.owner, repo.repo, pr_number
        ),
        "--paginate".into(),
    ];
    let raw = run_gh(&args)?;
    parse_comments_list_json(&raw, comment_key)
}

/// Parse `gh api --paginate issues/:n/comments` output and filter to those
/// carrying the outer marker. With `--paginate`, `gh` concatenates JSON
/// arrays (no separator between pages) — we re-parse as a stream.
fn parse_comments_list_json(raw: &str, comment_key: &str) -> Result<Vec<FetchedComment>> {
    #[derive(serde::Deserialize)]
    struct RawComment {
        id: u64,
        body: Option<String>,
    }

    let mut out: Vec<FetchedComment> = Vec::new();

    // `--paginate` output is one or more JSON arrays concatenated. Use a
    // streaming deserializer to eat them in order.
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(out);
    }
    let de = serde_json::Deserializer::from_str(trimmed);
    for value in de.into_iter::<Vec<RawComment>>() {
        let page = value
            .map_err(|e| Error::internal_json(e.to_string(), Some("gh api comments".into())))?;
        for c in page {
            let body = c.body.unwrap_or_default();
            if comment_matches_key(&body, comment_key) {
                out.push(FetchedComment { id: c.id, body });
            }
        }
    }
    Ok(out)
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
    fn marker_format_is_stable() {
        assert_eq!(
            marker_for_key("ci-status"),
            "<!-- homeboy:key=ci-status -->"
        );
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

    #[test]
    fn parse_comments_list_filters_by_key_and_handles_pagination() {
        // Two pages: gh --paginate concatenates JSON arrays with no separator.
        let raw = r#"[
            {"id": 1, "body": "<!-- homeboy:comment-key=ci:x -->\nsection"},
            {"id": 2, "body": "unrelated"}
        ][
            {"id": 3, "body": "<!-- homeboy:comment-key=ci:y -->\nother key"},
            {"id": 4, "body": "<!-- homeboy:comment-key=other -->\nother"}
        ]"#;
        let got = parse_comments_list_json(raw, "ci:x").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, 1);
    }

    #[test]
    fn parse_comments_list_empty_input_is_ok() {
        let got = parse_comments_list_json("", "ci:x").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn pr_comment_mode_default_is_fresh() {
        let opts = PrCommentOptions::default();
        assert_eq!(opts.mode, PrCommentMode::Fresh);
    }
}
