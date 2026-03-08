use std::path::Path;

use crate::changelog;
use crate::component::{self, Component};
use crate::core::lint_baseline;
use crate::core::local_files::FileSystem;
use crate::engine::pipeline::{self, PipelineStep};
use crate::error::{Error, ErrorCode, Result};
use crate::extension::{self, ExtensionManifest, ExtensionRunner};
use crate::git::{self, UncommittedChanges};
use crate::utils::validation::ValidationCollector;
use crate::version;

use super::executor::ReleaseStepExecutor;
use super::resolver::{resolve_extensions, ReleaseCapabilityResolver};
use super::types::{
    ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
    ReleaseSemverCommit, ReleaseSemverRecommendation,
};

/// Load a component with portable config fallback when path_override is set.
/// In CI environments, the component may not be registered — only homeboy.json exists.
fn load_component(component_id: &str, options: &ReleaseOptions) -> Result<Component> {
    match component::load(component_id) {
        Ok(mut comp) => {
            if let Some(ref path) = options.path_override {
                comp.local_path = path.clone();
            }
            Ok(comp)
        }
        Err(err) if matches!(err.code, ErrorCode::ComponentNotFound) => {
            if let Some(ref path) = options.path_override {
                if let Some(mut discovered) = component::discover_from_portable(Path::new(path)) {
                    discovered.id = component_id.to_string();
                    discovered.local_path = path.clone();
                    Ok(discovered)
                } else {
                    Ok(Component::new(
                        component_id.to_string(),
                        path.clone(),
                        String::new(),
                        None,
                    ))
                }
            } else {
                Err(err)
            }
        }
        Err(err) => Err(err),
    }
}

/// Execute a release by computing the plan and executing it.
/// What you preview (dry-run) is what you execute.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    let release_plan = plan(component_id, options)?;

    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component, None)?;
    let resolver = ReleaseCapabilityResolver::new(extensions.clone());
    let executor = ReleaseStepExecutor::new(component_id.to_string(), extensions);

    let pipeline_steps: Vec<PipelineStep> = release_plan
        .steps
        .iter()
        .map(|s| PipelineStep {
            id: s.id.clone(),
            step_type: s.step_type.clone(),
            label: s.label.clone(),
            needs: s.needs.clone(),
            config: s.config.clone(),
        })
        .collect();

    let run_result = pipeline::run(
        &pipeline_steps,
        std::sync::Arc::new(executor),
        std::sync::Arc::new(resolver),
        release_plan.enabled,
        "release.steps",
    )?;

    Ok(ReleaseRun {
        component_id: component_id.to_string(),
        enabled: release_plan.enabled,
        result: run_result,
    })
}

/// Plan a release with built-in core steps and extension-derived publish targets.
///
/// Requires a clean working tree (uncommitted changes will cause an error).
///
/// Core steps (always generated, non-configurable):
/// 1. Version bump + changelog finalization
/// 2. Git commit
/// 3. Git tag
/// 4. Git push (commits AND tags)
///
/// Publish steps (derived from extensions):
/// - From component's extensions that have `release.publish` action
/// - Or explicit `release.publish` array if configured
pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component, None)?;

    let mut v = ValidationCollector::new();

    // === Stage 0: Remote sync check (preflight) ===
    v.capture(validate_remote_sync(&component), "remote_sync");

    // === Stage 0.5: Code quality checks (lint + test) ===
    if options.skip_checks {
        log_status!("release", "Skipping code quality checks (--skip-checks)");
    } else {
        v.capture(validate_code_quality(&component), "code_quality");
    }

    // Build semver recommendation from commits early so both JSON output and
    // validation paths share a single source of truth.
    let semver_recommendation = build_semver_recommendation(&component, &options.bump_type)?;

    // === Stage 1: Independent validations ===
    v.capture(
        validate_commits_vs_changelog(&component, options.dry_run),
        "commits",
    );
    v.capture(validate_changelog(&component), "changelog");
    let version_info = v.capture(version::read_version(Some(component_id)), "version");

    // === Stage 2: Version-dependent validations ===
    let new_version = if let Some(ref info) = version_info {
        match version::increment_version(&info.version, &options.bump_type) {
            Some(ver) => Some(ver),
            None => {
                v.push(
                    "version",
                    &format!("Invalid version format: {}", info.version),
                    None,
                );
                None
            }
        }
    } else {
        None
    };

    if let (Some(ref info), Some(ref new_ver)) = (&version_info, &new_version) {
        v.capture(
            version::validate_changelog_for_bump(&component, &info.version, new_ver),
            "changelog_sync",
        );
    }

    // === Stage 3: Working tree check ===
    if let Some(ref info) = version_info {
        let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
        if uncommitted.has_changes {
            let changelog_path = changelog::resolve_changelog_path(&component)?;
            let version_targets: Vec<String> =
                info.targets.iter().map(|t| t.full_path.clone()).collect();

            let allowed = get_release_allowed_files(
                &changelog_path,
                &version_targets,
                std::path::Path::new(&component.local_path),
            );
            let unexpected = get_unexpected_uncommitted_files(&uncommitted, &allowed);

            if !unexpected.is_empty() {
                v.push(
                    "working_tree",
                    "Uncommitted changes detected",
                    Some(serde_json::json!({
                        "files": unexpected,
                        "hint": "Commit changes or stash before release"
                    })),
                );
            } else if uncommitted.has_changes && !options.dry_run {
                // Only changelog/version files are uncommitted — auto-stage them
                // so the release commit includes them (e.g., after `homeboy changelog add`).
                // Skip in dry-run mode to avoid mutating working tree.
                log_status!(
                    "release",
                    "Auto-staging changelog/version files for release commit"
                );
                let all_files: Vec<&String> = uncommitted
                    .staged
                    .iter()
                    .chain(uncommitted.unstaged.iter())
                    .collect();
                for file in all_files {
                    let full_path = std::path::Path::new(&component.local_path).join(file);
                    let _ = std::process::Command::new("git")
                        .args(["add", &full_path.to_string_lossy()])
                        .current_dir(&component.local_path)
                        .output();
                }
            }
        }
    }

    // === Return aggregated errors or proceed ===
    v.finish()?;

    // All validations passed — these are guaranteed Some by the validator above
    let version_info = version_info.ok_or_else(|| {
        Error::internal_unexpected("version_info missing after validation".to_string())
    })?;
    let new_version = new_version.ok_or_else(|| {
        Error::internal_unexpected("new_version missing after validation".to_string())
    })?;

    let mut warnings = Vec::new();
    let mut hints = Vec::new();

    let steps = build_release_steps(
        &component,
        &extensions,
        &version_info.version,
        &new_version,
        options,
        &mut warnings,
        &mut hints,
    )?;

    if options.dry_run {
        hints.push("Dry run: no changes will be made".to_string());
    }

    Ok(ReleasePlan {
        component_id: component_id.to_string(),
        enabled: true,
        steps,
        semver_recommendation,
        warnings,
        hints,
    })
}

fn build_semver_recommendation(
    component: &Component,
    requested_bump: &str,
) -> Result<Option<ReleaseSemverRecommendation>> {
    let requested = git::SemverBump::parse(requested_bump).ok_or_else(|| {
        Error::validation_invalid_argument(
            "bump_type",
            format!("Invalid bump type: {}", requested_bump),
            None,
            Some(vec!["Use one of: patch, minor, major".to_string()]),
        )
    })?;

    let latest_tag = git::get_latest_tag(&component.local_path)?;
    let commits = git::get_commits_since_tag(&component.local_path, latest_tag.as_deref())?;

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

fn validate_changelog(component: &Component) -> Result<()> {
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
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

/// Check if local branch is behind remote (after fetching).
/// Returns Err with actionable hints if behind, Ok(()) if up to date.
fn validate_remote_sync(component: &Component) -> Result<()> {
    let behind = git::fetch_and_get_behind_count(&component.local_path)?;

    if let Some(n) = behind {
        return Err(Error::validation_invalid_argument(
            "remote_sync",
            format!("Local branch is {} commit(s) behind remote", n),
            None,
            Some(vec![
                "Pull remote changes before releasing to avoid push conflicts".to_string(),
                "Run: git pull --rebase".to_string(),
            ]),
        ));
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
fn validate_code_quality(component: &Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(ext) if !ext.is_empty() => ext,
        _ => {
            log_status!(
                "release",
                "No extensions configured — skipping code quality checks"
            );
            return Ok(());
        }
    };

    // Determine which extension to use (prefer wordpress, then first available)
    let extension_id = if extensions.contains_key("wordpress") {
        "wordpress"
    } else {
        match extensions.keys().next() {
            Some(id) => id.as_str(),
            None => return Ok(()),
        }
    };

    let manifest = match extension::load_extension(extension_id) {
        Ok(m) => m,
        Err(_) => {
            log_status!(
                "release",
                "Extension '{}' not found — skipping code quality checks",
                extension_id
            );
            return Ok(());
        }
    };

    let mut checks_run = 0;
    let mut failures = Vec::new();

    // Run lint if extension provides it
    if let Some(lint_script) = manifest.lint_script() {
        log_status!("release", "Running lint ({})...", extension_id);

        // Create a temporary findings file so we can compare against baseline
        let lint_findings_file = std::env::temp_dir().join(format!(
            "homeboy-release-lint-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        match ExtensionRunner::new(&component.id, lint_script)
            .component(component.clone())
            .env(
                "HOMEBOY_LINT_FINDINGS_FILE",
                &lint_findings_file.to_string_lossy(),
            )
            .run()
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
                        lint_baseline::parse_findings_file(&lint_findings_file).unwrap_or_default();
                    let _ = std::fs::remove_file(&lint_findings_file);

                    if let Some(baseline) = lint_baseline::load_baseline(source_path) {
                        let comparison = lint_baseline::compare(&findings, &baseline);
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

                // Clean up findings file if not already removed
                let _ = std::fs::remove_file(&lint_findings_file);

                if lint_passed {
                    log_status!("release", "Lint passed");
                } else {
                    failures.push(format!("Lint failed (exit code {})", output.exit_code));
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&lint_findings_file);
                failures.push(format!("Lint error: {}", e));
            }
        }
    }

    // Run tests if extension provides them
    if let Some(test_script) = manifest.test_script() {
        log_status!("release", "Running tests ({})...", extension_id);
        match ExtensionRunner::new(&component.id, test_script)
            .component(component.clone())
            .run()
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
            "Extension '{}' has no lint/test scripts — skipping code quality checks",
            extension_id
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
/// Returns Ok(()) if validation passes, or Err if commits exist without entries.
/// When `dry_run` is true, reports what would be generated without writing to disk.
fn validate_commits_vs_changelog(component: &Component, dry_run: bool) -> Result<()> {
    // Get latest tag
    let latest_tag = git::get_latest_tag(&component.local_path)?;

    // Get commits since tag
    let commits = git::get_commits_since_tag(&component.local_path, latest_tag.as_deref())?;

    // If no commits, nothing to validate
    if commits.is_empty() {
        return Ok(());
    }

    // Read unreleased changelog entries
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let unreleased_entries =
        changelog::get_unreleased_entries(&changelog_content, &settings.next_section_aliases);

    let missing_commits = find_uncovered_commits(&commits, &unreleased_entries);

    // If all relevant commits are represented, validation passes.
    if missing_commits.is_empty() {
        return Ok(());
    }

    // Check if changelog is already finalized ahead of the latest tag
    // This handles cases where the changelog was manually finalized
    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content);
    if let (Some(latest_tag), Some(changelog_ver_str)) = (&latest_tag, latest_changelog_version) {
        let tag_version = latest_tag.trim_start_matches('v');
        if let (Ok(tag_ver), Ok(cl_ver)) = (
            semver::Version::parse(tag_version),
            semver::Version::parse(&changelog_ver_str),
        ) {
            // If changelog version is newer than tag, it's already finalized for pending changes
            if cl_ver > tag_ver {
                return Ok(());
            }
        }
    }

    // In dry-run mode, report what would be generated without writing to disk.
    if dry_run {
        let count = missing_commits
            .iter()
            .filter(|c| c.category.to_changelog_entry_type().is_some())
            .count();
        if count > 0 {
            log_status!(
                "release",
                "Would auto-generate {} changelog entries from commits (dry run)",
                count
            );
        }
        return Ok(());
    }

    // Auto-generate changelog entries only for uncovered commits.
    auto_generate_changelog_entries(component, &missing_commits)?;
    Ok(())
}

fn normalize_changelog_text(value: &str) -> String {
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
fn strip_pr_reference(value: &str) -> String {
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

fn find_uncovered_commits(
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
fn auto_generate_changelog_entries(
    component: &Component,
    commits: &[git::CommitInfo],
) -> Result<()> {
    let settings = changelog::resolve_effective_settings(Some(component));

    // Group commits by changelog entry type (skips docs, chore, merge)
    let mut entries_by_type: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();

    for commit in commits {
        if let Some(entry_type) = commit.category.to_changelog_entry_type() {
            let message = git::strip_conventional_prefix(&commit.subject);
            entries_by_type
                .entry(entry_type)
                .or_default()
                .push(message.to_string());
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
            .map(|c| git::strip_conventional_prefix(&c.subject).to_string())
            .unwrap_or_else(|| "Internal improvements".to_string());

        changelog::read_and_add_next_section_items_typed(
            component,
            &settings,
            &[fallback],
            "changed",
        )?;
        return Ok(());
    }

    // Add entries to changelog grouped by type
    for (entry_type, messages) in entries_by_type {
        changelog::read_and_add_next_section_items_typed(
            component, &settings, &messages, entry_type,
        )?;
    }

    Ok(())
}

/// Derive publish targets from extensions that have `release.publish` action.
fn get_publish_targets(extensions: &[ExtensionManifest]) -> Vec<String> {
    extensions
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.publish"))
        .map(|m| m.id.clone())
        .collect()
}

/// Check if any extension provides the `release.package` action.
fn has_package_capability(extensions: &[ExtensionManifest]) -> bool {
    extensions
        .iter()
        .any(|m| m.actions.iter().any(|a| a.id == "release.package"))
}

/// Build all release steps: core steps (non-configurable) + publish steps (extension-derived).
fn build_release_steps(
    component: &Component,
    extensions: &[ExtensionManifest],
    current_version: &str,
    new_version: &str,
    options: &ReleaseOptions,
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

    // 3. Git tag
    steps.push(ReleasePlanStep {
        id: "git.tag".to_string(),
        step_type: "git.tag".to_string(),
        label: Some(format!("Tag v{}", new_version)),
        needs: vec!["git.commit".to_string()],
        config: {
            let mut config = std::collections::HashMap::new();
            config.insert(
                "name".to_string(),
                serde_json::Value::String(format!("v{}", new_version)),
            );
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // 4. Git push (commits AND tags)
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

    // === PUBLISH STEPS (extension-derived, skipped with --skip-publish) ===

    if !publish_targets.is_empty() && !options.skip_publish {
        // 5. Package (produces artifacts for publish steps)
        steps.push(ReleasePlanStep {
            id: "package".to_string(),
            step_type: "package".to_string(),
            label: Some("Package release artifacts".to_string()),
            needs: vec!["git.push".to_string()],
            config: std::collections::HashMap::new(),
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });

        // 6. Publish steps (all run independently after package)
        let mut publish_step_ids: Vec<String> = Vec::new();
        for target in &publish_targets {
            let step_id = format!("publish.{}", target);
            let step_type = format!("publish.{}", target);

            publish_step_ids.push(step_id.clone());
            steps.push(ReleasePlanStep {
                id: step_id,
                step_type,
                label: Some(format!("Publish to {}", target)),
                needs: vec!["package".to_string()],
                config: std::collections::HashMap::new(),
                status: ReleasePlanStatus::Ready,
                missing: vec![],
            });
        }

        // 7. Cleanup step (runs after all publish steps)
        steps.push(ReleasePlanStep {
            id: "cleanup".to_string(),
            step_type: "cleanup".to_string(),
            label: Some("Clean up release artifacts".to_string()),
            needs: publish_step_ids,
            config: std::collections::HashMap::new(),
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    } else if options.skip_publish && !publish_targets.is_empty() {
        log_status!("release", "Skipping publish/package steps (--skip-publish)");
    }

    // === POST-RELEASE STEP (optional, runs after everything else, skipped with --skip-publish) ===
    let post_release_hooks =
        crate::hooks::resolve_hooks(component, crate::hooks::events::POST_RELEASE);
    if !post_release_hooks.is_empty() && !options.skip_publish {
        let post_release_needs = if !publish_targets.is_empty() {
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

/// Get list of files allowed to be dirty during release (relative paths).
fn get_release_allowed_files(
    changelog_path: &std::path::Path,
    version_targets: &[String],
    repo_root: &std::path::Path,
) -> Vec<String> {
    let mut allowed = Vec::new();

    // Add changelog (convert to relative path)
    if let Ok(relative) = changelog_path.strip_prefix(repo_root) {
        allowed.push(relative.to_string_lossy().to_string());
    }

    // Add version targets (convert to relative paths)
    for target in version_targets {
        if let Ok(relative) = std::path::Path::new(target).strip_prefix(repo_root) {
            let rel_str = relative.to_string_lossy().to_string();
            allowed.push(rel_str.clone());

            // If a Cargo.toml is a version target, also allow Cargo.lock
            // (version bump regenerates the lockfile to keep it in sync)
            if rel_str.ends_with("Cargo.toml") {
                let lock_path = relative.with_file_name("Cargo.lock");
                allowed.push(lock_path.to_string_lossy().to_string());
            }
        }
    }

    allowed
}

/// Get uncommitted files that are NOT in the allowed list.
fn get_unexpected_uncommitted_files(
    uncommitted: &UncommittedChanges,
    allowed: &[String],
) -> Vec<String> {
    let all_uncommitted: Vec<&String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .collect();

    all_uncommitted
        .into_iter()
        .filter(|f| !allowed.iter().any(|a| f.ends_with(a) || a.ends_with(*f)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{find_uncovered_commits, normalize_changelog_text, strip_pr_reference};
    use crate::git::{CommitCategory, CommitInfo};

    fn commit(subject: &str, category: CommitCategory) -> CommitInfo {
        CommitInfo {
            hash: "abc1234".to_string(),
            subject: subject.to_string(),
            category,
        }
    }

    #[test]
    fn test_normalize_changelog_text() {
        assert_eq!(
            normalize_changelog_text(
                "Fixed scoped audit exit codes to ignore unchanged legacy outliers"
            ),
            "fixed scoped audit exit codes to ignore unchanged legacy outliers"
        );
        assert_eq!(
            normalize_changelog_text("fix(audit): use scoped findings for changed-since exit"),
            "fix audit use scoped findings for changed since exit"
        );
    }

    #[test]
    fn test_find_uncovered_commits_ignores_covered_fix_commit() {
        let commits = vec![commit(
            "fix(audit): use scoped findings for changed-since exit",
            CommitCategory::Fix,
        )];
        let unreleased = vec![
            "use scoped findings for changed-since exit".to_string(),
            "another manual note".to_string(),
        ];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(uncovered.is_empty());
    }

    #[test]
    fn test_find_uncovered_commits_requires_feature_coverage() {
        let commits = vec![
            commit(
                "fix(audit): use scoped findings for changed-since exit",
                CommitCategory::Fix,
            ),
            commit(
                "feat(refactor): apply decompose plans with audit impact projection",
                CommitCategory::Feature,
            ),
        ];
        let unreleased = vec!["use scoped findings for changed-since exit".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert_eq!(uncovered.len(), 1);
        assert_eq!(
            uncovered[0].subject,
            "feat(refactor): apply decompose plans with audit impact projection"
        );
    }

    #[test]
    fn test_find_uncovered_commits_skips_docs_and_merge() {
        let commits = vec![
            commit("docs: update release notes", CommitCategory::Docs),
            commit("Merge pull request #1 from branch", CommitCategory::Merge),
        ];

        let uncovered = find_uncovered_commits(&commits, &[]);
        assert!(uncovered.is_empty());
    }

    #[test]
    fn test_strip_pr_reference() {
        assert_eq!(strip_pr_reference("fix something (#526)"), "fix something");
        assert_eq!(
            strip_pr_reference("feat: add feature (#123, #456)"),
            "feat: add feature"
        );
        assert_eq!(
            strip_pr_reference("no pr reference here"),
            "no pr reference here"
        );
        assert_eq!(
            strip_pr_reference("has parens (not a pr ref)"),
            "has parens (not a pr ref)"
        );
    }

    #[test]
    fn test_normalize_strips_pr_reference() {
        assert_eq!(
            normalize_changelog_text("fix something (#526)"),
            normalize_changelog_text("fix something")
        );
    }

    #[test]
    fn test_find_uncovered_commits_deduplicates_with_pr_suffix() {
        // Scenario: manual changelog entry without PR ref, commit has PR ref
        let commits = vec![commit(
            "fix: version bump dry-run no longer mutates changelog (#526)",
            CommitCategory::Fix,
        )];
        let unreleased = vec!["version bump dry-run no longer mutates changelog".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(
            uncovered.is_empty(),
            "Should detect commit as covered by manual entry (PR ref stripped)"
        );
    }

    #[test]
    fn test_find_uncovered_commits_bidirectional_match() {
        // Entry is longer/more descriptive than the commit message
        let commits = vec![commit("feat: enable autofix", CommitCategory::Feature)];
        let unreleased = vec!["enable autofix on PR and release CI workflows".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(
            uncovered.is_empty(),
            "Should match when commit subject is contained in the entry"
        );
    }
}
