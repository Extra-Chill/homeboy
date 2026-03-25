//! build — extracted from operations.rs.

use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use super::NOISY_UNTRACKED_DIRS;
use super::RepoBaselineSnapshot;
use super::get_repo_snapshot;
use super::VERBOSE_UNTRACKED_THRESHOLD;
use super::changes;
use super::status;
use super::detect_baseline_with_version;
use super::ChangesOutput;
use super::push;


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

pub fn build_untracked_hint(path: &str, untracked_count: usize) -> Option<String> {
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

pub(crate) fn build_bulk_changes_output(
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
