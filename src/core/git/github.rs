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

use crate::component;
use crate::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::error::{Error, Result};

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
    pub url: String,
    pub state: String,
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
}

/// Parameters for editing an existing PR.
#[derive(Debug, Clone, Default)]
pub struct PrEditOptions {
    pub number: u64,
    pub title: Option<String>,
    pub body: Option<String>,
}

/// Parameters for filtering PRs.
#[derive(Debug, Clone, Default)]
pub struct PrFindOptions {
    pub base: Option<String>,
    pub head: Option<String>,
    pub state: PrState,
    pub limit: usize,
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
}

impl Default for PrCommentOptions {
    fn default() -> Self {
        Self {
            number: 0,
            body: String::new(),
            mode: PrCommentMode::Fresh,
        }
    }
}

/// Which comment-posting flow to run. Mutually exclusive shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrCommentMode {
    /// Plain append. No marker. No find-or-update.
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
        /// Optional explicit section ordering. Sections listed here come first
        /// in the given order; any other sections are appended alphabetically.
        /// `None` = pure alphabetical.
        section_order: Option<Vec<String>>,
    },
}

impl Default for PrCommentMode {
    fn default() -> Self {
        PrCommentMode::Fresh
    }
}

// ---------------------------------------------------------------------------
// Public API — issue
// ---------------------------------------------------------------------------

/// Create a new issue on the component's GitHub repository.
pub fn issue_create(
    component_id: Option<&str>,
    options: IssueCreateOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id)?;
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
    number: u64,
    body: &str,
) -> Result<GithubIssueOutput> {
    let (id, repo) = resolve_component_github(component_id)?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "issue".into(),
        "comment".into(),
        number.to_string(),
        "-R".into(),
        repo_flag,
        "--body".into(),
        body.to_string(),
    ];

    let output = run_gh(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.comment".to_string(),
        success: true,
        number: Some(number),
        url: Some(output.trim().to_string()),
        title: None,
        state: None,
    })
}

/// Find issues matching the given filter. Useful for dedup before creating.
///
/// Uses `gh issue list --json number,title,url,state,labels` and filters
/// locally (title and label conjunctions are simpler to enforce client-side
/// than via the gh search syntax).
pub fn issue_find(
    component_id: Option<&str>,
    options: IssueFindOptions,
) -> Result<GithubFindOutput> {
    let (id, repo) = resolve_component_github(component_id)?;
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
        "number,title,url,state,labels".into(),
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
    let (id, repo) = resolve_component_github(component_id)?;
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
    let (id, repo) = resolve_component_github(component_id)?;
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
    let (id, repo) = resolve_component_github(component_id)?;
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
    let (id, repo) = resolve_component_github(component_id)?;
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
            section_order,
        } => pr_comment_sectioned(
            id,
            repo,
            options.number,
            options.body,
            comment_key,
            section_key,
            header,
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
///    marker (new OR legacy format — see [`parse_comment_sections`] contract).
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
    for comment in &matches {
        let parsed = parse_comment_sections(&comment.body);
        for (k, v) in parsed {
            merged = merge_section(merged, &k, v);
        }
        // First comment wins the header (in ascending id order = lowest id),
        // but only if caller didn't pass one explicitly.
        if discovered_header.is_none() {
            discovered_header = extract_header(&comment.body);
        }
    }
    // Current invocation wins last.
    merged = merge_section(merged, &section_key, section_body);

    let rendered = render_comment(
        &comment_key,
        discovered_header.as_deref(),
        &merged,
        section_order.as_deref(),
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
fn resolve_component_github(component_id: Option<&str>) -> Result<(String, GitHubRepo)> {
    let (id, path) = resolve_target(component_id, None)?;
    let comp = component::resolve_effective(Some(&id), None, None)?;

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

/// Error out if `gh` is missing or unauthenticated. Unlike `run_github_release`
/// (which soft-fails because the tag is already pushed), primitive operations
/// have no already-committed side effect to preserve — fail loudly.
fn ensure_gh_ready() -> Result<()> {
    let available = Command::new("gh")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !available {
        return Err(Error::internal_io(
            "`gh` CLI not found on PATH".to_string(),
            Some("gh".to_string()),
        )
        .with_hint("Install the GitHub CLI: https://cli.github.com"));
    }

    let authed = Command::new("gh")
        .args(["auth", "status", "--hostname", "github.com"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !authed {
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

// ---------------------------------------------------------------------------
// Sectioned-comment primitive: pure parser / renderer / merger
// ---------------------------------------------------------------------------
//
// Marker contract recognized by the parser (both formats, for rollout compat):
//
//   NEW (written on render):
//     <!-- homeboy:comment-key=<outer> -->
//     <!-- homeboy:section-key=<inner>:start -->
//     ...
//     <!-- homeboy:section-key=<inner>:end -->
//
//   LEGACY (homeboy-action scripts wrote these before the primitive shipped):
//     <!-- homeboy-action-results:key=<outer> -->
//     <!-- homeboy-action-section:key=<inner>:start -->
//     ...
//     <!-- homeboy-action-section:key=<inner>:end -->
//
// The parser accepts both so the primitive can adopt in-flight comments that
// the action wrote under legacy markers. The renderer writes only new markers;
// re-parsing the output migrates legacy → new on the next invocation.

/// Minimal shape for a fetched PR comment (id + body).
struct FetchedComment {
    id: u64,
    body: String,
}

/// Outer-key marker formats (start-of-body anchor for the shared comment).
fn comment_key_markers(comment_key: &str) -> [String; 2] {
    [
        format!("<!-- homeboy:comment-key={} -->", comment_key),
        format!("<!-- homeboy-action-results:key={} -->", comment_key),
    ]
}

/// Does `body` carry the comment-key marker under either format?
fn comment_matches_key(body: &str, comment_key: &str) -> bool {
    let markers = comment_key_markers(comment_key);
    markers.iter().any(|m| body.contains(m.as_str()))
}

/// Parse section blocks out of a comment body. Honors BOTH new and legacy
/// marker formats.
///
/// Returns an ordered `Vec<(key, body)>` in the order encountered. Keys are
/// trimmed; bodies have leading/trailing newlines stripped. Unpaired or
/// malformed markers are skipped silently (no panic, no error) so the merge
/// loop can always make forward progress even if a comment body was hand-
/// edited.
pub fn parse_comment_sections(body: &str) -> Vec<(String, String)> {
    // Single regex that matches either marker format. The `regex` crate does
    // not support backreferences, so we capture both start-key and end-key
    // and verify equality post-match. Both formats:
    //   <!-- homeboy:section-key=KEY:start -->       (new)
    //   <!-- homeboy-action-section:key=KEY:start --> (legacy)
    let re = regex::Regex::new(
        r"(?s)<!-- homeboy(?:-action-section:key|:section-key)=([^:]*?):start -->\n?(.*?)\n?<!-- homeboy(?:-action-section:key|:section-key)=([^:]*?):end -->",
    )
    .expect("section-marker regex is valid");

    let mut out: Vec<(String, String)> = Vec::new();
    for caps in re.captures_iter(body) {
        let start_key = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let end_key = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");
        if start_key.is_empty() || start_key != end_key {
            // Unmatched or unnamed — skip.
            continue;
        }
        let inner = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let inner = inner.trim_matches('\n').to_string();
        out.push((start_key.to_string(), inner));
    }
    out
}

/// Extract the header line(s) of a comment — everything between the outer
/// marker and the first section marker, minus the outer marker itself.
///
/// Used to preserve an existing header when merging (so we don't clobber
/// `## Homeboy Results — <component>` that an earlier invocation wrote).
fn extract_header(body: &str) -> Option<String> {
    // Find the end of the outer marker line. Either format.
    let outer_end = body.find("-->\n")?;
    let after_outer = &body[outer_end + 4..];
    // First section marker (either format).
    let first_section_idx = after_outer
        .find("<!-- homeboy:section-key=")
        .or_else(|| after_outer.find("<!-- homeboy-action-section:key="))?;
    let header = after_outer[..first_section_idx].trim_matches('\n').trim();
    if header.is_empty() {
        None
    } else {
        Some(header.to_string())
    }
}

/// Merge `(section_key, body)` into `sections`. Replaces any existing entry
/// for `section_key`, preserving the original position; otherwise appends.
pub fn merge_section(
    mut sections: Vec<(String, String)>,
    section_key: &str,
    body: String,
) -> Vec<(String, String)> {
    for entry in sections.iter_mut() {
        if entry.0 == section_key {
            entry.1 = body;
            return sections;
        }
    }
    sections.push((section_key.to_string(), body));
    sections
}

/// Render a comment body from a set of sections.
///
/// - `comment_key` → outer marker (new format).
/// - `header` → optional line(s) written after the outer marker.
/// - `sections` → section map. Insertion order is preserved only when
///   `explicit_order` is `None`; otherwise explicit-ordered keys come first
///   in the given order, and any remaining keys follow alphabetically.
/// - Output is always newline-normalized with a trailing newline and uses the
///   **new** marker format. Re-rendering a legacy-parsed body produces
///   new-format output (migration path).
pub fn render_comment(
    comment_key: &str,
    header: Option<&str>,
    sections: &[(String, String)],
    explicit_order: Option<&[String]>,
) -> String {
    let ordered = order_sections(sections, explicit_order);

    let mut out = String::new();
    out.push_str(&format!("<!-- homeboy:comment-key={} -->\n", comment_key));
    if let Some(h) = header {
        let h = h.trim_matches('\n');
        if !h.is_empty() {
            out.push_str(h);
            out.push('\n');
            out.push('\n');
        }
    }

    for (idx, (key, body)) in ordered.iter().enumerate() {
        out.push_str(&format!("<!-- homeboy:section-key={}:start -->\n", key));
        let body_trimmed = body.trim_matches('\n');
        if !body_trimmed.is_empty() {
            out.push_str(body_trimmed);
            out.push('\n');
        }
        out.push_str(&format!("<!-- homeboy:section-key={}:end -->", key));
        if idx + 1 < ordered.len() {
            out.push_str("\n\n");
        } else {
            out.push('\n');
        }
    }

    out
}

/// Apply the ordering rule: explicit-ordered keys first (in given order),
/// remaining keys alphabetically. Unknown keys in `explicit_order` are
/// silently dropped (nothing to render).
fn order_sections<'a>(
    sections: &'a [(String, String)],
    explicit_order: Option<&[String]>,
) -> Vec<&'a (String, String)> {
    match explicit_order {
        Some(order) => {
            let mut out: Vec<&(String, String)> = Vec::new();
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

            // 1. Keys in explicit order (only if present in sections).
            for key in order {
                if let Some(entry) = sections.iter().find(|(k, _)| k == key) {
                    out.push(entry);
                    seen.insert(entry.0.as_str());
                }
            }

            // 2. Remaining keys, alphabetical.
            let mut leftovers: Vec<&(String, String)> = sections
                .iter()
                .filter(|(k, _)| !seen.contains(k.as_str()))
                .collect();
            leftovers.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(leftovers);

            out
        }
        None => {
            // Pure alphabetical.
            let mut out: Vec<&(String, String)> = sections.iter().collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }
    }
}

/// List all PR issue-comments that carry the given comment-key marker (either
/// format). Returns `(id, body)` pairs so the merge step can parse each body.
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
        url: String,
        state: String,
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
            url: i.url,
            state: i.state,
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
            url: p.url,
            state: p.state,
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

    // -----------------------------------------------------------------------
    // Sectioned-comment primitive tests (#1348)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_sections_new_markers() {
        let body = "\
<!-- homeboy:comment-key=ci:homeboy -->
## Homeboy Results — `homeboy`

<!-- homeboy:section-key=lint:start -->
:white_check_mark: **lint**
<!-- homeboy:section-key=lint:end -->

<!-- homeboy:section-key=test:start -->
:x: **test**
1 failure
<!-- homeboy:section-key=test:end -->
";
        let sections = parse_comment_sections(body);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "lint");
        assert_eq!(sections[0].1, ":white_check_mark: **lint**");
        assert_eq!(sections[1].0, "test");
        assert!(sections[1].1.contains(":x: **test**"));
        assert!(sections[1].1.contains("1 failure"));
    }

    #[test]
    fn parse_sections_legacy_markers() {
        // Body shape written by homeboy-action today (merge-pr-comment.py).
        let body = "\
<!-- homeboy-action-results:key=ci:homeboy -->
## Homeboy Results — `homeboy`

<!-- homeboy-action-section:key=lint:start -->
:white_check_mark: **lint**
<!-- homeboy-action-section:key=lint:end -->

<!-- homeboy-action-section:key=test:start -->
:x: **test**
<!-- homeboy-action-section:key=test:end -->
";
        let sections = parse_comment_sections(body);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "lint");
        assert!(sections[0].1.contains("white_check_mark"));
        assert_eq!(sections[1].0, "test");
    }

    #[test]
    fn parse_sections_mixed_markers() {
        // Edge: a body that was rendered half-new, half-legacy (shouldn't
        // happen in practice, but the parser must not die if it does).
        let body = "\
<!-- homeboy:comment-key=ci:homeboy -->

<!-- homeboy:section-key=lint:start -->
new-style lint
<!-- homeboy:section-key=lint:end -->

<!-- homeboy-action-section:key=test:start -->
legacy-style test
<!-- homeboy-action-section:key=test:end -->
";
        let sections = parse_comment_sections(body);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].1, "new-style lint");
        assert_eq!(sections[1].1, "legacy-style test");
    }

    #[test]
    fn parse_sections_returns_empty_for_unmarkered_body() {
        let body = "Just a regular PR comment.\n\nNo markers here.\n";
        assert!(parse_comment_sections(body).is_empty());
    }

    #[test]
    fn parse_sections_skips_malformed_blocks() {
        // Start without matching end — should be ignored, not panic.
        let body = "\
<!-- homeboy:section-key=lint:start -->
never-ends
";
        assert!(parse_comment_sections(body).is_empty());
    }

    #[test]
    fn render_comment_writes_new_markers() {
        let sections = vec![
            ("lint".to_string(), "lint body".to_string()),
            ("test".to_string(), "test body".to_string()),
        ];
        let out = render_comment("ci:homeboy", Some("## Header"), &sections, None);
        assert!(out.starts_with("<!-- homeboy:comment-key=ci:homeboy -->\n"));
        assert!(out.contains("## Header"));
        assert!(out.contains("<!-- homeboy:section-key=lint:start -->"));
        assert!(out.contains("<!-- homeboy:section-key=lint:end -->"));
        assert!(out.contains("<!-- homeboy:section-key=test:start -->"));
        // No legacy markers in rendered output.
        assert!(!out.contains("homeboy-action-results"));
        assert!(!out.contains("homeboy-action-section"));
    }

    #[test]
    fn render_comment_round_trips_through_parse() {
        let sections = vec![
            ("audit".to_string(), "audit body\nmulti-line".to_string()),
            ("lint".to_string(), "lint body".to_string()),
        ];
        let rendered = render_comment("ci:x", None, &sections, None);
        let reparsed = parse_comment_sections(&rendered);

        // Alphabetical default → audit before lint.
        assert_eq!(reparsed.len(), 2);
        assert_eq!(reparsed[0].0, "audit");
        assert_eq!(reparsed[0].1, "audit body\nmulti-line");
        assert_eq!(reparsed[1].0, "lint");
    }

    #[test]
    fn render_comment_alphabetical_by_default() {
        let sections = vec![
            ("test".to_string(), "t".to_string()),
            ("audit".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
        ];
        let out = render_comment("k", None, &sections, None);
        let audit_pos = out.find("section-key=audit:start").unwrap();
        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        assert!(audit_pos < lint_pos);
        assert!(lint_pos < test_pos);
    }

    #[test]
    fn render_comment_honors_explicit_order() {
        let sections = vec![
            ("audit".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
            ("test".to_string(), "t".to_string()),
        ];
        let order = vec!["lint".to_string(), "test".to_string(), "audit".to_string()];
        let out = render_comment("k", None, &sections, Some(&order));
        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        let audit_pos = out.find("section-key=audit:start").unwrap();
        assert!(lint_pos < test_pos);
        assert!(test_pos < audit_pos);
    }

    #[test]
    fn render_comment_unknown_keys_appended_alphabetically() {
        let sections = vec![
            ("zeta".to_string(), "z".to_string()),
            ("alpha".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
            ("test".to_string(), "t".to_string()),
        ];
        // Only lint+test in explicit order — zeta and alpha are "unknown".
        let order = vec!["lint".to_string(), "test".to_string()];
        let out = render_comment("k", None, &sections, Some(&order));

        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        let alpha_pos = out.find("section-key=alpha:start").unwrap();
        let zeta_pos = out.find("section-key=zeta:start").unwrap();

        // Explicit-order keys come first in their listed order.
        assert!(lint_pos < test_pos);
        // Unknown keys appended after, alphabetical among themselves.
        assert!(test_pos < alpha_pos);
        assert!(alpha_pos < zeta_pos);
    }

    #[test]
    fn render_comment_explicit_order_ignores_missing_keys() {
        let sections = vec![("lint".to_string(), "l".to_string())];
        // `test` is in the order but not present in sections — should not appear
        // in output.
        let order = vec!["test".to_string(), "lint".to_string()];
        let out = render_comment("k", None, &sections, Some(&order));
        assert!(!out.contains("section-key=test:start"));
        assert!(out.contains("section-key=lint:start"));
    }

    #[test]
    fn merge_section_replaces_existing_preserves_position() {
        let sections = vec![
            ("lint".to_string(), "old lint".to_string()),
            ("test".to_string(), "old test".to_string()),
        ];
        let merged = merge_section(sections, "lint", "new lint".to_string());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, "lint");
        assert_eq!(merged[0].1, "new lint");
        assert_eq!(merged[1].0, "test");
        assert_eq!(merged[1].1, "old test");
    }

    #[test]
    fn merge_section_appends_when_absent() {
        let sections = vec![("lint".to_string(), "lint".to_string())];
        let merged = merge_section(sections, "test", "test".to_string());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].0, "test");
        assert_eq!(merged[1].1, "test");
    }

    #[test]
    fn legacy_markers_rerender_as_new_markers() {
        // This is the rollout compat path: an existing homeboy-action comment
        // is read, parsed, re-rendered. The output must use ONLY new markers
        // so subsequent reads are self-consistent.
        let legacy_body = "\
<!-- homeboy-action-results:key=ci:homeboy -->
## Homeboy Results — `homeboy`

<!-- homeboy-action-section:key=lint:start -->
:white_check_mark: lint passed
<!-- homeboy-action-section:key=lint:end -->

<!-- homeboy-action-section:key=test:start -->
:x: 1 test failure
<!-- homeboy-action-section:key=test:end -->
";
        let sections = parse_comment_sections(legacy_body);
        assert_eq!(sections.len(), 2);

        let rendered = render_comment(
            "ci:homeboy",
            Some("## Homeboy Results — `homeboy`"),
            &sections,
            None,
        );

        // New markers only.
        assert!(rendered.contains("<!-- homeboy:comment-key=ci:homeboy -->"));
        assert!(rendered.contains("<!-- homeboy:section-key=lint:start -->"));
        assert!(rendered.contains("<!-- homeboy:section-key=test:end -->"));
        assert!(!rendered.contains("homeboy-action-results"));
        assert!(!rendered.contains("homeboy-action-section"));

        // Content preserved.
        assert!(rendered.contains(":white_check_mark: lint passed"));
        assert!(rendered.contains(":x: 1 test failure"));
    }

    #[test]
    fn comment_matches_key_recognizes_both_marker_formats() {
        let new_body = "<!-- homeboy:comment-key=ci:x -->\nbody\n";
        let legacy_body = "<!-- homeboy-action-results:key=ci:x -->\nbody\n";
        let unrelated = "<!-- homeboy:comment-key=ci:y -->\nbody\n";
        let unmarked = "just a comment\n";

        assert!(comment_matches_key(new_body, "ci:x"));
        assert!(comment_matches_key(legacy_body, "ci:x"));
        assert!(!comment_matches_key(unrelated, "ci:x"));
        assert!(!comment_matches_key(unmarked, "ci:x"));
    }

    #[test]
    fn extract_header_reads_between_markers() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
## Homeboy Results — `homeboy`

<!-- homeboy:section-key=lint:start -->
body
<!-- homeboy:section-key=lint:end -->
";
        assert_eq!(
            extract_header(body),
            Some("## Homeboy Results — `homeboy`".to_string())
        );
    }

    #[test]
    fn extract_header_empty_when_no_header_text() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
<!-- homeboy:section-key=lint:start -->
body
<!-- homeboy:section-key=lint:end -->
";
        assert_eq!(extract_header(body), None);
    }

    #[test]
    fn extract_header_legacy_markers() {
        let body = "\
<!-- homeboy-action-results:key=ci:x -->
## Legacy Header

<!-- homeboy-action-section:key=lint:start -->
body
<!-- homeboy-action-section:key=lint:end -->
";
        assert_eq!(extract_header(body), Some("## Legacy Header".to_string()));
    }

    #[test]
    fn parse_comments_list_filters_by_key_and_handles_pagination() {
        // Two pages: gh --paginate concatenates JSON arrays with no separator.
        let raw = r#"[
            {"id": 1, "body": "<!-- homeboy:comment-key=ci:x -->\nsection"},
            {"id": 2, "body": "unrelated"}
        ][
            {"id": 3, "body": "<!-- homeboy-action-results:key=ci:x -->\nlegacy section"},
            {"id": 4, "body": "<!-- homeboy:comment-key=other -->\nother"}
        ]"#;
        let got = parse_comments_list_json(raw, "ci:x").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, 1);
        assert_eq!(got[1].id, 3);
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
