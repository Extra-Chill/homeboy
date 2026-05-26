use serde::{Deserialize, Serialize};

use crate::core::config::read_json_spec_to_string;
use crate::core::error::{Error, Result};
use crate::core::output::{BulkResult, BulkResultBuilder};

use super::operation_output::GitOutput;
use super::{execute_git, resolve_target};

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

    let mut builder = BulkResultBuilder::with_capacity("commit", input.components.len());

    for spec in &input.components {
        let id = match &spec.id {
            Some(id) => id.clone(),
            None => {
                builder.record_error("unknown", "Missing 'id' field in bulk commit spec");
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
                    builder.record_success(id, output);
                } else {
                    builder.record_failed_result(id, output);
                }
            }
            Err(e) => {
                builder.record_error(id, e.to_string());
            }
        }
    }

    Ok(builder.finish())
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
