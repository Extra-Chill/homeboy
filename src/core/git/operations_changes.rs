use serde::Serialize;

use crate::core::config::read_json_spec_to_string;
use crate::core::error::{Error, Result};
use crate::core::output::{BulkResult, BulkResultBuilder};
use crate::core::project;
use crate::core::release::changelog;

use super::changes::*;
use super::commits::*;
use super::operations::get_repo_snapshot;
use super::resolve_target;

const DEFAULT_COMMIT_LIMIT: usize = 10;

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

#[derive(Debug, Clone)]
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
    // machine) are available before we resolve the baseline. Best-effort:
    // if there is no remote or the network is unavailable we silently
    // proceed with whatever tags are already local.
    let _ =
        crate::core::engine::command::run_in_optional(path, "git", &["fetch", "--tags", "--quiet"]);

    detect_baseline_with_version_from_fetched_tags(path, current_version)
}

/// Detect baseline using tags already available locally.
///
/// Callers that batch status work can fetch tags once per repository and reuse
/// this path to avoid repeating network fetches for every component probe.
pub fn detect_baseline_with_version_from_fetched_tags(
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
                    return Ok(version_commit_baseline(
                        Some(tag.clone()),
                        hash,
                        format!(
                            "Latest tag '{}' doesn't match version {}. Using most recent version commit.",
                            tag, current
                        ),
                    ));
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
        return Ok(version_commit_baseline(
            None,
            hash,
            "No tags found. Using most recent version commit as baseline.".to_string(),
        ));
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

fn version_commit_baseline(
    latest_tag: Option<String>,
    reference: String,
    warning: String,
) -> BaselineInfo {
    BaselineInfo {
        latest_tag,
        source: Some(BaselineSource::VersionCommit),
        reference: Some(reference),
        warning: Some(warning),
    }
}

fn resolve_changelog_info(
    component: &crate::core::component::Component,
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

/// Get all changes for a component.
pub fn changes(
    component_id: Option<&str>,
    since_tag: Option<&str>,
    include_diff: bool,
) -> Result<ChangesOutput> {
    changes_at(component_id, since_tag, include_diff, None)
}

/// Like [`changes`] but with an explicit path override for git operations.
pub fn changes_at(
    component_id: Option<&str>,
    since_tag: Option<&str>,
    include_diff: bool,
    path_override: Option<&str>,
) -> Result<ChangesOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    // Load component for version checking and changelog info
    let component = crate::core::component::resolve_effective(Some(&id), Some(&path), None).ok();

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
                .and_then(crate::core::release::version::get_component_version);
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
        component_id: id,
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
    let mut builder = BulkResultBuilder::with_capacity("changes", component_ids.len());

    for id in component_ids {
        match changes(Some(id), None, include_diff) {
            Ok(output) => {
                if output.success {
                    builder.record_success(id.clone(), output);
                } else {
                    builder.record_failed_result(id.clone(), output);
                }
            }
            Err(e) => {
                builder.record_error(id.clone(), e.to_string());
            }
        }
    }

    builder.finish()
}

/// Get changes for multiple components from JSON spec.
pub fn changes_bulk(json_spec: &str, include_diff: bool) -> Result<BulkResult<ChangesOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: super::operations::BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
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
