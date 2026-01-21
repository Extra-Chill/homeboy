use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::project;
use crate::utils::command;

// ============================================================================
// Low-level Git Primitives (path-based)
// ============================================================================

/// Clone a git repository to a target directory.
pub fn clone_repo(url: &str, target_dir: &Path) -> Result<()> {
    command::run("git", &["clone", url, &target_dir.to_string_lossy()], "git clone")
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(())
}

/// Pull latest changes in a git repository.
pub fn pull_repo(repo_dir: &Path) -> Result<()> {
    command::run_in(&repo_dir.to_string_lossy(), "git", &["pull"], "git pull")
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(())
}

/// Check if a git working directory has no uncommitted changes.
pub fn is_workdir_clean(path: &Path) -> bool {
    command::run_in_optional(&path.to_string_lossy(), "git", &["status", "--porcelain"])
        .map(|s| s.is_empty())
        .unwrap_or(false)
}

/// Get the root directory of a git repository containing the given path.
/// Returns None if the path is not within a git repository.
pub fn get_git_root(path: &str) -> Option<String> {
    command::run_in_optional(path, "git", &["rev-parse", "--show-toplevel"])
}

/// List all git-tracked markdown files in a directory.
/// Uses `git ls-files` to respect .gitignore and only include tracked/staged files.
/// Returns relative paths from the repository root.
pub fn list_tracked_markdown_files(path: &Path) -> Result<Vec<String>> {
    let stdout = command::run_in(
        &path.to_string_lossy(),
        "git",
        &["ls-files", "--cached", "--others", "--exclude-standard", "*.md"],
        "git ls-files",
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    Ok(stdout.lines().filter(|l| !l.is_empty()).map(String::from).collect())
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

/// Extract version number from a git tag.
/// Handles formats: v1.0.0, 1.0.0, component-v1.0.0
fn extract_version_from_tag(tag: &str) -> Option<String> {
    let version_pattern = Regex::new(r"v?(\d+\.\d+(?:\.\d+)?)").ok()?;
    version_pattern
        .captures(tag)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Get the latest git tag in the repository.
/// Returns None if no tags exist.
pub fn get_latest_tag(path: &str) -> Result<Option<String>> {
    Ok(command::run_in_optional(path, "git", &["describe", "--tags", "--abbrev=0"]))
}

const DEFAULT_COMMIT_LIMIT: usize = 10;
const VERBOSE_UNTRACKED_THRESHOLD: usize = 200;
const NOISY_UNTRACKED_DIRS: [&str; 8] = [
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    "vendor",
    "target",
    ".cache",
];

/// Find the most recent commit containing a version number in its message.
/// Matches strict patterns: v1.0.0, bump to X, release X, version X
/// Returns the commit hash if found, None otherwise.
pub fn find_version_commit(path: &str) -> Result<Option<String>> {
    let stdout = command::run_in(path, "git", &["log", "-200", "--format=%h|%s"], "git log")?;

    let version_pattern = Regex::new(
        r"(?i)(?:^v|^version\s+(?:bump\s+(?:to\s+)?)?v?|^bump\s+(?:version\s+)?(?:to\s+)?v?|^(?:chore\([^)]*\):\s*)?release:?\s*v?)(\d+\.\d+(?:\.\d+)?)",
    )
    .expect("Invalid regex pattern");

    for line in stdout.lines() {
        if let Some((hash, subject)) = line.split_once('|') {
            if version_pattern.is_match(subject) {
                return Ok(Some(hash.to_string()));
            }
        }
    }

    Ok(None)
}

/// Find the commit that released a specific version.
/// Uses strict patterns to avoid false positives - only matches commits that
/// clearly mark a release (e.g., "release: v0.2.0"), not mentions of releases.
pub fn find_version_release_commit(path: &str, version: &str) -> Result<Option<String>> {
    let Some(stdout) = command::run_in_optional(path, "git", &["log", "-200", "--format=%h|%s"])
    else {
        return Ok(None);
    };

    let escaped_version = regex::escape(version);
    let patterns = [
        format!(r"(?i)^(?:chore\([^)]*\):\s*)?release:?\s*v?{}(?:\s|$)", escaped_version),
        format!(r"(?i)^v?{}\s*$", escaped_version),
        format!(r"(?i)^bump\s+(?:version\s+)?(?:to\s+)?v?{}(?:\s|$)", escaped_version),
        // Match "Version X.Y.Z" or "Version bump to X.Y.Z"
        format!(r"(?i)^version\s+(?:bump\s+(?:to\s+)?)?v?{}(?:\s|:|-|$)", escaped_version),
    ];

    for line in stdout.lines() {
        if let Some((hash, subject)) = line.split_once('|') {
            for pattern in &patterns {
                if Regex::new(pattern).map(|re| re.is_match(subject)).unwrap_or(false) {
                    return Ok(Some(hash.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// Get the last N commits from the repository.
pub fn get_last_n_commits(path: &str, n: usize) -> Result<Vec<CommitInfo>> {
    let stdout = command::run_in(path, "git", &["log", &format!("-{}", n), "--format=%h|%s"], "git log")?;

    let commits = stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('|')?;
            Some(CommitInfo {
                hash: hash.to_string(),
                subject: subject.to_string(),
                category: parse_conventional_commit(subject),
            })
        })
        .collect();

    Ok(commits)
}

/// Get commits since a given tag (or all commits if tag is None).
/// Returns commits in reverse chronological order (newest first).
pub fn get_commits_since_tag(path: &str, tag: Option<&str>) -> Result<Vec<CommitInfo>> {
    let range = tag.map(|t| format!("{}..HEAD", t)).unwrap_or_else(|| "HEAD".to_string());
    let stdout = command::run_in(path, "git", &["log", &range, "--format=%h|%s"], "git log")?;

    let commits = stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('|')?;
            Some(CommitInfo {
                hash: hash.to_string(),
                subject: subject.to_string(),
                category: parse_conventional_commit(subject),
            })
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
pub struct RepoSnapshot {
    pub branch: String,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
}

impl GitOutput {
    fn from_output(id: String, path: String, action: &str, output: std::process::Output) -> Self {
        Self {
            component_id: id,
            path,
            action: action.to_string(),
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }
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

pub struct UncommittedChanges {
    pub has_changes: bool,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]

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
    pub uncommitted_diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct BaselineInfo {
    pub latest_tag: Option<String>,
    pub source: Option<BaselineSource>,
    pub reference: Option<String>,
    pub warning: Option<String>,
}

/// Detect baseline for a path (public wrapper).
/// For version-aware baseline detection, use detect_baseline_with_version().
pub fn detect_baseline_for_path(path: &str) -> Result<BaselineInfo> {
    detect_baseline_with_version(path, None)
}

/// Detect baseline with version alignment checking.
/// If a tag exists but doesn't match current_version, warns and finds version commit instead.
pub fn detect_baseline_with_version(
    path: &str,
    current_version: Option<&str>,
) -> Result<BaselineInfo> {
    // Priority 1: Check for latest tag
    if let Some(tag) = get_latest_tag(path)? {
        let tag_version = extract_version_from_tag(&tag);

        // If we have current version, check alignment
        if let (Some(current), Some(tag_ver)) = (current_version, &tag_version) {
            if current != tag_ver {
                // Tag is stale - try to find the release commit for current version
                if let Some(hash) = find_version_release_commit(path, current)? {
                    return Ok(BaselineInfo {
                        latest_tag: Some(tag.clone()),
                        source: Some(BaselineSource::VersionCommit),
                        reference: Some(hash),
                        warning: Some(format!(
                            "Latest tag '{}' doesn't match version {}. Using release commit as baseline. Consider: git tag v{}",
                            tag, current, current
                        )),
                    });
                }

                // No matching release commit - fall back to generic version commit
                if let Some(hash) = find_version_commit(path)? {
                    return Ok(BaselineInfo {
                        latest_tag: Some(tag.clone()),
                        source: Some(BaselineSource::VersionCommit),
                        reference: Some(hash),
                        warning: Some(format!(
                            "Latest tag '{}' doesn't match version {}. Using most recent version commit.",
                            tag, current
                        )),
                    });
                }

                // No version commits found - use the stale tag but warn
                return Ok(BaselineInfo {
                    latest_tag: Some(tag.clone()),
                    source: Some(BaselineSource::Tag),
                    reference: Some(tag.clone()),
                    warning: Some(format!(
                        "Latest tag '{}' doesn't match version {}. Consider: git tag v{}",
                        tag, current, current
                    )),
                });
            }
        }

        // Tag version matches or no version to compare - use tag
        return Ok(BaselineInfo {
            latest_tag: Some(tag.clone()),
            source: Some(BaselineSource::Tag),
            reference: Some(tag),
            warning: None,
        });
    }

    // Priority 2: No tags - try version commit for current version first
    if let Some(current) = current_version {
        if let Some(hash) = find_version_release_commit(path, current)? {
            return Ok(BaselineInfo {
                latest_tag: None,
                source: Some(BaselineSource::VersionCommit),
                reference: Some(hash),
                warning: Some("No tags found. Using release commit for current version.".to_string()),
            });
        }
    }

    // Priority 3: Generic version commit
    if let Some(hash) = find_version_commit(path)? {
        return Ok(BaselineInfo {
            latest_tag: None,
            source: Some(BaselineSource::VersionCommit),
            reference: Some(hash),
            warning: Some(
                "No tags found. Using most recent version commit as baseline.".to_string(),
            ),
        });
    }

    // Fallback: No baseline found
    Ok(BaselineInfo {
        latest_tag: None,
        source: Some(BaselineSource::LastNCommits),
        reference: None,
        warning: Some(format!(
            "No tags or version commits found. Showing last {} commits.",
            DEFAULT_COMMIT_LIMIT
        )),
    })
}


// Input types for JSON parsing
#[derive(Debug, Deserialize)]

struct BulkIdsInput {
    component_ids: Vec<String>,
    #[serde(default)]
    tags: bool,
}

#[derive(Debug, Deserialize)]

struct BulkCommitInput {
    components: Vec<CommitSpec>,
}

#[derive(Debug, Deserialize)]

struct CommitSpec {
    #[serde(default)]
    id: Option<String>,
    message: String,
    #[serde(default)]
    staged_only: bool,
    #[serde(default, alias = "include_files")]
    files: Option<Vec<String>>,
    #[serde(default, alias = "exclude_files")]
    exclude_files: Option<Vec<String>>,
}

/// Options for commit operations.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Skip `git add` and commit only staged changes
    pub staged_only: bool,
    /// Stage and commit only these specific files
    pub files: Option<Vec<String>>,
    /// Stage all except these files (mutually exclusive with `files`)
    pub exclude: Option<Vec<String>>,
    /// Amend the previous commit instead of creating a new one
    pub amend: bool,
}

fn get_component_path(component_id: &str) -> Result<String> {
    let comp = component::load(component_id)?;
    Ok(comp.local_path)
}

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").args(args).current_dir(path).output()
}

pub fn execute_git_for_release(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    execute_git(path, args)
}

pub fn get_repo_snapshot(path: &str) -> Result<RepoSnapshot> {
    if !is_git_repo(path) {
        return Err(Error::git_command_failed("Not a git repository"));
    }

    let branch = command::run_in(path, "git", &["rev-parse", "--abbrev-ref", "HEAD"], "git branch")?;
    let clean = command::run_in_optional(path, "git", &["status", "--porcelain=v1"])
        .map(|s| s.is_empty())
        .unwrap_or(false);

    let (ahead, behind) = command::run_in_optional(path, "git", &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .and_then(|_| {
            command::run_in_optional(path, "git", &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        })
        .map(|counts| parse_ahead_behind(&counts))
        .unwrap_or((None, None));

    Ok(RepoSnapshot {
        branch,
        clean,
        ahead,
        behind,
    })
}

fn parse_ahead_behind(counts: &str) -> (Option<u32>, Option<u32>) {
    let trimmed = counts.trim();
    let mut parts = trimmed.split_whitespace();
    let ahead = parts.next().and_then(|v| v.parse::<u32>().ok());
    let behind = parts.next().and_then(|v| v.parse::<u32>().ok());
    (ahead, behind)
}

fn build_untracked_hint(path: &str, untracked_count: usize) -> Option<String> {
    if untracked_count < VERBOSE_UNTRACKED_THRESHOLD {
        return None;
    }

    let ignored_output = execute_git(path, &["status", "--ignored", "--porcelain=v1"]).ok()?;
    if !ignored_output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&ignored_output.stdout);
    let ignored_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("!!"))
        .collect();
    if ignored_lines.is_empty() {
        return None;
    }

    let mut noisy_ignored = Vec::new();
    for line in ignored_lines {
        let path = line[3..].trim();
        for dir in NOISY_UNTRACKED_DIRS {
            if path == dir || path.starts_with(&format!("{}/", dir)) {
                noisy_ignored.push(dir.to_string());
                break;
            }
        }
    }

    noisy_ignored.sort();
    noisy_ignored.dedup();

    if noisy_ignored.is_empty() {
        return None;
    }

    Some(format!(
        "Large untracked list detected ({}). Common noisy directories ignored by git: {}. If output feels too big, add them to .gitignore.",
        untracked_count,
        noisy_ignored.join(", ")
    ))
}

fn resolve_target(component_id: Option<&str>) -> Result<(String, String)> {
    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId",
            None,
            Some(vec![
                "Provide a component ID: homeboy git <command> <component-id>".to_string(),
                "List available components: homeboy component list".to_string(),
            ]),
        )
    })?;
    Ok((id.to_string(), get_component_path(id)?))
}

/// Get git status for a component.
pub fn status(component_id: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id)?;
    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "status", output))
}

fn run_bulk_ids<F>(ids: &[String], action: &str, op: F) -> BulkResult<GitOutput>
where
    F: Fn(&str) -> Result<GitOutput>,
{
    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in ids {
        match op(id) {
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
        action: action.to_string(),
        results,
        summary: BulkSummary {
            total: succeeded + failed,
            succeeded,
            failed,
        },
    }
}

/// Get git status for multiple components from JSON spec.
pub fn status_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk status input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    Ok(run_bulk_ids(&input.component_ids, "status", |id| {
        status(Some(id))
    }))
}

/// Commit changes for a component.
///
/// By default, stages all changes before committing. Use `options` to control staging:
/// - `staged_only`: Skip staging, commit only what's already staged
/// - `files`: Stage only these specific files before committing
pub fn commit(
    component_id: Option<&str>,
    message: Option<&str>,
    options: CommitOptions,
) -> Result<GitOutput> {
    let msg = message.ok_or_else(|| {
        Error::validation_invalid_argument("message", "Missing commit message", None, None)
    })?;
    let (id, path) = resolve_target(component_id)?;

    // Check for changes - behavior differs based on staged_only
    let status_output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::other(e.to_string()))?;
    let status_str = String::from_utf8_lossy(&status_output.stdout);

    if options.staged_only {
        // Check if there are staged changes (lines starting with non-space in first column)
        let has_staged = status_str.lines().any(|line| {
            let first_char = line.chars().next().unwrap_or(' ');
            first_char != ' ' && first_char != '?'
        });
        if !has_staged {
            return Ok(GitOutput {
                component_id: id,
                path,
                action: "commit".to_string(),
                success: true,
                exit_code: 0,
                stdout: "Nothing staged to commit".to_string(),
                stderr: String::new(),
            });
        }
    } else if status_str.trim().is_empty() {
        return Ok(GitOutput {
            component_id: id,
            path,
            action: "commit".to_string(),
            success: true,
            exit_code: 0,
            stdout: "Nothing to commit, working tree clean".to_string(),
            stderr: String::new(),
        });
    }

    // Stage changes based on options
    if !options.staged_only {
        match (&options.files, &options.exclude) {
            // Both specified: error (mutually exclusive)
            (Some(_), Some(_)) => {
                return Err(Error::validation_invalid_argument(
                    "files/exclude",
                    "Cannot use both --files and --exclude",
                    None,
                    None,
                ));
            }
            // Include only specific files
            (Some(files), None) => {
                let mut args = vec!["add", "--"];
                args.extend(files.iter().map(|s| s.as_str()));
                let add_output =
                    execute_git(&path, &args).map_err(|e| Error::other(e.to_string()))?;
                if !add_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", add_output));
                }
            }
            // Exclude specific files: stage all, then unstage excluded
            (None, Some(excluded)) => {
                let add_output =
                    execute_git(&path, &["add", "."]).map_err(|e| Error::other(e.to_string()))?;
                if !add_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", add_output));
                }
                // Unstage excluded files (git reset -- file1 file2)
                // Note: git reset without --hard only unstages, does not discard changes
                let mut reset_args = vec!["reset", "--"];
                reset_args.extend(excluded.iter().map(|s| s.as_str()));
                let reset_output =
                    execute_git(&path, &reset_args).map_err(|e| Error::other(e.to_string()))?;
                if !reset_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", reset_output));
                }
            }
            // Default: stage all
            (None, None) => {
                let add_output =
                    execute_git(&path, &["add", "."]).map_err(|e| Error::other(e.to_string()))?;
                if !add_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", add_output));
                }
            }
        }
    }

    let args: Vec<&str> = if options.amend {
        vec!["commit", "--amend", "-m", msg]
    } else {
        vec!["commit", "-m", msg]
    };
    let output = execute_git(&path, &args).map_err(|e| Error::other(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "commit", output))
}

/// Commit multiple components from JSON spec (bulk format).
fn commit_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let input: BulkCommitInput = serde_json::from_str(json_spec).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk commit input".to_string()),
            Some(json_spec.chars().take(200).collect::<String>()),
        )
    })?;

    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for spec in &input.components {
        let id = match &spec.id {
            Some(id) => id.clone(),
            None => {
                failed += 1;
                results.push(ItemOutcome {
                    id: "unknown".to_string(),
                    result: None,
                    error: Some("Missing 'id' field in bulk commit spec".to_string()),
                });
                continue;
            }
        };
        let options = CommitOptions {
            staged_only: spec.staged_only,
            files: spec.files.clone(),
            exclude: spec.exclude_files.clone(),
            amend: false,
        };
        match commit(Some(&id), Some(&spec.message), options) {
            Ok(output) => {
                if output.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                results.push(ItemOutcome {
                    id,
                    result: Some(output),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(ItemOutcome {
                    id,
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

/// Output from commit_from_json - either single or bulk result.
#[derive(Serialize)]
#[serde(untagged)]
pub enum CommitJsonOutput {
    Single(GitOutput),
    Bulk(BulkResult<GitOutput>),
}

/// Commit from JSON spec. Auto-detects single vs bulk format.
///
/// Single format: `{"id":"x","message":"m"}` or `{"message":"m"}` (uses CWD/positional ID)
/// Bulk format: `{"components":[{"id":"x","message":"m"},...]}`
pub fn commit_from_json(id: Option<&str>, json_spec: &str) -> Result<CommitJsonOutput> {
    let raw = read_json_spec_to_string(json_spec)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse commit json".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;

    // Auto-detect: bulk if has "components" array
    if parsed.get("components").is_some() {
        let bulk = commit_bulk(&raw)?;
        return Ok(CommitJsonOutput::Bulk(bulk));
    }

    // Single spec - parse and extract fields
    let spec: CommitSpec = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse commit spec".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;

    // ID priority: positional arg > JSON body
    let target_id = id.map(|s| s.to_string()).or(spec.id);
    let options = CommitOptions {
        staged_only: spec.staged_only,
        files: spec.files,
        exclude: spec.exclude_files,
        amend: false,
    };

    let output = commit(target_id.as_deref(), Some(&spec.message), options)?;
    Ok(CommitJsonOutput::Single(output))
}

/// Push local commits for a component.
pub fn push(component_id: Option<&str>, tags: bool) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id)?;
    let args: Vec<&str> = if tags {
        vec!["push", "--follow-tags"]
    } else {
        vec!["push"]
    };
    let output = execute_git(&path, &args).map_err(|e| Error::other(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "push", output))
}

/// Push multiple components from JSON spec.
pub fn push_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk push input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    let push_tags = input.tags;
    Ok(run_bulk_ids(&input.component_ids, "push", |id| {
        push(Some(id), push_tags)
    }))
}

/// Pull remote changes for a component.
pub fn pull(component_id: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id)?;
    let output = execute_git(&path, &["pull"]).map_err(|e| Error::other(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "pull", output))
}

/// Pull multiple components from JSON spec.
pub fn pull_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk pull input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    Ok(run_bulk_ids(&input.component_ids, "pull", |id| {
        pull(Some(id))
    }))
}

/// Create a git tag for a component.
pub fn tag(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
) -> Result<GitOutput> {
    let name = tag_name.ok_or_else(|| {
        Error::validation_invalid_argument("tagName", "Missing tag name", None, None)
    })?;
    let (id, path) = resolve_target(component_id)?;
    let args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", name, "-m", msg],
        None => vec!["tag", name],
    };
    let output = execute_git(&path, &args).map_err(|e| Error::other(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "tag", output))
}

// === Changes Operations ===

/// Parse git status output into structured uncommitted changes.
pub fn get_uncommitted_changes(path: &str) -> Result<UncommittedChanges> {
    let output = execute_git(
        path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
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
    let hint = build_untracked_hint(path, untracked.len());

    Ok(UncommittedChanges {
        has_changes,
        staged,
        unstaged,
        untracked,
        hint,
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
            result.push('\n');
        }
        result.push_str("=== Unstaged Changes ===\n");
        result.push_str(&unstaged_diff);
    }

    Ok(result)
}

/// Get diff between baseline ref and HEAD (commit range diff).
pub fn get_range_diff(path: &str, baseline_ref: &str) -> Result<String> {
    let output = execute_git(
        path,
        &["diff", &format!("{}..HEAD", baseline_ref), "--", "."],
    )
    .map_err(|e| Error::other(e.to_string()))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get all changes for a component.
pub fn changes(
    component_id: Option<&str>,
    since_tag: Option<&str>,
    include_diff: bool,
) -> Result<ChangesOutput> {
    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId",
            None,
            Some(vec![
                "Provide a component ID: homeboy changes <component-id>".to_string(),
                "List available components: homeboy component list".to_string(),
            ]),
        )
    })?;
    let path = get_component_path(id)?;

    // Determine baseline with version alignment awareness
    let baseline = match since_tag {
        Some(t) => {
            // Explicit tag override - use as-is
            BaselineInfo {
                latest_tag: Some(t.to_string()),
                source: Some(BaselineSource::Tag),
                reference: Some(t.to_string()),
                warning: None,
            }
        }
        None => {
            // Load component version for alignment checking
            let current_version = crate::component::load(id)
                .ok()
                .and_then(|c| crate::version::get_component_version(&c));
            detect_baseline_with_version(&path, current_version.as_deref())?
        }
    };

    let commits = match baseline.source {
        Some(BaselineSource::LastNCommits) => get_last_n_commits(&path, DEFAULT_COMMIT_LIMIT)?,
        _ => get_commits_since_tag(&path, baseline.reference.as_deref())?,
    };

    let uncommitted = get_uncommitted_changes(&path)?;
    let uncommitted_diff = if uncommitted.has_changes {
        Some(get_diff(&path)?)
    } else {
        None
    };
    let diff = if include_diff {
        baseline
            .reference
            .as_ref()
            .map(|r| get_range_diff(&path, r))
            .transpose()?
    } else {
        None
    };

    Ok(ChangesOutput {
        component_id: id.to_string(),
        path,
        success: true,
        latest_tag: baseline.latest_tag,
        baseline_source: baseline.source,
        baseline_ref: baseline.reference,
        commits,
        uncommitted,
        uncommitted_diff,
        diff,
        warning: baseline.warning,
        error: None,
    })
}

fn build_bulk_changes_output(
    component_ids: &[String],
    include_diff: bool,
) -> BulkResult<ChangesOutput> {
    let mut results = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for id in component_ids {
        match changes(Some(id), None, include_diff) {
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
        Error::validation_invalid_json(
            e,
            Some("parse bulk changes input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
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

fn is_git_repo(path: &str) -> bool {
    command::succeeded_in(path, "git", &["rev-parse", "--git-dir"])
}

/// Check if a tag exists on the remote.
pub fn tag_exists_on_remote(path: &str, tag_name: &str) -> Result<bool> {
    Ok(command::run_in_optional(
        path,
        "git",
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{}", tag_name)],
    )
    .map(|s| !s.is_empty())
    .unwrap_or(false))
}

/// Check if a tag exists locally.
pub fn tag_exists_locally(path: &str, tag_name: &str) -> Result<bool> {
    Ok(command::run_in_optional(path, "git", &["tag", "-l", tag_name])
        .map(|s| !s.is_empty())
        .unwrap_or(false))
}

/// Get the commit SHA a tag points to.
pub fn get_tag_commit(path: &str, tag_name: &str) -> Result<String> {
    command::run_in(path, "git", &["rev-list", "-n", "1", tag_name], &format!("get commit for tag '{}'", tag_name))
}

/// Get the current HEAD commit SHA.
pub fn get_head_commit(path: &str) -> Result<String> {
    command::run_in(path, "git", &["rev-parse", "HEAD"], "get HEAD commit")
}

/// Stage specific files in a git repository.
pub fn stage_files(path: &str, files: &[&str]) -> Result<()> {
    let mut args = vec!["add", "--"];
    args.extend(files);
    command::run_in(path, "git", &args, "stage files")?;
    Ok(())
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
