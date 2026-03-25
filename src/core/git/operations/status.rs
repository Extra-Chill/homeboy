//! status — extracted from operations.rs.

use std::process::Command;
use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::primitives::is_git_repo;
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use super::tag;
use super::BaselineSource;
use super::BulkIdsInput;
use super::BaselineInfo;
use super::RepoSnapshot;
use super::GitOutput;
use super::super::changes::*;
use super::super::commits::*;


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
