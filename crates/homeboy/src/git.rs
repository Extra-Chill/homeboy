use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::component;
use crate::json::read_json_spec_to_string;
use crate::error::{Error, Result};

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
        return Err(Error::git_command_failed(format!("git clone failed: {}", stderr)));
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
        return Err(Error::git_command_failed(format!("git pull failed: {}", stderr)));
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub hash: String,
    pub subject: String,
    pub category: CommitCategory,
}

#[derive(Debug, Clone, PartialEq)]
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
        return Err(Error::other(format!(
            "git describe failed: {}",
            stderr
        )));
    }

    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tag.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tag))
    }
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkGitOutput {
    pub action: String,
    pub results: Vec<GitOutput>,
    pub summary: BulkSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
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
pub fn status_bulk(json_spec: &str) -> Result<BulkGitOutput> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse bulk status input".to_string())))?;

    let mut results = Vec::new();
    for id in &input.component_ids {
        match status(id) {
            Ok(output) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "status".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;

    Ok(BulkGitOutput {
        action: "status".to_string(),
        results,
        summary: BulkSummary {
            total: input.component_ids.len(),
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

    let add_output = execute_git(&path, &["add", "."])
        .map_err(|e| Error::other(e.to_string()))?;

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

    let commit_output = execute_git(&path, &["commit", "-m", message])
        .map_err(|e| Error::other(e.to_string()))?;

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
pub fn commit_bulk(json_spec: &str) -> Result<BulkGitOutput> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkCommitInput = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse bulk commit input".to_string())))?;

    let mut results = Vec::new();
    for spec in &input.components {
        match commit(&spec.id, &spec.message) {
            Ok(output) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: spec.id.clone(),
                path: String::new(),
                action: "commit".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;

    Ok(BulkGitOutput {
        action: "commit".to_string(),
        results,
        summary: BulkSummary {
            total: input.components.len(),
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

    let output = execute_git(&path, &push_args)
        .map_err(|e| Error::other(e.to_string()))?;
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
pub fn push_bulk(json_spec: &str) -> Result<BulkGitOutput> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse bulk push input".to_string())))?;

    let mut results = Vec::new();
    let push_tags = input.tags;

    for id in &input.component_ids {
        match push(id, push_tags) {
            Ok(output) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "push".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;

    Ok(BulkGitOutput {
        action: "push".to_string(),
        results,
        summary: BulkSummary {
            total: input.component_ids.len(),
            succeeded,
            failed,
        },
    })
}

/// Pull remote changes for a component.
pub fn pull(component_id: &str) -> Result<GitOutput> {
    let path = get_component_path(component_id)?;

    let output = execute_git(&path, &["pull"])
        .map_err(|e| Error::other(e.to_string()))?;
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
pub fn pull_bulk(json_spec: &str) -> Result<BulkGitOutput> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some("parse bulk pull input".to_string())))?;

    let mut results = Vec::new();
    for id in &input.component_ids {
        match pull(id) {
            Ok(output) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "pull".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;

    Ok(BulkGitOutput {
        action: "pull".to_string(),
        results,
        summary: BulkSummary {
            total: input.component_ids.len(),
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

    let output = execute_git(&path, &tag_args)
        .map_err(|e| Error::other(e.to_string()))?;
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
