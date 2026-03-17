//! status — extracted from operations.rs.

use std::process::Command;
use crate::component;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::primitives::is_git_repo;
use super::super::{execute_git, resolve_target};
use super::from_output;
use super::CommitOptions;
use super::RepoSnapshot;
use super::BulkCommitInput;
use super::run_bulk_ids;
use super::commit;
use super::push_at;
use super::BulkIdsInput;
use super::CommitSpec;
use super::CommitJsonOutput;
use super::changes;
use super::super::changes::*;
use super::super::commits::*;
use super::super::*;


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

pub(crate) fn parse_ahead_behind(counts: &str) -> (Option<u32>, Option<u32>) {
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

/// Commit multiple components from JSON spec (bulk format).
pub(crate) fn commit_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
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
    push_at(component_id, tags, None)
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
    let (id, path) = resolve_target(component_id, None)?;
    let output =
        execute_git(&path, &["pull"]).map_err(|e| Error::git_command_failed(e.to_string()))?;
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
