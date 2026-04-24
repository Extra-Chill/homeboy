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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Default)]
pub struct PrCommentOptions {
    pub number: u64,
    pub body: String,
    /// Optional marker key. When set, the body is prefixed with an HTML comment
    /// (`<!-- homeboy:key=<key> -->`) and the function looks for an existing
    /// comment carrying that marker — if found, updates it in place. Otherwise
    /// a new comment is posted. This is the mechanism behind sticky CI comments.
    pub key: Option<String>,
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
    let limit = if options.limit == 0 { 30 } else { options.limit };
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
pub fn pr_create(
    component_id: Option<&str>,
    options: PrCreateOptions,
) -> Result<GithubPrOutput> {
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
        state: None,
        base: None,
        head: None,
    })
}

/// Find PRs matching the given filter.
pub fn pr_find(component_id: Option<&str>, options: PrFindOptions) -> Result<GithubFindOutput> {
    let (id, repo) = resolve_component_github(component_id)?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let limit = if options.limit == 0 { 30 } else { options.limit };
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

/// Post a comment on a PR. When `options.key` is set, existing comments with
/// the same marker are updated in place (sticky-comment semantics) instead of
/// being appended. Returns a `GithubPrOutput` describing the resulting action.
pub fn pr_comment(component_id: Option<&str>, options: PrCommentOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id)?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let body = match &options.key {
        Some(key) => format!("{}\n{}", marker_for_key(key), options.body),
        None => options.body.clone(),
    };

    // Sticky comment flow: find-or-update.
    if let Some(key) = &options.key {
        if let Some(existing_id) = find_sticky_comment_id(&repo, options.number, key)? {
            let args: Vec<String> = vec![
                "api".into(),
                format!("repos/{}/{}/issues/comments/{}", repo.owner, repo.repo, existing_id),
                "--method".into(),
                "PATCH".into(),
                "-f".into(),
                format!("body={}", body),
            ];
            run_gh(&args)?;
            return Ok(GithubPrOutput {
                component_id: id,
                owner: repo.owner,
                repo: repo.repo,
                action: "pr.comment.update".to_string(),
                success: true,
                number: Some(options.number),
                url: None,
                title: None,
                state: None,
                base: None,
                head: None,
            });
        }
    }

    // Fresh-comment flow.
    let args: Vec<String> = vec![
        "pr".into(),
        "comment".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        "--body".into(),
        body,
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
        title: None,
        state: None,
        base: None,
        head: None,
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
        .map_err(|e| Error::internal_io(format!("Failed to invoke gh: {}", e), Some("gh".into())))?;

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
        assert_eq!(marker_for_key("ci-status"), "<!-- homeboy:key=ci-status -->");
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
}
