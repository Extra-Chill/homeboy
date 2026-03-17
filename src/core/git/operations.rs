mod changes;
mod commit;
mod constants;
mod git_output;
mod path;
mod status;
mod tag_exists;
mod types;

pub use changes::*;
pub use commit::*;
pub use constants::*;
pub use git_output::*;
pub use path::*;
pub use status::*;
pub use tag_exists::*;
pub use types::*;

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

    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    "vendor",
    "target",
    ".cache",
];

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

/// Like [`push`] but with an explicit path override for git operations.
pub fn push_at(
    component_id: Option<&str>,
    tags: bool,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let args: Vec<&str> = if tags {
        vec!["push", "--follow-tags"]
    } else {
        vec!["push"]
    };
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "push", output))
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
}
