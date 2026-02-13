use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::changelog;
use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::project;

use super::commits::*;
use super::changes::*;
use super::primitives::is_git_repo;
use super::{execute_git, resolve_target};

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
pub struct ChangelogInfo {
    pub unreleased_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<ChangelogInfo>,
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

pub fn execute_git_for_release(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    execute_git(path, args)
}

pub fn get_repo_snapshot(path: &str) -> Result<RepoSnapshot> {
    if !is_git_repo(path) {
        return Err(Error::git_command_failed("Not a git repository"));
    }

    let branch = crate::utils::command::run_in(path, "git", &["rev-parse", "--abbrev-ref", "HEAD"], "git branch")?;

    // Use direct Command to properly handle empty output (clean repo).
    // run_in_optional returns None for empty stdout, which would incorrectly
    // indicate a dirty repo when used with .unwrap_or(false).
    let clean = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false);

    let (ahead, behind) = crate::utils::command::run_in_optional(path, "git", &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .and_then(|_| {
            crate::utils::command::run_in_optional(path, "git", &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
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

pub(crate) fn build_untracked_hint(path: &str, untracked_count: usize) -> Option<String> {
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

fn resolve_changelog_info(
    component: &crate::component::Component,
    commits: &[CommitInfo],
) -> Option<ChangelogInfo> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let content = std::fs::read_to_string(&changelog_path).ok()?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let unreleased_entries =
        changelog::count_unreleased_entries(&content, &settings.next_section_aliases);

    let hint = if unreleased_entries == 0 && !commits.is_empty() {
        Some(format!(
            "Run `homeboy changelog add {}` before bumping version",
            component.id
        ))
    } else {
        None
    };

    Some(ChangelogInfo {
        unreleased_entries,
        path: Some(changelog_path.to_string_lossy().to_string()),
        hint,
    })
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

/// Check if a tag exists on the remote.
pub fn tag_exists_on_remote(path: &str, tag_name: &str) -> Result<bool> {
    Ok(crate::utils::command::run_in_optional(
        path,
        "git",
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{}", tag_name)],
    )
    .map(|s| !s.is_empty())
    .unwrap_or(false))
}

/// Check if a tag exists locally.
pub fn tag_exists_locally(path: &str, tag_name: &str) -> Result<bool> {
    Ok(crate::utils::command::run_in_optional(path, "git", &["tag", "-l", tag_name])
        .map(|s| !s.is_empty())
        .unwrap_or(false))
}

/// Get the commit SHA a tag points to.
pub fn get_tag_commit(path: &str, tag_name: &str) -> Result<String> {
    crate::utils::command::run_in(path, "git", &["rev-list", "-n", "1", tag_name], &format!("get commit for tag '{}'", tag_name))
}

/// Get the current HEAD commit SHA.
pub fn get_head_commit(path: &str) -> Result<String> {
    crate::utils::command::run_in(path, "git", &["rev-parse", "HEAD"], "get HEAD commit")
}

/// Stage specific files in a git repository.
pub fn stage_files(path: &str, files: &[&str]) -> Result<()> {
    let mut args = vec!["add", "--"];
    args.extend(files);
    crate::utils::command::run_in(path, "git", &args, "stage files")?;
    Ok(())
}

/// Fetch from remote and return count of commits behind upstream.
/// Returns Ok(Some(n)) if behind by n commits, Ok(None) if not behind or no upstream.
pub fn fetch_and_get_behind_count(path: &str) -> Result<Option<u32>> {
    // Run git fetch (update tracking refs)
    crate::utils::command::run_in(path, "git", &["fetch"], "git fetch")?;

    // Check if upstream exists
    let upstream = crate::utils::command::run_in_optional(path, "git", &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    if upstream.is_none() {
        return Ok(None); // No upstream configured
    }

    // Get ahead/behind counts
    let counts = crate::utils::command::run_in_optional(
        path,
        "git",
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    );

    match counts {
        Some(output) => {
            let (_, behind) = parse_ahead_behind(&output);
            Ok(behind.filter(|&n| n > 0))
        }
        None => Ok(None),
    }
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

    // Load component for version checking and changelog info
    let component = crate::component::load(id).ok();

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
            // Use component version for alignment checking
            let current_version = component
                .as_ref()
                .and_then(|c| crate::version::get_component_version(c));
            detect_baseline_with_version(&path, current_version.as_deref())?
        }
    };

    let commits = match baseline.source {
        Some(BaselineSource::LastNCommits) => get_last_n_commits(&path, DEFAULT_COMMIT_LIMIT)?,
        _ => get_commits_since_tag(&path, baseline.reference.as_deref())?,
    };

    // Resolve changelog info if component has changelog configured
    let changelog_info = component
        .as_ref()
        .and_then(|c| resolve_changelog_info(c, &commits));

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
        changelog: changelog_info,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_workdir_clean_returns_true_for_clean_repo() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let path = temp_dir.path();

        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");

        // Configure git user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git email");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git name");

        // Create a file and commit it
        fs::write(path.join("test.txt"), "content").expect("Failed to write file");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("Failed to git add");

        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("Failed to commit");

        // Now the repo should be clean
        assert!(
            super::super::is_workdir_clean(path),
            "Expected clean repo to return true"
        );
    }

    #[test]
    fn is_workdir_clean_returns_false_for_dirty_repo() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let path = temp_dir.path();

        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");

        // Create an untracked file
        fs::write(path.join("untracked.txt"), "content").expect("Failed to write file");

        // Repo should be dirty (untracked file)
        assert!(
            !super::super::is_workdir_clean(path),
            "Expected dirty repo to return false"
        );
    }

    #[test]
    fn is_workdir_clean_returns_false_for_invalid_path() {
        let path = std::path::Path::new("/nonexistent/path/that/does/not/exist");
        assert!(
            !super::super::is_workdir_clean(path),
            "Expected invalid path to return false"
        );
    }
}
