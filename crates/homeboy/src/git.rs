use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::component;
use crate::error::{Error, Result};
use crate::json::{read_json_spec_to_string, BulkResult, BulkSummary, ItemOutcome};
use crate::project;

// ============================================================================
// Low-level Git Primitives (path-based)
// ============================================================================

/// Clone a git repository to a target directory.
pub fn clone_repo(url: &str, target_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["clone", url, &target_dir.to_string_lossy()])
        .output()
        .map_err(|e| Error::git_command_failed(format!("Failed to run git clone: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git clone failed: {}",
            stderr
        )));
    }

    Ok(())
}

/// Pull latest changes in a git repository.
pub fn pull_repo(repo_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["pull"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| Error::git_command_failed(format!("Failed to run git pull: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git pull failed: {}",
            stderr
        )));
    }

    Ok(())
}

/// Check if a git working directory has no uncommitted changes.
pub fn is_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(output) => output.status.success() && output.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Pull with fast-forward only, inheriting stdio for interactive output.
pub fn pull_ff_only_interactive(path: &Path) -> Result<()> {
    use std::process::Stdio;

    let status = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::git_command_failed(format!("Failed to run git pull: {}", e)))?;

    if !status.success() {
        return Err(Error::git_command_failed(
            "git pull --ff-only failed".to_string(),
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitInfo {
    pub hash: String,
    pub subject: String,
    pub category: CommitCategory,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum CommitCategory {
    Breaking,
    Feature,
    Fix,
    Docs,
    Chore,
    Other,
}

impl CommitCategory {
    pub fn prefix(&self) -> Option<&'static str> {
        match self {
            CommitCategory::Breaking => Some("BREAKING"),
            CommitCategory::Feature => Some("feat"),
            CommitCategory::Fix => Some("fix"),
            CommitCategory::Docs => Some("docs"),
            CommitCategory::Chore => Some("chore"),
            CommitCategory::Other => None,
        }
    }
}

/// Parse a commit subject into a category based on conventional commit format.
/// Falls back to Other if no pattern matches - this is fine, commits still get included.
pub fn parse_conventional_commit(subject: &str) -> CommitCategory {
    let lower = subject.to_lowercase();

    if lower.contains("breaking change") || subject.contains("!:") {
        CommitCategory::Breaking
    } else if lower.starts_with("feat") {
        CommitCategory::Feature
    } else if lower.starts_with("fix") {
        CommitCategory::Fix
    } else if lower.starts_with("docs") {
        CommitCategory::Docs
    } else if lower.starts_with("chore") {
        CommitCategory::Chore
    } else {
        CommitCategory::Other
    }
}

/// Get the latest git tag in the repository.
/// Returns None if no tags exist.
pub fn get_latest_tag(path: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::other(format!("Failed to run git describe: {}", e)))?;

    if !output.status.success() {
        // No tags exist - this is fine, not an error
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No names found") || stderr.contains("No tags can describe") {
            return Ok(None);
        }
        return Err(Error::other(format!("git describe failed: {}", stderr)));
    }

    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tag.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tag))
    }
}

const DEFAULT_COMMIT_LIMIT: usize = 10;

/// Find the most recent commit containing a version number in its message.
/// Matches strict patterns: v1.0.0, bump to X, release X, version X
/// Returns the commit hash if found, None otherwise.
pub fn find_version_commit(path: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["log", "-200", "--format=%h|%s"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::other(format!("Failed to run git log: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!("git log failed: {}", stderr)));
    }

    let version_pattern =
        Regex::new(r"(?i)(?:^v|bump\s+(?:to\s+)?|release\s+v?|version\s+)(\d+\.\d+(?:\.\d+)?)")
            .expect("Invalid regex pattern");

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() == 2 {
            let hash = parts[0];
            let subject = parts[1];
            if version_pattern.is_match(subject) {
                return Ok(Some(hash.to_string()));
            }
        }
    }

    Ok(None)
}

/// Get the last N commits from the repository.
pub fn get_last_n_commits(path: &str, n: usize) -> Result<Vec<CommitInfo>> {
    let output = Command::new("git")
        .args(["log", &format!("-{}", n), "--format=%h|%s"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::other(format!("Failed to run git log: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!("git log failed: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commits = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() == 2 {
                let hash = parts[0].to_string();
                let subject = parts[1].to_string();
                let category = parse_conventional_commit(&subject);
                Some(CommitInfo {
                    hash,
                    subject,
                    category,
                })
            } else {
                None
            }
        })
        .collect();

    Ok(commits)
}

/// Get commits since a given tag (or all commits if tag is None).
/// Returns commits in reverse chronological order (newest first).
pub fn get_commits_since_tag(path: &str, tag: Option<&str>) -> Result<Vec<CommitInfo>> {
    let range = match tag {
        Some(t) => format!("{}..HEAD", t),
        None => "HEAD".to_string(),
    };

    let output = Command::new("git")
        .args(["log", &range, "--format=%h|%s"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::other(format!("Failed to run git log: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!("git log failed: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commits = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() == 2 {
                let hash = parts[0].to_string();
                let subject = parts[1].to_string();
                let category = parse_conventional_commit(&subject);
                Some(CommitInfo {
                    hash,
                    subject,
                    category,
                })
            } else {
                None
            }
        })
        .collect();

    Ok(commits)
}

/// Convert commits to changelog entries.
/// Strips conventional commit prefixes for cleaner changelog.
pub fn commits_to_changelog_entries(commits: &[CommitInfo]) -> Vec<String> {
    commits
        .iter()
        .map(|c| {
            // Strip conventional commit prefix if present
            let subject = strip_conventional_prefix(&c.subject);
            subject.to_string()
        })
        .collect()
}

/// Strip conventional commit prefix from a subject line.
/// "feat: Add new feature" -> "Add new feature"
/// "fix(scope): Fix bug" -> "Fix bug"
fn strip_conventional_prefix(subject: &str) -> &str {
    // Pattern: type(scope)?: message or type!: message
    if let Some(pos) = subject.find(": ") {
        let prefix = &subject[..pos];
        // Check if it looks like a conventional commit prefix
        if prefix
            .chars()
            .all(|c| c.is_alphanumeric() || c == '(' || c == ')' || c == '!')
        {
            return &subject[pos + 2..];
        }
    }
    subject
}

// === Component Git Operations ===

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOutput {
    pub component_id: String,
    pub path: String,
    pub action: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}


// === Changes Output Types ===

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineSource {
    Tag,
    VersionCommit,
    LastNCommits,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UncommittedChanges {
    pub has_changes: bool,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesOutput {
    pub component_id: String,
    pub path: String,
    pub success: bool,
    pub latest_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_source: Option<BaselineSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    pub commits: Vec<CommitInfo>,
    pub uncommitted: UncommittedChanges,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}


// Input types for JSON parsing
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkIdsInput {
    component_ids: Vec<String>,
    #[serde(default)]
    tags: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkCommitInput {
    components: Vec<CommitSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitSpec {
    id: String,
    message: String,
}

fn get_component_path(component_id: &str) -> Result<String> {
    let comp = component::load(component_id)?;
    Ok(comp.local_path)
}

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").args(args).current_dir(path).output()
}

fn to_exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Get git status for a component.
pub fn status(component_id: &str) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;

    Ok(GitOutput {
        component_id: component_id.to_string(),
        path,
        action: "status".to_string(),
        success: output.status.success(),
        exit_code: to_exit_code(output.status),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Get git status for multiple components from JSON spec.
pub fn status_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse bulk status input".to_string()))
    })?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in &input.component_ids {
        match status(id) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(BulkResult {
        action: "status".to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    })
}

/// Stage all changes and commit for a component.
pub fn commit(component_id: &str, message: &str) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let status_output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;

    let status_stdout = String::from_utf8_lossy(&status_output.stdout).to_string();

    if status_stdout.trim().is_empty() {
        return Ok(GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "commit".to_string(),
            success: true,
            exit_code: 0,
            stdout: "Nothing to commit, working tree clean".to_string(),
            stderr: String::new(),
        });
    }

    let add_output = execute_git(&path, &["add", "."]).map_err(|e| Error::other(e.to_string()))?;

    if !add_output.status.success() {
        let exit_code = to_exit_code(add_output.status);
        return Ok(GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "commit".to_string(),
            success: false,
            exit_code,
            stdout: String::from_utf8_lossy(&add_output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&add_output.stderr).to_string(),
        });
    }

    let commit_output =
        execute_git(&path, &["commit", "-m", message]).map_err(|e| Error::other(e.to_string()))?;

    let exit_code = to_exit_code(commit_output.status);

    Ok(GitOutput {
        component_id: component_id.to_string(),
        path,
        action: "commit".to_string(),
        success: commit_output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&commit_output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&commit_output.stderr).to_string(),
    })
}

/// Commit multiple components from JSON spec.
pub fn commit_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkCommitInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse bulk commit input".to_string()))
    })?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for spec in &input.components {
        match commit(&spec.id, &spec.message) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: spec.id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: spec.id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(BulkResult {
        action: "commit".to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    })
}

/// Push local commits for a component.
pub fn push(component_id: &str, tags: bool) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let push_args: Vec<&str> = if tags {
        vec!["push", "--tags"]
    } else {
        vec!["push"]
    };

    let output = execute_git(&path, &push_args).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: component_id.to_string(),
        path,
        action: "push".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Push multiple components from JSON spec.
pub fn push_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse bulk push input".to_string()))
    })?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let push_tags = input.tags;

    for id in &input.component_ids {
        match push(id, push_tags) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(BulkResult {
        action: "push".to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    })
}

/// Pull remote changes for a component.
pub fn pull(component_id: &str) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let output = execute_git(&path, &["pull"]).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: component_id.to_string(),
        path,
        action: "pull".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Pull multiple components from JSON spec.
pub fn pull_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse bulk pull input".to_string()))
    })?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in &input.component_ids {
        match pull(id) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(BulkResult {
        action: "pull".to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    })
}

/// Create a git tag for a component.
pub fn tag(component_id: &str, tag_name: &str, message: Option<&str>) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let tag_args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", tag_name, "-m", msg],
        None => vec!["tag", tag_name],
    };

    let output = execute_git(&path, &tag_args).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: component_id.to_string(),
        path,
        action: "tag".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

// === Changes Operations ===

/// Parse git status output into structured uncommitted changes.
pub fn get_uncommitted_changes(path: &str) -> Result<UncommittedChanges> {
    let output = execute_git(path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!("git status failed: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for line in stdout.lines() {
        if line.len() < 3 {
            continue;
        }
        let index_status = line.chars().next().unwrap_or(' ');
        let worktree_status = line.chars().nth(1).unwrap_or(' ');
        let file_path = line[3..].to_string();

        match (index_status, worktree_status) {
            ('?', '?') => untracked.push(file_path),
            (idx, wt) => {
                if idx != ' ' && idx != '?' {
                    staged.push(file_path.clone());
                }
                if wt != ' ' && wt != '?' {
                    unstaged.push(file_path);
                }
            }
        }
    }

    let has_changes = !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty();

    Ok(UncommittedChanges {
        has_changes,
        staged,
        unstaged,
        untracked,
    })
}

/// Get diff of uncommitted changes.
pub fn get_diff(path: &str) -> Result<String> {
    // Get both staged and unstaged diff
    let staged =
        execute_git(path, &["diff", "--cached"]).map_err(|e| Error::other(e.to_string()))?;
    let unstaged = execute_git(path, &["diff"]).map_err(|e| Error::other(e.to_string()))?;

    let staged_diff = String::from_utf8_lossy(&staged.stdout);
    let unstaged_diff = String::from_utf8_lossy(&unstaged.stdout);

    let mut result = String::new();
    if !staged_diff.is_empty() {
        result.push_str("=== Staged Changes ===\n");
        result.push_str(&staged_diff);
    }
    if !unstaged_diff.is_empty() {
        if !result.is_empty() {
            result.push_str("\n");
        }
        result.push_str("=== Unstaged Changes ===\n");
        result.push_str(&unstaged_diff);
    }

    Ok(result)
}

/// Get all changes for a component since its latest tag.
pub fn changes(
    component_id: &str,
    since_tag: Option<&str>,
    include_diff: bool,
) -> Result<ChangesOutput> {
    let path = get_component_path(component_id)?;

    // Determine baseline using fallback chain: tag -> version commit -> last N commits
    let (latest_tag, baseline_source, baseline_ref, warning) = match since_tag {
        Some(t) => (
            Some(t.to_string()),
            Some(BaselineSource::Tag),
            Some(t.to_string()),
            None,
        ),
        None => {
            if let Some(tag) = get_latest_tag(&path)? {
                (
                    Some(tag.clone()),
                    Some(BaselineSource::Tag),
                    Some(tag),
                    None,
                )
            } else if let Some(version_hash) = find_version_commit(&path)? {
                (
                    None,
                    Some(BaselineSource::VersionCommit),
                    Some(version_hash),
                    Some("No tags found. Using most recent version commit as baseline.".to_string()),
                )
            } else {
                (
                    None,
                    Some(BaselineSource::LastNCommits),
                    None,
                    Some(format!(
                        "No tags or version commits found. Showing last {} commits.",
                        DEFAULT_COMMIT_LIMIT
                    )),
                )
            }
        }
    };

    // Get commits based on baseline source
    let commits = match baseline_source {
        Some(BaselineSource::LastNCommits) => get_last_n_commits(&path, DEFAULT_COMMIT_LIMIT)?,
        _ => get_commits_since_tag(&path, baseline_ref.as_deref())?,
    };

    // Get uncommitted changes
    let uncommitted = get_uncommitted_changes(&path)?;

    // Get diff if requested
    let diff = if include_diff {
        Some(get_diff(&path)?)
    } else {
        None
    };

    Ok(ChangesOutput {
        component_id: component_id.to_string(),
        path,
        success: true,
        latest_tag,
        baseline_source,
        baseline_ref,
        commits,
        uncommitted,
        diff,
        warning,
        error: None,
    })
}

fn build_bulk_changes_output(component_ids: &[String], include_diff: bool) -> BulkResult<ChangesOutput> {
    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in component_ids {
        match changes(id, None, include_diff) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id: id.clone(),
                    result: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    BulkResult {
        action: "changes".to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    }
}

/// Get changes for multiple components from JSON spec.
pub fn changes_bulk(json_spec: &str, include_diff: bool) -> Result<BulkResult<ChangesOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse bulk changes input".to_string()))
    })?;

    Ok(build_bulk_changes_output(
        &input.component_ids,
        include_diff,
    ))
}

/// Get changes for all components in a project.
pub fn changes_project(project_id: &str, include_diff: bool) -> Result<BulkResult<ChangesOutput>> {
    let proj = project::load(project_id)?;
    Ok(build_bulk_changes_output(&proj.component_ids, include_diff))
}

/// Get changes for specific components in a project (filtered).
pub fn changes_project_filtered(
    project_id: &str,
    component_ids: &[String],
    include_diff: bool,
) -> Result<BulkResult<ChangesOutput>> {
    let proj = project::load(project_id)?;

    // Filter to only components that are in the project
    let filtered: Vec<String> = component_ids
        .iter()
        .filter(|id| proj.component_ids.contains(id))
        .cloned()
        .collect();

    if filtered.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            format!(
                "None of the specified components are in project '{}'. Available: {}",
                project_id,
                proj.component_ids.join(", ")
            ),
            None,
            None,
        ));
    }

    Ok(build_bulk_changes_output(&filtered, include_diff))
}

/// Get changes for the current working directory (ad-hoc mode).
pub fn changes_cwd(include_diff: bool) -> Result<ChangesOutput> {
    let path = std::env::current_dir()
        .map_err(|e| Error::other(format!("Failed to get current directory: {}", e)))?
        .to_string_lossy()
        .to_string();

    // Determine baseline using fallback chain: tag -> version commit -> last N commits
    let (latest_tag, baseline_source, baseline_ref, warning) =
        if let Some(tag) = get_latest_tag(&path)? {
            (
                Some(tag.clone()),
                Some(BaselineSource::Tag),
                Some(tag),
                None,
            )
        } else if let Some(version_hash) = find_version_commit(&path)? {
            (
                None,
                Some(BaselineSource::VersionCommit),
                Some(version_hash),
                Some("No tags found. Using most recent version commit as baseline.".to_string()),
            )
        } else {
            (
                None,
                Some(BaselineSource::LastNCommits),
                None,
                Some(format!(
                    "No tags or version commits found. Showing last {} commits.",
                    DEFAULT_COMMIT_LIMIT
                )),
            )
        };

    // Get commits based on baseline source
    let commits = match baseline_source {
        Some(BaselineSource::LastNCommits) => get_last_n_commits(&path, DEFAULT_COMMIT_LIMIT)?,
        _ => get_commits_since_tag(&path, baseline_ref.as_deref())?,
    };

    let uncommitted = get_uncommitted_changes(&path)?;
    let diff = if include_diff {
        Some(get_diff(&path)?)
    } else {
        None
    };

    Ok(ChangesOutput {
        component_id: "cwd".to_string(),
        path,
        success: true,
        latest_tag,
        baseline_source,
        baseline_ref,
        commits,
        uncommitted,
        diff,
        warning,
        error: None,
    })
}

// === CWD Git Operations ===

fn get_cwd_path() -> Result<String> {
    std::env::current_dir()
        .map_err(|e| Error::other(format!("Failed to get current directory: {}", e)))
        .map(|p| p.to_string_lossy().to_string())
}

/// Get git status for the current working directory.
pub fn status_cwd() -> Result<GitOutput> {
    let path = get_cwd_path()?;

    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;

    Ok(GitOutput {
        component_id: "cwd".to_string(),
        path,
        action: "status".to_string(),
        success: output.status.success(),
        exit_code: to_exit_code(output.status),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Stage all changes and commit in the current working directory.
pub fn commit_cwd(message: &str) -> Result<GitOutput> {
    let path = get_cwd_path()?;

    let status_output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;

    let status_stdout = String::from_utf8_lossy(&status_output.stdout).to_string();

    if status_stdout.trim().is_empty() {
        return Ok(GitOutput {
            component_id: "cwd".to_string(),
            path,
            action: "commit".to_string(),
            success: true,
            exit_code: 0,
            stdout: "Nothing to commit, working tree clean".to_string(),
            stderr: String::new(),
        });
    }

    let add_output = execute_git(&path, &["add", "."]).map_err(|e| Error::other(e.to_string()))?;

    if !add_output.status.success() {
        let exit_code = to_exit_code(add_output.status);
        return Ok(GitOutput {
            component_id: "cwd".to_string(),
            path,
            action: "commit".to_string(),
            success: false,
            exit_code,
            stdout: String::from_utf8_lossy(&add_output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&add_output.stderr).to_string(),
        });
    }

    let commit_output =
        execute_git(&path, &["commit", "-m", message]).map_err(|e| Error::other(e.to_string()))?;

    let exit_code = to_exit_code(commit_output.status);

    Ok(GitOutput {
        component_id: "cwd".to_string(),
        path,
        action: "commit".to_string(),
        success: commit_output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&commit_output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&commit_output.stderr).to_string(),
    })
}

/// Push local commits in the current working directory.
pub fn push_cwd(tags: bool) -> Result<GitOutput> {
    let path = get_cwd_path()?;

    let push_args: Vec<&str> = if tags {
        vec!["push", "--tags"]
    } else {
        vec!["push"]
    };

    let output = execute_git(&path, &push_args).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: "cwd".to_string(),
        path,
        action: "push".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Pull remote changes in the current working directory.
pub fn pull_cwd() -> Result<GitOutput> {
    let path = get_cwd_path()?;

    let output = execute_git(&path, &["pull"]).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: "cwd".to_string(),
        path,
        action: "pull".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Create a git tag in the current working directory.
pub fn tag_cwd(tag_name: &str, message: Option<&str>) -> Result<GitOutput> {
    let path = get_cwd_path()?;

    let tag_args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", tag_name, "-m", msg],
        None => vec!["tag", tag_name],
    };

    let output = execute_git(&path, &tag_args).map_err(|e| Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok(GitOutput {
        component_id: "cwd".to_string(),
        path,
        action: "tag".to_string(),
        success: output.status.success(),
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conventional_commit_feat() {
        assert_eq!(
            parse_conventional_commit("feat: Add new feature"),
            CommitCategory::Feature
        );
        assert_eq!(
            parse_conventional_commit("feat(scope): Add scoped feature"),
            CommitCategory::Feature
        );
    }

    #[test]
    fn parse_conventional_commit_fix() {
        assert_eq!(
            parse_conventional_commit("fix: Fix a bug"),
            CommitCategory::Fix
        );
    }

    #[test]
    fn parse_conventional_commit_breaking() {
        assert_eq!(
            parse_conventional_commit("feat!: Breaking change"),
            CommitCategory::Breaking
        );
        assert_eq!(
            parse_conventional_commit("BREAKING CHANGE: Something big"),
            CommitCategory::Breaking
        );
    }

    #[test]
    fn parse_conventional_commit_other() {
        assert_eq!(
            parse_conventional_commit("Random commit message"),
            CommitCategory::Other
        );
    }

    #[test]
    fn strip_conventional_prefix_works() {
        assert_eq!(
            strip_conventional_prefix("feat: Add feature"),
            "Add feature"
        );
        assert_eq!(
            strip_conventional_prefix("fix(shell): Fix escaping"),
            "Fix escaping"
        );
        assert_eq!(
            strip_conventional_prefix("Regular commit"),
            "Regular commit"
        );
    }
}
