use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::project;
use crate::release::changelog;

use super::changes::*;
use super::commits::*;
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

#[derive(Debug, Clone, Serialize)]
pub struct RepoBaselineSnapshot {
    pub branch: String,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_since_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
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

pub fn build_repo_baseline_snapshot(
    path: &str,
    current_version: Option<&str>,
) -> Result<RepoBaselineSnapshot> {
    let snapshot = get_repo_snapshot(path)?;
    let baseline = detect_baseline_with_version(path, current_version).ok();
    let commits_since = baseline.as_ref().and_then(|b| {
        get_commits_since_tag(path, b.reference.as_deref())
            .ok()
            .map(|c| c.len() as u32)
    });

    Ok(RepoBaselineSnapshot {
        branch: snapshot.branch,
        clean: snapshot.clean,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        commits_since_version: commits_since,
        baseline_ref: baseline.as_ref().and_then(|b| b.reference.clone()),
        baseline_warning: baseline.and_then(|b| b.warning),
    })
}

/// Detect baseline with version alignment checking.
/// If a tag exists but doesn't match current_version, warns and finds version commit instead.
pub fn detect_baseline_with_version(
    path: &str,
    current_version: Option<&str>,
) -> Result<BaselineInfo> {
    // Fetch tags from remote so locally-missing tags (pushed from another
    // machine) are available before we resolve the baseline.  Best-effort:
    // if there is no remote or the network is unavailable we silently
    // proceed with whatever tags are already local.
    let _ = crate::engine::command::run_in_optional(path, "git", &["fetch", "--tags", "--quiet"]);

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
                warning: Some(
                    "No tags found. Using release commit for current version.".to_string(),
                ),
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
    #[serde(default)]
    force_with_lease: bool,
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
    let comp = component::resolve_effective(Some(component_id), None, None)?;
    Ok(comp.local_path)
}

pub fn execute_git_for_release(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    execute_git(path, args)
}

pub fn get_repo_snapshot(path: &str) -> Result<RepoSnapshot> {
    if !is_git_repo(path) {
        return Err(Error::git_command_failed("Not a git repository"));
    }

    let branch = crate::engine::command::run_in(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "git branch",
    )?;

    // Use direct Command to properly handle empty output (clean repo).
    // run_in_optional returns None for empty stdout, which would incorrectly
    // indicate a dirty repo when used with .unwrap_or(false).
    let clean = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false);

    let (ahead, behind) = crate::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
    )
    .and_then(|_| {
        crate::engine::command::run_in_optional(
            path,
            "git",
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )
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
    // git rev-list --left-right --count @{upstream}...HEAD outputs:
    //   <upstream_only>\t<local_only>
    // upstream_only = commits on remote not in local (behind)
    // local_only = commits in local not on remote (ahead)
    let trimmed = counts.trim();
    let mut parts = trimmed.split_whitespace();
    let behind = parts.next().and_then(|v| v.parse::<u32>().ok());
    let ahead = parts.next().and_then(|v| v.parse::<u32>().ok());
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
    _commits: &[CommitInfo],
) -> Option<ChangelogInfo> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let content = std::fs::read_to_string(&changelog_path).ok()?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let unreleased_entries =
        changelog::count_unreleased_entries(&content, &settings.next_section_aliases);

    // No hint: homeboy auto-generates changelog entries from commits at
    // release time, so an empty `## Unreleased` section no longer implies
    // the user needs to do anything. The count itself is still useful
    // context for `homeboy changes` output.
    Some(ChangelogInfo {
        unreleased_entries,
        path: Some(changelog_path.to_string_lossy().to_string()),
        hint: None,
    })
}

/// Get git status for a component.
pub fn status(component_id: Option<&str>) -> Result<GitOutput> {
    status_at(component_id, None)
}

/// Like [`status`] but with an explicit path override for git operations.
pub fn status_at(component_id: Option<&str>, path_override: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
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
///
/// When `path_override` is provided, git operations run in that directory
/// instead of the component's configured `local_path`.
pub fn commit(
    component_id: Option<&str>,
    message: Option<&str>,
    options: CommitOptions,
) -> Result<GitOutput> {
    commit_at(component_id, message, options, None)
}

/// Like [`commit`] but with an explicit path override for git operations.
pub fn commit_at(
    component_id: Option<&str>,
    message: Option<&str>,
    options: CommitOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let msg = message.ok_or_else(|| {
        Error::validation_invalid_argument("message", "Missing commit message", None, None)
    })?;
    let (id, path) = resolve_target(component_id, path_override)?;

    // Check for changes - behavior differs based on staged_only
    let status_output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
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
                let add_output = execute_git(&path, &args)
                    .map_err(|e| Error::git_command_failed(e.to_string()))?;
                if !add_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", add_output));
                }
            }
            // Exclude specific files: stage all, then unstage excluded
            (None, Some(excluded)) => {
                let add_output = execute_git(&path, &["add", "."])
                    .map_err(|e| Error::git_command_failed(e.to_string()))?;
                if !add_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", add_output));
                }
                // Unstage excluded files (git reset -- file1 file2)
                // Note: git reset without --hard only unstages, does not discard changes
                let mut reset_args = vec!["reset", "--"];
                reset_args.extend(excluded.iter().map(|s| s.as_str()));
                let reset_output = execute_git(&path, &reset_args)
                    .map_err(|e| Error::git_command_failed(e.to_string()))?;
                if !reset_output.status.success() {
                    return Ok(GitOutput::from_output(id, path, "commit", reset_output));
                }
            }
            // Default: stage all
            (None, None) => {
                let add_output = execute_git(&path, &["add", "."])
                    .map_err(|e| Error::git_command_failed(e.to_string()))?;
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
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
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

/// Options for [`push`].
#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    /// Push tags as well (`--follow-tags`).
    pub tags: bool,
    /// Use `--force-with-lease` for safe force-pushes (e.g. after a rebase).
    /// Deliberately the only force flavour exposed — never plain `--force`.
    pub force_with_lease: bool,
}

/// Push local commits for a component.
pub fn push(component_id: Option<&str>, options: PushOptions) -> Result<GitOutput> {
    push_at(component_id, options, None)
}

/// Like [`push`] but with an explicit path override for git operations.
pub fn push_at(
    component_id: Option<&str>,
    options: PushOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let mut args: Vec<&str> = vec!["push"];
    if options.tags {
        args.push("--follow-tags");
    }
    if options.force_with_lease {
        args.push("--force-with-lease");
    }
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
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
    let force_with_lease = input.force_with_lease;
    Ok(run_bulk_ids(&input.component_ids, "push", |id| {
        push(
            Some(id),
            PushOptions {
                tags: push_tags,
                force_with_lease,
            },
        )
    }))
}

/// Pull remote changes for a component.
pub fn pull(component_id: Option<&str>) -> Result<GitOutput> {
    pull_at(component_id, None)
}

/// Like [`pull`] but with an explicit path override for git operations.
pub fn pull_at(component_id: Option<&str>, path_override: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let output =
        execute_git(&path, &["pull"]).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "pull", output))
}

/// Options for [`rebase`].
#[derive(Debug, Clone, Default)]
pub struct RebaseOptions {
    /// Upstream / target ref to rebase onto. `None` defaults to the
    /// current branch's tracked upstream (`@{upstream}`), matching
    /// `git pull --rebase` semantics.
    pub onto: Option<String>,
    /// `git rebase --continue` after manual conflict resolution. Mutually
    /// exclusive with `abort` at the CLI layer.
    pub continue_: bool,
    /// `git rebase --abort` to bail out of an in-progress rebase.
    pub abort: bool,
}

/// Rebase the current branch onto another ref.
///
/// Default behaviour (no `onto`) is `git rebase @{upstream}`, which drops
/// commits whose patch-id matches a commit already in upstream — the
/// standard rebase merged-commit dedup. Squash-merged PRs (different
/// patch-id) are NOT dropped by default; that case will land in a
/// follow-up via `gh`-aware PR drop.
///
/// On conflict, the operation returns a `GitOutput { success: false }`
/// with stderr from git. The caller resolves with raw `git`, then runs
/// `homeboy git rebase --continue` or `--abort`. No state-machine
/// orchestration in MVP.
pub fn rebase(component_id: Option<&str>, options: RebaseOptions) -> Result<GitOutput> {
    rebase_at(component_id, options, None)
}

/// Like [`rebase`] but with an explicit path override.
pub fn rebase_at(
    component_id: Option<&str>,
    options: RebaseOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    let args: Vec<String> = if options.abort {
        vec!["rebase".into(), "--abort".into()]
    } else if options.continue_ {
        vec!["rebase".into(), "--continue".into()]
    } else {
        let mut a = vec!["rebase".into()];
        if let Some(onto) = options.onto.as_deref() {
            a.push(onto.to_string());
        }
        // No `onto` arg → bare `git rebase` rebases onto @{upstream}.
        a
    };
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "rebase", output))
}

/// Options for [`cherry_pick`].
#[derive(Debug, Clone, Default)]
pub struct CherryPickOptions {
    /// Commit refs to cherry-pick. Accepts SHAs, branches, ranges
    /// (`<sha1>..<sha2>`). Empty when `continue_` or `abort` is set.
    pub refs: Vec<String>,
    /// Cherry-pick all commits from a GitHub PR (one or more). Resolved
    /// via `gh pr view <n> --json commits`. Each PR's commits are picked
    /// in oldest-to-newest order. Combinable with `refs`; PR commits are
    /// expanded first and then concatenated with explicit refs.
    pub prs: Vec<u64>,
    /// `git cherry-pick --continue` after manual conflict resolution.
    pub continue_: bool,
    /// `git cherry-pick --abort` to bail out of an in-progress pick.
    pub abort: bool,
}

/// Cherry-pick one or more commits onto the current branch.
///
/// On conflict, returns `GitOutput { success: false }` with git's stderr.
/// Resolve manually, then run with `--continue` or `--abort`.
pub fn cherry_pick(component_id: Option<&str>, options: CherryPickOptions) -> Result<GitOutput> {
    cherry_pick_at(component_id, options, None)
}

/// Like [`cherry_pick`] but with an explicit path override.
pub fn cherry_pick_at(
    component_id: Option<&str>,
    options: CherryPickOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    if options.abort {
        let output = execute_git(&path, &["cherry-pick", "--abort"])
            .map_err(|e| Error::git_command_failed(e.to_string()))?;
        return Ok(GitOutput::from_output(id, path, "cherry-pick", output));
    }
    if options.continue_ {
        let output = execute_git(&path, &["cherry-pick", "--continue"])
            .map_err(|e| Error::git_command_failed(e.to_string()))?;
        return Ok(GitOutput::from_output(id, path, "cherry-pick", output));
    }

    // Expand any PR numbers into commit SHAs via `gh`. PR commits come
    // before explicit refs in argv order so the user's positional args
    // can fine-tune ordering by interleaving — but in practice most
    // callers pass either `--pr` or `<refs>`, not both.
    let mut refs: Vec<String> = Vec::new();
    for pr in &options.prs {
        let pr_commits = resolve_pr_commits(&path, *pr)?;
        refs.extend(pr_commits);
    }
    refs.extend(options.refs.iter().cloned());

    if refs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "refs",
            "cherry-pick requires at least one commit ref or --pr <number>",
            None,
            Some(vec![
                "Provide a commit ref: homeboy git cherry-pick <sha>".to_string(),
                "Or pick a PR: homeboy git cherry-pick --pr <number>".to_string(),
            ]),
        ));
    }

    let mut args: Vec<String> = vec!["cherry-pick".into()];
    args.extend(refs);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "cherry-pick", output))
}

/// Resolve a GitHub PR number to its list of commit SHAs (oldest first)
/// using `gh pr view`. Used by [`cherry_pick`] to expand `--pr <n>`.
fn resolve_pr_commits(path: &str, pr: u64) -> Result<Vec<String>> {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", &pr.to_string(), "--json", "commits"])
        .current_dir(path)
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr view {}: {} (is `gh` installed and authenticated?)",
                pr, e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr view {} failed: {}",
            pr,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse `gh pr view {} --json commits`", pr)),
            Some(stdout.chars().take(200).collect()),
        )
    })?;

    let commits = parsed
        .get("commits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "gh pr view {} returned JSON without a `commits` array",
                pr
            ))
        })?;

    let mut shas = Vec::with_capacity(commits.len());
    for commit in commits {
        let oid = commit.get("oid").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::git_command_failed(format!("gh pr view {} returned a commit without `oid`", pr))
        })?;
        shas.push(oid.to_string());
    }
    Ok(shas)
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
    tag_at(component_id, tag_name, message, None)
}

/// Like [`tag`] but with an explicit path override for git operations.
pub fn tag_at(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let name = tag_name.ok_or_else(|| {
        Error::validation_invalid_argument("tagName", "Missing tag name", None, None)
    })?;
    let (id, path) = resolve_target(component_id, path_override)?;
    let args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", name, "-m", msg],
        None => vec!["tag", name],
    };
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "tag", output))
}

/// Check if a tag exists on the remote.
pub fn tag_exists_on_remote(path: &str, tag_name: &str) -> Result<bool> {
    Ok(crate::engine::command::run_in_optional(
        path,
        "git",
        &[
            "ls-remote",
            "--tags",
            "origin",
            &format!("refs/tags/{}", tag_name),
        ],
    )
    .map(|s| !s.is_empty())
    .unwrap_or(false))
}

/// Check if a tag exists locally.
pub fn tag_exists_locally(path: &str, tag_name: &str) -> Result<bool> {
    Ok(
        crate::engine::command::run_in_optional(path, "git", &["tag", "-l", tag_name])
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    )
}

/// Get the commit SHA a tag points to.
pub fn get_tag_commit(path: &str, tag_name: &str) -> Result<String> {
    crate::engine::command::run_in(
        path,
        "git",
        &["rev-list", "-n", "1", tag_name],
        &format!("get commit for tag '{}'", tag_name),
    )
}

/// Get the current HEAD commit SHA.
pub fn get_head_commit(path: &str) -> Result<String> {
    crate::engine::command::run_in(path, "git", &["rev-parse", "HEAD"], "get HEAD commit")
}

/// Fetch from remote and return count of commits behind upstream.
/// Returns Ok(Some(n)) if behind by n commits, Ok(None) if not behind or no upstream.
pub fn fetch_and_get_behind_count(path: &str) -> Result<Option<u32>> {
    // Run git fetch (update tracking refs)
    crate::engine::command::run_in(path, "git", &["fetch"], "git fetch")?;

    // Check if upstream exists
    let upstream = crate::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
    );
    if upstream.is_none() {
        return Ok(None); // No upstream configured
    }

    // Get ahead/behind counts
    let counts = crate::engine::command::run_in_optional(
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

/// Fetch from remote and fast-forward if behind.
///
/// Returns Ok(Some(n)) with the number of commits fast-forwarded, or Ok(None) if
/// already up-to-date. Errors if the fast-forward fails (diverged histories).
pub fn fetch_and_fast_forward(path: &str) -> Result<Option<u32>> {
    let behind = fetch_and_get_behind_count(path)?;

    match behind {
        None => Ok(None),
        Some(n) => {
            // Attempt fast-forward pull
            let output = execute_git(path, &["pull", "--ff-only"])
                .map_err(|e| Error::git_command_failed(e.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::validation_invalid_argument(
                    "remote_sync",
                    format!(
                        "Branch has diverged from remote — fast-forward failed: {}",
                        stderr.trim()
                    ),
                    None,
                    Some(vec![
                        "Resolve the divergence manually before releasing".to_string(),
                        "Run: git pull --rebase".to_string(),
                    ]),
                ));
            }

            Ok(Some(n))
        }
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
    let component = crate::component::resolve_effective(Some(id), None, None).ok();

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
                .and_then(crate::release::version::get_component_version);
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
    let component_ids: Vec<String> = project::resolve_project_components(&proj)?
        .into_iter()
        .map(|component| component.id)
        .collect();
    Ok(build_bulk_changes_output(&component_ids, include_diff))
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
        .filter(|id| project::has_component(&proj, id))
        .cloned()
        .collect();

    if filtered.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            format!(
                "None of the specified components are in project '{}'. Available: {}",
                project_id,
                project::project_component_ids(&proj).join(", ")
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

    // ------------------------------------------------------------------
    // rebase / cherry-pick / push --force-with-lease tests
    //
    // These shell out to real git in tempdirs. They exercise the wiring
    // (option struct → argv → execute_git → GitOutput) end-to-end without
    // touching the homeboy registry. `--path` keeps resolve_target out of
    // the registry path so the tests don't need HOME isolation.
    // ------------------------------------------------------------------

    /// Create a fresh git repo with a single committed file. Returns the
    /// TempDir (drop-cleanup) and the repo path as a String.
    fn init_repo_with_initial_commit() -> (tempfile::TempDir, String) {
        use std::fs;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();

        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&path)
            .output()
            .unwrap();
        fs::write(dir.path().join("README.md"), "initial\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn rebase_against_self_is_a_noop_success() {
        let (_dir, path) = init_repo_with_initial_commit();

        // Rebase onto HEAD — always a no-op, exit 0.
        let out = rebase_at(
            None,
            RebaseOptions {
                onto: Some("HEAD".to_string()),
                ..Default::default()
            },
            Some(&path),
        )
        .expect("rebase_at");

        assert!(out.success, "rebase HEAD should succeed: {:?}", out.stderr);
        assert_eq!(out.action, "rebase");
        assert_eq!(out.path, path);
    }

    #[test]
    fn rebase_abort_outside_of_rebase_is_an_error() {
        let (_dir, path) = init_repo_with_initial_commit();

        // git rebase --abort with no rebase in progress fails. We surface
        // that via GitOutput { success: false, stderr } — NOT via Err —
        // because the caller may want to inspect the message.
        let out = rebase_at(
            None,
            RebaseOptions {
                abort: true,
                ..Default::default()
            },
            Some(&path),
        )
        .expect("rebase_at returns Ok with failed GitOutput");

        assert!(!out.success);
        assert_ne!(out.exit_code, 0);
        assert!(
            out.stderr.contains("rebase") || out.stderr.contains("No rebase"),
            "expected stderr to mention rebase: {:?}",
            out.stderr
        );
    }

    #[test]
    fn cherry_pick_picks_a_commit_from_another_branch() {
        use std::fs;
        let (dir, path) = init_repo_with_initial_commit();

        // Create a side branch with a unique commit.
        Command::new("git")
            .args(["checkout", "-q", "-b", "side"])
            .current_dir(&path)
            .output()
            .unwrap();
        fs::write(dir.path().join("from-side.txt"), "side\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "side commit"])
            .current_dir(&path)
            .output()
            .unwrap();
        let side_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // Switch back to main; pick the side commit.
        Command::new("git")
            .args(["checkout", "-q", "main"])
            .current_dir(&path)
            .output()
            .unwrap();

        let out = cherry_pick_at(
            None,
            CherryPickOptions {
                refs: vec![side_sha.clone()],
                ..Default::default()
            },
            Some(&path),
        )
        .expect("cherry_pick_at");

        assert!(
            out.success,
            "cherry-pick should succeed: stderr={:?}",
            out.stderr
        );
        assert!(
            dir.path().join("from-side.txt").exists(),
            "cherry-picked file should exist on main"
        );
    }

    #[test]
    fn cherry_pick_with_no_refs_and_no_pr_errors() {
        let (_dir, path) = init_repo_with_initial_commit();

        // Empty refs + no PR + not continue/abort → user-facing error,
        // NOT a `git cherry-pick` invocation.
        let err = cherry_pick_at(None, CherryPickOptions::default(), Some(&path))
            .expect_err("cherry_pick with empty refs should Err");

        let msg = err.to_string();
        assert!(
            msg.contains("at least one commit ref") || msg.contains("--pr"),
            "expected helpful error, got: {}",
            msg
        );
    }

    #[test]
    fn cherry_pick_abort_outside_of_pick_is_a_failed_output() {
        let (_dir, path) = init_repo_with_initial_commit();

        let out = cherry_pick_at(
            None,
            CherryPickOptions {
                abort: true,
                ..Default::default()
            },
            Some(&path),
        )
        .expect("cherry_pick_at returns Ok with failed GitOutput");

        assert!(!out.success);
        assert_ne!(out.exit_code, 0);
    }

    #[test]
    fn push_options_force_with_lease_includes_flag() {
        // We can't test the real push (no remote) but we can verify that
        // push_at with force_with_lease=true at least invokes git with
        // the right argv. With no remote configured, git push fails —
        // we check stderr to confirm the flag flowed through and the
        // failure is the expected "no remote" failure, not a wiring bug.
        let (_dir, path) = init_repo_with_initial_commit();

        let out = push_at(
            None,
            PushOptions {
                tags: false,
                force_with_lease: true,
            },
            Some(&path),
        )
        .expect("push_at");

        assert!(!out.success, "push without remote should fail");
        // The failure message is from git, not us — but it should at
        // least mention the absence of a remote / upstream rather than
        // anything about an unknown flag.
        assert!(
            !out.stderr.contains("unknown option") && !out.stderr.contains("invalid argument"),
            "--force-with-lease should be a known flag, got: {}",
            out.stderr
        );
    }
}
