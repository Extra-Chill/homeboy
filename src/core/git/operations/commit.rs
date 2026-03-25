//! commit — extracted from operations.rs.

use crate::config::read_json_spec_to_string;
use crate::error::{Error, Result};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use super::status;
use super::GitOutput;
use super::BulkCommitInput;
use super::push;
use super::CommitJsonOutput;
use super::CommitSpec;
use super::CommitOptions;
use super::super::changes::*;
use super::super::commits::*;


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
