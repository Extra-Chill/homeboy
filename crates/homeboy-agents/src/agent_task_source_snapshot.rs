//! Task source snapshot (#18884).
//!
//! At Cook admission Homeboy reads the GitHub task the Cook was given and
//! records what it read as structured prompt input, so a Cook never depends on
//! the agent deciding to fetch its own spec:
//!
//! - an issue: its title, body and every comment;
//! - a pull request rework: its title and body, plus the comments and reviews
//!   posted after the PR's last head commit.
//!
//! The snapshot travels as data under `inputs.homeboy.task_source` (Homeboy
//! authors no agent-facing prose, #15818), and its content digest becomes the
//! `revision` of the request's `task` source ref.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task::{AgentTaskRequest, AgentTaskSourceRef};
use homeboy_core::{Error, Result};

pub const TASK_SOURCE_SNAPSHOT_SCHEMA: &str = "homeboy/task-source-snapshot/v1";

/// Key of the snapshot under `inputs.homeboy`.
pub const TASK_SOURCE_INPUT_KEY: &str = "task_source";

/// Bound on the characters of the body and of each comment or review.
pub const TASK_SOURCE_TEXT_MAX_CHARS: usize = 32_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskSourceKind {
    Issue,
    PullRequest,
}

/// One comment or review as it stood at admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSourceEntry {
    pub author: String,
    pub created_at: String,
    pub body: String,
    /// Review verdict (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What Homeboy read from the task at admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSourceSnapshot {
    pub schema: String,
    pub kind: TaskSourceKind,
    pub url: String,
    /// `sha256:` digest of the snapshot content; the `task` source ref's revision.
    pub revision: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<TaskSourceEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<TaskSourceEntry>,
    /// The pull request's head commit at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// Comments and reviews postdate this head commit timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Pull request comments and reviews left out because they predate `since`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_before_head: usize,
    /// The issue a pull request rework was resolved from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_issue: Option<Box<TaskSourceSnapshot>>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// The GitHub task a URL names, when it names one.
pub fn task_source_kind(url: &str) -> Option<TaskSourceKind> {
    let path = url.trim().strip_prefix("https://github.com/")?;
    let path = path.split(['#', '?']).next()?.trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, repo, kind, number] = parts.as_slice() else {
        return None;
    };
    if owner.is_empty() || repo.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match *kind {
        "issues" => Some(TaskSourceKind::Issue),
        "pull" => Some(TaskSourceKind::PullRequest),
        _ => None,
    }
}

/// Snapshot an issue from `gh issue view --json title,body,comments`.
pub fn issue_snapshot(url: &str, view: &Value) -> TaskSourceSnapshot {
    let comments = entries(&view["comments"], "createdAt");
    finish(TaskSourceSnapshot {
        schema: TASK_SOURCE_SNAPSHOT_SCHEMA.to_string(),
        kind: TaskSourceKind::Issue,
        url: url.to_string(),
        revision: String::new(),
        title: text(&view["title"]),
        body: bounded(&text(&view["body"])),
        comments,
        reviews: Vec::new(),
        head_sha: None,
        since: None,
        omitted_before_head: 0,
        linked_issue: None,
    })
}

/// Snapshot a pull request from
/// `gh pr view --json title,body,comments,reviews,commits,headRefOid`.
/// Only comments and reviews posted after the head commit are kept.
pub fn pull_request_snapshot(url: &str, view: &Value) -> TaskSourceSnapshot {
    let head_sha = view["headRefOid"]
        .as_str()
        .filter(|sha| !sha.is_empty())
        .map(str::to_string);
    let commits = view["commits"].as_array().cloned().unwrap_or_default();
    let head_commit = head_sha
        .as_deref()
        .and_then(|sha| commits.iter().find(|commit| commit["oid"] == sha))
        .or_else(|| commits.last());
    let since = head_commit.and_then(|commit| {
        ["committedDate", "authoredDate"]
            .into_iter()
            .find_map(|key| commit[key].as_str().filter(|date| !date.is_empty()))
            .map(str::to_string)
    });
    let all_comments = entries(&view["comments"], "createdAt");
    let all_reviews = entries(&view["reviews"], "submittedAt");
    let total = all_comments.len() + all_reviews.len();
    let after_head = |entry: &TaskSourceEntry| {
        since
            .as_deref()
            .is_none_or(|since| posted_after(&entry.created_at, since))
    };
    let comments: Vec<_> = all_comments.into_iter().filter(after_head).collect();
    let reviews: Vec<_> = all_reviews
        .into_iter()
        .filter(after_head)
        .filter(|review| !review.body.trim().is_empty() || review.state.is_some())
        .collect();
    let omitted_before_head = total.saturating_sub(comments.len() + reviews.len());
    finish(TaskSourceSnapshot {
        schema: TASK_SOURCE_SNAPSHOT_SCHEMA.to_string(),
        kind: TaskSourceKind::PullRequest,
        url: url.to_string(),
        revision: String::new(),
        title: text(&view["title"]),
        body: bounded(&text(&view["body"])),
        comments,
        reviews,
        head_sha,
        since,
        omitted_before_head,
        linked_issue: None,
    })
}

/// Attach the issue a pull request rework was resolved from; the revision
/// then covers both.
pub fn with_linked_issue(
    snapshot: TaskSourceSnapshot,
    issue: TaskSourceSnapshot,
) -> TaskSourceSnapshot {
    finish(TaskSourceSnapshot {
        linked_issue: Some(Box::new(issue)),
        ..snapshot
    })
}

/// Read the task `url` names through `gh`. `Ok(None)` for a URL that names no
/// GitHub issue or pull request.
pub fn capture_task_source(url: &str) -> Result<Option<TaskSourceSnapshot>> {
    let Some(kind) = task_source_kind(url) else {
        return Ok(None);
    };
    let (subcommand, fields) = match kind {
        TaskSourceKind::Issue => ("issue", "title,body,comments"),
        TaskSourceKind::PullRequest => ("pr", "title,body,comments,reviews,commits,headRefOid"),
    };
    // One read: admission records a failed snapshot rather than stall on it.
    let output = std::process::Command::new("gh")
        .args([subcommand, "view", url, "--json", fields])
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("gh task source".to_string()))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "task_url",
            format!(
                "could not snapshot the task source: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(url.to_string()),
            None,
        ));
    }
    let view: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(Some(match kind {
        TaskSourceKind::Issue => issue_snapshot(url, &view),
        TaskSourceKind::PullRequest => pull_request_snapshot(url, &view),
    }))
}

/// Record `snapshot` as prompt input on `request` and pin the matching `task`
/// source ref to its revision.
pub fn apply_task_source_snapshot(request: &mut AgentTaskRequest, snapshot: &TaskSourceSnapshot) {
    crate::agent_task_prompt_template::merge_homeboy_facts(
        request,
        serde_json::json!({ TASK_SOURCE_INPUT_KEY: snapshot }),
    );
    let revision = Some(snapshot.revision.clone());
    match request
        .source_refs
        .iter_mut()
        .find(|source| source.kind == "task" && source.uri == snapshot.url)
    {
        Some(source) => source.revision = revision,
        None => request.source_refs.push(AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: snapshot.url.clone(),
            revision,
        }),
    }
}

fn finish(mut snapshot: TaskSourceSnapshot) -> TaskSourceSnapshot {
    snapshot.revision = String::new();
    let content = serde_json::to_vec(&snapshot).unwrap_or_default();
    snapshot.revision = format!(
        "sha256:{}",
        homeboy_engine_primitives::content_hash::sha256_hex(&content)
    );
    snapshot
}

fn entries(values: &Value, timestamp_key: &str) -> Vec<TaskSourceEntry> {
    let mut entries: Vec<TaskSourceEntry> = values
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| TaskSourceEntry {
            author: text(&entry["author"]["login"]),
            created_at: text(&entry[timestamp_key]),
            body: bounded(&text(&entry["body"])),
            state: entry["state"]
                .as_str()
                .filter(|state| !state.is_empty())
                .map(str::to_string),
            url: entry["url"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(str::to_string),
        })
        .collect();
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    entries
}

/// RFC 3339 comparison; GitHub timestamps that fail to parse compare as text.
fn posted_after(posted: &str, since: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(posted),
        chrono::DateTime::parse_from_rfc3339(since),
    ) {
        (Ok(posted), Ok(since)) => posted > since,
        _ => posted > since,
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn bounded(text: &str) -> String {
    if text.chars().count() <= TASK_SOURCE_TEXT_MAX_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(TASK_SOURCE_TEXT_MAX_CHARS).collect();
    format!("{kept}\n[truncated by Homeboy at {TASK_SOURCE_TEXT_MAX_CHARS} characters]")
}

#[cfg(test)]
#[path = "agent_task_source_snapshot_tests.rs"]
mod tests;
