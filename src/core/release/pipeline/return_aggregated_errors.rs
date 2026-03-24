//! return_aggregated_errors — extracted from pipeline.rs.

use crate::component::{self, Component};
use crate::engine::run_dir::{self, RunDir};
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};
use crate::git::{self, UncommittedChanges};
use crate::release::changelog;
use crate::version;
use crate::git::{CommitCategory, CommitInfo};


pub(crate) fn build_semver_recommendation(
    component: &Component,
    requested_bump: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<Option<ReleaseSemverRecommendation>> {
    let requested = git::SemverBump::parse(requested_bump).ok_or_else(|| {
        Error::validation_invalid_argument(
            "bump_type",
            format!("Invalid bump type: {}", requested_bump),
            None,
            Some(vec!["Use one of: patch, minor, major".to_string()]),
        )
    })?;

    let (latest_tag, commits) = resolve_tag_and_commits(&component.local_path, monorepo)?;

    if commits.is_empty() {
        return Ok(None);
    }

    let recommended = git::recommended_bump_from_commits(&commits);
    let is_underbump = recommended
        .map(|r| requested.rank() < r.rank())
        .unwrap_or(false);

    let commit_rows: Vec<ReleaseSemverCommit> = commits
        .iter()
        .map(|c| ReleaseSemverCommit {
            sha: c.hash.clone(),
            subject: c.subject.clone(),
            commit_type: match c.category {
                git::CommitCategory::Breaking => "breaking",
                git::CommitCategory::Feature => "feature",
                git::CommitCategory::Fix => "fix",
                git::CommitCategory::Docs => "docs",
                git::CommitCategory::Chore => "chore",
                git::CommitCategory::Merge => "merge",
                git::CommitCategory::Other => "other",
            }
            .to_string(),
            breaking: c.category == git::CommitCategory::Breaking,
        })
        .collect();

    let reasons: Vec<String> = commits
        .iter()
        .filter(|c| {
            if let Some(rec) = recommended {
                match rec {
                    git::SemverBump::Major => c.category == git::CommitCategory::Breaking,
                    git::SemverBump::Minor => {
                        c.category == git::CommitCategory::Breaking
                            || c.category == git::CommitCategory::Feature
                    }
                    git::SemverBump::Patch => {
                        matches!(
                            c.category,
                            git::CommitCategory::Breaking
                                | git::CommitCategory::Feature
                                | git::CommitCategory::Fix
                                | git::CommitCategory::Other
                        )
                    }
                }
            } else {
                false
            }
        })
        .take(10)
        .map(|c| format!("{} {}", c.hash, c.subject))
        .collect();

    let range = latest_tag
        .as_ref()
        .map(|t| format!("{}..HEAD", t))
        .unwrap_or_else(|| "HEAD".to_string());

    Ok(Some(ReleaseSemverRecommendation {
        latest_tag,
        range,
        commits: commit_rows,
        recommended_bump: recommended.map(|r| r.as_str().to_string()),
        requested_bump: requested.as_str().to_string(),
        is_underbump,
        reasons,
    }))
}

/// Resolve the latest tag and commits since that tag for a component.
///
/// In a monorepo, uses component-prefixed tags and path-scoped commits.
/// In a single-repo, uses standard global tags and all commits.
pub(super) fn resolve_tag_and_commits(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<(Option<String>, Vec<git::CommitInfo>)> {
    match monorepo {
        Some(ctx) => {
            let latest_tag = git::get_latest_tag_with_prefix(&ctx.git_root, Some(&ctx.tag_prefix))?;
            let commits = git::get_commits_since_tag_for_path(
                &ctx.git_root,
                latest_tag.as_deref(),
                Some(&ctx.path_prefix),
            )?;
            Ok((latest_tag, commits))
        }
        None => {
            let latest_tag = git::get_latest_tag(local_path)?;
            let commits = git::get_commits_since_tag(local_path, latest_tag.as_deref())?;
            Ok((latest_tag, commits))
        }
    }
}

pub(crate) fn validate_changelog(component: &Component) -> Result<()> {
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::engine::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(component));

    if let Some(status) =
        changelog::check_next_section_content(&changelog_content, &settings.next_section_aliases)?
    {
        match status.as_str() {
            "empty" => {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Changelog has no unreleased entries",
                    None,
                    Some(vec![
                        "Add changelog entries: homeboy changelog add <component> -m \"...\""
                            .to_string(),
                    ]),
                ));
            }
            "subsection_headers_only" => {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Changelog has subsection headers but no items",
                    None,
                    Some(vec![
                        "Add changelog entries: homeboy changelog add <component> -m \"...\""
                            .to_string(),
                    ]),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Fetch from remote and fast-forward if behind.
///
/// Ensures the release commit is created on top of the actual remote HEAD,
/// preventing detached release tags when PRs merge during a CI quality gate.
/// Returns Err if the branch has diverged and can't be fast-forwarded.
pub(crate) fn validate_remote_sync(component: &Component) -> Result<()> {
    let synced = git::fetch_and_fast_forward(&component.local_path)?;

    if let Some(n) = synced {
        log_status!(
            "release",
            "Fast-forwarded {} commit(s) from remote before release",
            n
        );
    }

    Ok(())
}

/// Run code quality checks (lint + test) via the component's extension.
///
/// Resolves the extension for the component, then runs lint and test scripts
/// if the extension provides them. If no extension is configured or the extension
/// doesn't provide lint/test, those checks are silently skipped.
///
/// This is the pre-release quality gate — ensures code passes lint and tests
/// before any version bump or tag is created.
pub(crate) fn validate_code_quality(component: &Component) -> Result<()> {
    let lint_context = extension::lint::resolve_lint_command(component);
    let test_context = extension::test::resolve_test_command(component);

    let mut checks_run = 0;
    let mut failures = Vec::new();

    if let Ok(lint_context) = lint_context {
        log_status!("release", "Running lint ({})...", lint_context.extension_id);

        let release_run_dir = RunDir::create()?;
        let lint_findings_file = release_run_dir.step_file(run_dir::files::LINT_FINDINGS);

        match extension::lint::build_lint_runner(
            component,
            None,
            &[],
            false,
            None,
            None,
            false,
            None,
            None,
            None,
            &release_run_dir,
        )
        .and_then(|runner| runner.run())
        {
            Ok(output) => {
                checks_run += 1;

                // Check baseline before declaring pass/fail
                let lint_passed = if output.success {
                    true
                } else {
                    // Lint failed — but check if baseline says drift didn't increase
                    let source_path = std::path::Path::new(&component.local_path);
                    let findings =
                        crate::extension::lint::baseline::parse_findings_file(&lint_findings_file)
                            .unwrap_or_default();

                    if let Some(baseline) =
                        crate::extension::lint::baseline::load_baseline(source_path)
                    {
                        let comparison =
                            crate::extension::lint::baseline::compare(&findings, &baseline);
                        if comparison.drift_increased {
                            log_status!(
                                "release",
                                "Lint baseline drift increased: {} new finding(s)",
                                comparison.new_items.len()
                            );
                            false
                        } else {
                            log_status!(
                                "release",
                                "Lint has known findings but no new drift (baseline honored)"
                            );
                            true
                        }
                    } else {
                        // No baseline — raw exit code is authoritative
                        false
                    }
                };

                if lint_passed {
                    log_status!("release", "Lint passed");
                } else {
                    failures.push(format!("Lint failed (exit code {})", output.exit_code));
                }
            }
            Err(e) => {
                failures.push(format!("Lint error: {}", e));
            }
        }
    }

    if let Ok(test_context) = test_context {
        log_status!(
            "release",
            "Running tests ({})...",
            test_context.extension_id
        );
        let test_run_dir = RunDir::create()?;
        match extension::test::build_test_runner(
            component,
            None,
            &[],
            false,
            false,
            None,
            None,
            &test_run_dir,
        )
        .and_then(|runner| runner.run())
        {
            Ok(output) if output.success => {
                log_status!("release", "Tests passed");
                checks_run += 1;
            }
            Ok(output) => {
                checks_run += 1;
                failures.push(format!("Tests failed (exit code {})", output.exit_code));
            }
            Err(e) => {
                failures.push(format!("Test error: {}", e));
            }
        }
    }

    if checks_run == 0 {
        log_status!(
            "release",
            "No linked extensions provide lint/test scripts — skipping code quality checks"
        );
        return Ok(());
    }

    if failures.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "code_quality",
        failures.join("; "),
        None,
        Some(vec![
            "Fix the issues above before releasing".to_string(),
            "To bypass: homeboy release <component> --skip-checks".to_string(),
        ]),
    ))
}

/// Validate that commits since the last tag have corresponding changelog entries.
/// Returns Ok(Some(entries)) when entries need to be auto-generated (passed to executor),
/// Ok(None) if all entries already exist, or Err on failure.
/// Never writes to disk — the executor handles writes via finalize_with_generated_entries.
pub(crate) fn validate_commits_vs_changelog(
    component: &Component,
    dry_run: bool,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<Option<std::collections::HashMap<String, Vec<String>>>> {
    // Get latest tag and commits (scoped to component in monorepo)
    let (latest_tag, commits) = resolve_tag_and_commits(&component.local_path, monorepo)?;

    // If no commits, nothing to validate
    if commits.is_empty() {
        return Ok(None);
    }

    // Read unreleased changelog entries
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::engine::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let unreleased_entries =
        changelog::get_unreleased_entries(&changelog_content, &settings.next_section_aliases);

    let missing_commits = find_uncovered_commits(&commits, &unreleased_entries);

    // If all relevant commits are already represented in the changelog, no new
    // entries needed. Return an empty map (not None) so will_auto_generate stays
    // true — this prevents changelog_sync from running and failing when there's
    // no ## Unreleased section (fully automated changelogs never have one).
    if missing_commits.is_empty() {
        return Ok(Some(std::collections::HashMap::new()));
    }

    // Check if changelog is already finalized ahead of the latest tag.
    // This handles fully automated changelogs where entries are generated from
    // commits and finalized into a versioned section — no ## Unreleased section
    // ever exists on disk. Return an empty entries map so will_auto_generate is
    // true, which skips changelog_sync validation (it would fail looking for a
    // non-existent ## Unreleased section).
    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content);
    if let (Some(latest_tag), Some(changelog_ver_str)) = (&latest_tag, latest_changelog_version) {
        let tag_version = latest_tag.trim_start_matches('v');
        if let (Ok(tag_ver), Ok(cl_ver)) = (
            semver::Version::parse(tag_version),
            semver::Version::parse(&changelog_ver_str),
        ) {
            if cl_ver > tag_ver {
                log_status!(
                    "release",
                    "Changelog already finalized at {} (ahead of tag {})",
                    changelog_ver_str,
                    latest_tag
                );
                return Ok(Some(std::collections::HashMap::new()));
            }
        }
    }

    // Build entries from commits — pure computation, no disk writes.
    let entries = group_commits_for_changelog(&missing_commits);
    let count: usize = entries.values().map(|v| v.len()).sum();

    if dry_run {
        log_status!(
            "release",
            "Would auto-generate {} changelog entries from commits (dry run)",
            count
        );
    } else {
        log_status!(
            "release",
            "Will auto-generate {} changelog entries from commits",
            count
        );
    }

    Ok(Some(entries))
}

pub(crate) fn normalize_changelog_text(value: &str) -> String {
    // Strip trailing PR/issue references like (#123) before normalizing
    let stripped = strip_pr_reference(value);
    stripped
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip trailing PR/issue references like "(#123)" or "(#123, #456)" from text.
pub(crate) fn strip_pr_reference(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(pos) = trimmed.rfind('(') {
        let after = &trimmed[pos..];
        // Match patterns like (#123) or (#123, #456)
        if after.ends_with(')')
            && after[1..after.len() - 1]
                .split(',')
                .all(|part| part.trim().starts_with('#'))
        {
            return trimmed[..pos].trim().to_string();
        }
    }
    trimmed.to_string()
}

pub(crate) fn find_uncovered_commits(
    commits: &[git::CommitInfo],
    unreleased_entries: &[String],
) -> Vec<git::CommitInfo> {
    let normalized_entries: Vec<String> = unreleased_entries
        .iter()
        .map(|entry| normalize_changelog_text(entry))
        .collect();

    commits
        .iter()
        .filter(|commit| commit.category.to_changelog_entry_type().is_some())
        .filter(|commit| {
            let normalized_subject =
                normalize_changelog_text(git::strip_conventional_prefix(&commit.subject));

            // Check both directions: entry contains subject OR subject contains entry.
            // This handles cases where the manual changelog entry is a substring of the
            // commit message or vice versa.
            !normalized_entries.iter().any(|entry| {
                !entry.is_empty()
                    && (entry.contains(&normalized_subject)
                        || normalized_subject.contains(entry.as_str()))
            })
        })
        .cloned()
        .collect()
}

/// Generate changelog entries from conventional commit messages.
/// Group conventional commits into changelog entries by type.
/// Returns a map of entry_type -> messages (e.g. "added" -> ["feature X", "feature Y"]).
/// Pure function — no I/O. Skips docs, chore, and merge commits.
pub(crate) fn group_commits_for_changelog(
    commits: &[git::CommitInfo],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut entries_by_type: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for commit in commits {
        if let Some(entry_type) = commit.category.to_changelog_entry_type() {
            let message = strip_pr_reference(git::strip_conventional_prefix(&commit.subject));
            entries_by_type
                .entry(entry_type.to_string())
                .or_default()
                .push(message);
        }
    }

    // If no entries generated (all docs/chore/merge), use first non-skip commit or fallback
    if entries_by_type.is_empty() {
        let fallback = commits
            .iter()
            .find(|c| {
                !matches!(
                    c.category,
                    git::CommitCategory::Docs
                        | git::CommitCategory::Chore
                        | git::CommitCategory::Merge
                )
            })
            .map(|c| strip_pr_reference(git::strip_conventional_prefix(&c.subject)))
            .unwrap_or_else(|| "Internal improvements".to_string());

        entries_by_type.insert("changed".to_string(), vec![fallback]);
    }

    entries_by_type
}

/// Serialize changelog entries to JSON for embedding in step config.
pub(crate) fn changelog_entries_to_json(
    entries: &std::collections::HashMap<String, Vec<String>>,
) -> serde_json::Value {
    serde_json::to_value(entries).unwrap_or_default()
}

/// Deserialize changelog entries from step config JSON.
pub(super) fn changelog_entries_from_json(
    value: &serde_json::Value,
) -> Option<std::collections::HashMap<String, Vec<String>>> {
    serde_json::from_value(value.clone()).ok()
}

/// Derive publish targets from extensions that have `release.publish` action.
pub(crate) fn get_publish_targets(extensions: &[ExtensionManifest]) -> Vec<String> {
    extensions
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.publish"))
        .map(|m| m.id.clone())
        .collect()
}

/// Check if any extension provides the `release.package` action.
pub(crate) fn has_package_capability(extensions: &[ExtensionManifest]) -> bool {
    extensions
        .iter()
        .any(|m| m.actions.iter().any(|a| a.id == "release.package"))
}

/// Build all release steps: core steps (non-configurable) + publish steps (extension-derived).
pub(crate) fn build_release_steps(
    component: &Component,
    extensions: &[ExtensionManifest],
    current_version: &str,
    new_version: &str,
    options: &ReleaseOptions,
    monorepo: Option<&git::MonorepoContext>,
    warnings: &mut Vec<String>,
    _hints: &mut Vec<String>,
) -> Result<Vec<ReleasePlanStep>> {
    let mut steps = Vec::new();
    let publish_targets = get_publish_targets(extensions);

    // === WARNING: No package capability ===
    if !publish_targets.is_empty() && !has_package_capability(extensions) {
        warnings.push(
            "Publish targets derived from extensions but no extension provides 'release.package'. \
             Add a extension like 'rust' that provides packaging."
                .to_string(),
        );
    }

    // === CORE STEPS (non-configurable, always present) ===

    // 1. Version bump
    steps.push(ReleasePlanStep {
        id: "version".to_string(),
        step_type: "version".to_string(),
        label: Some(format!(
            "Bump version {} → {} ({})",
            current_version, new_version, options.bump_type
        )),
        needs: vec![],
        config: {
            let mut config = std::collections::HashMap::new();
            config.insert(
                "bump".to_string(),
                serde_json::Value::String(options.bump_type.clone()),
            );
            config.insert(
                "from".to_string(),
                serde_json::Value::String(current_version.to_string()),
            );
            config.insert(
                "to".to_string(),
                serde_json::Value::String(new_version.to_string()),
            );
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // 2. Git commit
    steps.push(ReleasePlanStep {
        id: "git.commit".to_string(),
        step_type: "git.commit".to_string(),
        label: Some(format!("Commit release: v{}", new_version)),
        needs: vec!["version".to_string()],
        config: std::collections::HashMap::new(),
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // === PUBLISH STEPS (extension-derived, skipped with --skip-publish) ===
    //
    // Package runs BEFORE git.tag + git.push so that build failures don't
    // leave orphan tags on the remote. The order is:
    //   version → git.commit → package → git.tag → git.push → publish → cleanup
    //
    // If the build fails: the version is committed locally but no tag exists
    // and nothing is pushed. The user can `git reset HEAD~1` to undo.
    // If the build succeeds: tag + push + publish proceed normally.

    let tag_needs = if !publish_targets.is_empty() && !options.skip_publish {
        // 3. Package (produces artifacts — runs before tagging)
        steps.push(ReleasePlanStep {
            id: "package".to_string(),
            step_type: "package".to_string(),
            label: Some("Package release artifacts".to_string()),
            needs: vec!["git.commit".to_string()],
            config: std::collections::HashMap::new(),
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
        vec!["package".to_string()]
    } else {
        vec!["git.commit".to_string()]
    };

    // 4. Git tag (after package succeeds, or after commit if no package)
    let tag_name = match monorepo {
        Some(ctx) => ctx.format_tag(new_version),
        None => format!("v{}", new_version),
    };
    steps.push(ReleasePlanStep {
        id: "git.tag".to_string(),
        step_type: "git.tag".to_string(),
        label: Some(format!("Tag {}", tag_name)),
        needs: tag_needs,
        config: {
            let mut config = std::collections::HashMap::new();
            config.insert("name".to_string(), serde_json::Value::String(tag_name));
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // 5. Git push (commits AND tags)
    steps.push(ReleasePlanStep {
        id: "git.push".to_string(),
        step_type: "git.push".to_string(),
        label: Some("Push to remote".to_string()),
        needs: vec!["git.tag".to_string()],
        config: {
            let mut config = std::collections::HashMap::new();
            config.insert("tags".to_string(), serde_json::Value::Bool(true));
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    if !publish_targets.is_empty() && !options.skip_publish {
        // 6. Publish steps (all run independently after git.push)
        // Package already ran before tagging; publish needs the push to have
        // completed (e.g., crates.io/Homebrew need the tag on the remote).
        let mut publish_step_ids: Vec<String> = Vec::new();
        for target in &publish_targets {
            let step_id = format!("publish.{}", target);
            let step_type = format!("publish.{}", target);

            publish_step_ids.push(step_id.clone());
            steps.push(ReleasePlanStep {
                id: step_id,
                step_type,
                label: Some(format!("Publish to {}", target)),
                needs: vec!["git.push".to_string()],
                config: std::collections::HashMap::new(),
                status: ReleasePlanStatus::Ready,
                missing: vec![],
            });
        }

        // 7. Cleanup step (runs after all publish steps)
        // Skip cleanup when --deploy is pending — the deploy step needs the
        // build artifact (ZIP) that cleanup would delete. Cleanup runs after
        // deployment completes instead.
        if !options.deploy {
            steps.push(ReleasePlanStep {
                id: "cleanup".to_string(),
                step_type: "cleanup".to_string(),
                label: Some("Clean up release artifacts".to_string()),
                needs: publish_step_ids,
                config: std::collections::HashMap::new(),
                status: ReleasePlanStatus::Ready,
                missing: vec![],
            });
        }
    } else if options.skip_publish && !publish_targets.is_empty() {
        log_status!("release", "Skipping publish/package steps (--skip-publish)");
    }

    // === POST-RELEASE STEP (optional, runs after everything else) ===
    // Always runs when hooks are configured — NOT gated on --skip-publish.
    // Post-release hooks (e.g., moving floating tags) are distinct from publish
    // targets (crates.io, npm, etc.). --skip-publish only skips publish/package steps.
    let post_release_hooks =
        crate::engine::hooks::resolve_hooks(component, crate::engine::hooks::events::POST_RELEASE);
    if !post_release_hooks.is_empty() {
        let post_release_needs = if !options.skip_publish && !publish_targets.is_empty() {
            vec!["cleanup".to_string()]
        } else {
            vec!["git.push".to_string()]
        };

        steps.push(ReleasePlanStep {
            id: "post_release".to_string(),
            step_type: "post_release".to_string(),
            label: Some("Run post-release hooks".to_string()),
            needs: post_release_needs,
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert(
                    "commands".to_string(),
                    serde_json::Value::Array(
                        post_release_hooks
                            .iter()
                            .map(|s: &String| serde_json::Value::String(s.clone()))
                            .collect(),
                    ),
                );
                config
            },
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    }

    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_semver_recommendation_some_vec_use_one_of_patch_minor_major_to_string() {

        let result = build_semver_recommendation();
        assert!(result.is_some(), "expected Some for: Some(vec![\"Use one of: patch, minor, major\".to_string()]),");
    }

    #[test]
    fn test_build_semver_recommendation_default_path() {

        let _result = build_semver_recommendation();
    }

    #[test]
    fn test_build_semver_recommendation_default_path_2() {

        let _result = build_semver_recommendation();
    }

    #[test]
    fn test_build_semver_recommendation_commits_is_empty() {

        let _result = build_semver_recommendation();
    }

    #[test]
    fn test_build_semver_recommendation_if_let_some_rec_recommended() {

        let result = build_semver_recommendation();
        assert!(result.is_some(), "expected Some for: if let Some(rec) = recommended {{");
    }

    #[test]
    fn test_resolve_tag_and_commits_match_monorepo() {

        let result = resolve_tag_and_commits();
        assert!(result.is_some(), "expected Some for: match monorepo");
    }

    #[test]
    fn test_resolve_tag_and_commits_match_monorepo_2() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_some_ctx_path_prefix() {

        let result = resolve_tag_and_commits();
        assert!(result.is_some(), "expected Some for: Some(&ctx.path_prefix),");
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_ok_latest_tag_commits() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path_2() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path_3() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_ok_latest_tag_commits_2() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_validate_changelog_default_path() {

        let _result = validate_changelog();
    }

    #[test]
    fn test_validate_changelog_default_path_2() {

        let _result = validate_changelog();
    }

    #[test]
    fn test_validate_changelog_default_path_3() {

        let _result = validate_changelog();
    }

    #[test]
    fn test_validate_changelog_if_let_some_status() {

        let _result = validate_changelog();
    }

    #[test]
    fn test_validate_changelog_let_some_status() {

        let _result = validate_changelog();
    }

    #[test]
    fn test_validate_changelog_ok() {

        let result = validate_changelog();
        assert!(result.is_ok(), "expected Ok for: Ok(())");
    }

    #[test]
    fn test_validate_changelog_has_expected_effects() {
        // Expected effects: logging

        let _ = validate_changelog();
    }

    #[test]
    fn test_validate_remote_sync_default_path() {

        let _result = validate_remote_sync();
    }

    #[test]
    fn test_validate_remote_sync_if_let_some_n_synced() {

        let _result = validate_remote_sync();
    }

    #[test]
    fn test_validate_remote_sync_ok() {

        let result = validate_remote_sync();
        assert!(result.is_ok(), "expected Ok for: Ok(())");
    }

    #[test]
    fn test_validate_remote_sync_has_expected_effects() {
        // Expected effects: logging

        let _ = validate_remote_sync();
    }

    #[test]
    fn test_validate_code_quality_if_let_ok_lint_context_lint_context() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: if let Ok(lint_context) = lint_context {{");
    }

    #[test]
    fn test_validate_code_quality_let_ok_lint_context_lint_context() {

        let _result = validate_code_quality();
    }

    #[test]
    fn test_validate_code_quality_ok_output() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: Ok(output) => {{");
    }

    #[test]
    fn test_validate_code_quality_if_let_some_baseline() {

        let _result = validate_code_quality();
    }

    #[test]
    fn test_validate_code_quality_else() {

        let result = validate_code_quality();
        assert!(result.is_err(), "expected Err for: else");
    }

    #[test]
    fn test_validate_code_quality_if_let_ok_test_context_test_context() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: if let Ok(test_context) = test_context {{");
    }

    #[test]
    fn test_validate_code_quality_default_path() {

        let _result = validate_code_quality();
    }

    #[test]
    fn test_validate_code_quality_ok_output_if_output_success() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: Ok(output) if output.success => {{");
    }

    #[test]
    fn test_validate_code_quality_ok_output_2() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: Ok(output) => {{");
    }

    #[test]
    fn test_validate_code_quality_err_e() {

        let result = validate_code_quality();
        assert!(result.is_err(), "expected Err for: Err(e) => {{");
    }

    #[test]
    fn test_validate_code_quality_checks_run_0() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: checks_run == 0");
    }

    #[test]
    fn test_validate_code_quality_failures_is_empty() {

        let result = validate_code_quality();
        assert!(result.is_ok(), "expected Ok for: failures.is_empty()");
    }

    #[test]
    fn test_validate_code_quality_has_expected_effects() {
        // Expected effects: logging, mutation

        let _ = validate_code_quality();
    }

    #[test]
    fn test_validate_commits_vs_changelog_default_path() {

        let _result = validate_commits_vs_changelog();
    }

    #[test]
    fn test_validate_commits_vs_changelog_commits_is_empty() {

        let _result = validate_commits_vs_changelog();
    }

    #[test]
    fn test_validate_commits_vs_changelog_commits_is_empty_2() {

        let _result = validate_commits_vs_changelog();
    }

    #[test]
    fn test_validate_commits_vs_changelog_default_path_2() {

        let _result = validate_commits_vs_changelog();
    }

    #[test]
    fn test_validate_commits_vs_changelog_default_path_3() {

        let result = validate_commits_vs_changelog();
        assert!(result.is_some(), "expected Some for: default path");
    }

    #[test]
    fn test_validate_commits_vs_changelog_missing_commits_is_empty() {

        let result = validate_commits_vs_changelog();
        assert!(result.is_some(), "expected Some for: missing_commits.is_empty()");
    }

    #[test]
    fn test_validate_commits_vs_changelog_if_let_some_latest_tag_some_changelog_ver_str_latest_tag_lat() {

        let result = validate_commits_vs_changelog();
        assert!(result.is_some(), "expected Some for: if let (Some(latest_tag), Some(changelog_ver_str)) = (&latest_tag, latest_changelog_version) {{");
    }

    #[test]
    fn test_validate_commits_vs_changelog_let_some_latest_tag_some_changelog_ver_str_latest_tag_latest() {

        let _result = validate_commits_vs_changelog();
    }

    #[test]
    fn test_validate_commits_vs_changelog_default_path_4() {

        let result = validate_commits_vs_changelog();
        assert!(result.is_some(), "expected Some for: default path");
    }

    #[test]
    fn test_validate_commits_vs_changelog_ok_some_entries() {

        let result = validate_commits_vs_changelog();
        assert!(result.is_some(), "expected Some for: Ok(Some(entries))");
    }

    #[test]
    fn test_validate_commits_vs_changelog_has_expected_effects() {
        // Expected effects: logging

        let _ = validate_commits_vs_changelog();
    }

    #[test]
    fn test_normalize_changelog_text_default_path() {

        let _result = normalize_changelog_text();
    }

    #[test]
    fn test_strip_pr_reference_if_let_some_pos_trimmed_rfind() {

        let _result = strip_pr_reference();
    }

    #[test]
    fn test_find_uncovered_commits_default_path() {

        let result = find_uncovered_commits();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_group_commits_for_changelog_if_let_some_entry_type_commit_category_to_changelog_entry_ty() {

        let result = group_commits_for_changelog();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(entry_type) = commit.category.to_changelog_entry_type() {{");
    }

    #[test]
    fn test_group_commits_for_changelog_has_expected_effects() {
        // Expected effects: mutation

        let _ = group_commits_for_changelog();
    }

    #[test]
    fn test_changelog_entries_to_json_default_path() {

        let _result = changelog_entries_to_json();
    }

    #[test]
    fn test_changelog_entries_from_json_default_path() {

        let _result = changelog_entries_from_json();
    }

    #[test]
    fn test_get_publish_targets_default_path() {

        let result = get_publish_targets();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_has_package_capability_default_path() {

        let _result = has_package_capability();
    }

    #[test]
    fn test_build_release_steps_label_some_format_commit_release_v_new_version() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(format!(\"Commit release: v{{}}\", new_version)),");
    }

    #[test]
    fn test_build_release_steps_label_some_package_release_artifacts_to_string() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(\"Package release artifacts\".to_string()),");
    }

    #[test]
    fn test_build_release_steps_some_ctx_ctx_format_tag_new_version() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: Some(ctx) => ctx.format_tag(new_version),");
    }

    #[test]
    fn test_build_release_steps_label_some_format_tag_tag_name() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(format!(\"Tag {{}}\", tag_name)),");
    }

    #[test]
    fn test_build_release_steps_label_some_push_to_remote_to_string() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(\"Push to remote\".to_string()),");
    }

    #[test]
    fn test_build_release_steps_label_some_format_publish_to_target() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(format!(\"Publish to {{}}\", target)),");
    }

    #[test]
    fn test_build_release_steps_options_deploy() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: !options.deploy");
    }

    #[test]
    fn test_build_release_steps_label_some_run_post_release_hooks_to_string() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: label: Some(\"Run post-release hooks\".to_string()),");
    }

    #[test]
    fn test_build_release_steps_ok_steps() {

        let result = build_release_steps();
        assert!(!result.is_empty(), "expected non-empty collection for: Ok(steps)");
    }

    #[test]
    fn test_build_release_steps_has_expected_effects() {
        // Expected effects: logging, mutation

        let _ = build_release_steps();
    }

}
