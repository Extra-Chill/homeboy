//! Release pipeline — planning, validation, and straight-line execution.
//!
//! Previously this module drove execution through a generic DAG pipeline in
//! `engine::pipeline` with a trait-dispatched `ReleaseStepExecutor` and a
//! capability `ReleaseCapabilityResolver`. In practice every release ran the
//! same sequential order through a `Mutex<ReleaseContext>` shared between
//! steps — the DAG bought nothing but indirection. See issue #1187.
//!
//! The layers collapsed into one function (`execute`) that calls the step
//! implementations in [`super::executor`] directly. The `plan()` function
//! still returns a serializable `ReleasePlan` for `--dry-run` / `--json`
//! consumers, but it's now a *description* of what `execute` will do, not
//! the thing that drives execution.

use crate::component::{self, Component};
use crate::engine::local_files::FileSystem;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::validation::ValidationCollector;
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};
use crate::git::{self, UncommittedChanges};
use crate::release::changelog;
use crate::version;

use super::executor;
use super::types::{
    ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun, ReleaseRunResult,
    ReleaseRunSummary, ReleaseSemverCommit, ReleaseSemverRecommendation, ReleaseState,
    ReleaseStepResult, ReleaseStepStatus,
};

/// Load a component with portable config fallback when path_override is set.
/// In CI environments, the component may not be registered — only homeboy.json exists.
pub(crate) fn load_component(component_id: &str, options: &ReleaseOptions) -> Result<Component> {
    component::resolve_effective(Some(component_id), options.path_override.as_deref(), None)
}

/// Resolve the component's declared extensions (for publish/package dispatch).
fn resolve_extensions(component: &Component) -> Result<Vec<ExtensionManifest>> {
    let mut extensions = Vec::new();
    if let Some(configured) = component.extensions.as_ref() {
        let mut extension_ids: Vec<String> = configured.keys().cloned().collect();
        extension_ids.sort();
        let suggestions = extension::available_extension_ids();
        for extension_id in extension_ids {
            let manifest = extension::load_extension(&extension_id).map_err(|_| {
                Error::extension_not_found(extension_id.to_string(), suggestions.clone())
            })?;
            extensions.push(manifest);
        }
    }
    Ok(extensions)
}

/// Execute a release end-to-end.
///
/// Runs the preflight validations (via [`plan`]), then walks the release
/// steps in order, threading [`ReleaseState`] between them. Steps that fail
/// cause subsequent steps to be marked `Skipped` but execution continues so
/// the caller gets a full per-step result list; post-release hooks still
/// run so any failure can be observed.
///
/// What you preview with `--dry-run` is what executes.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    // plan() performs all validations + bootstraps. Its output also tells us
    // which publish/cleanup/post_release steps should run for this component,
    // so we don't duplicate the capability checks below.
    let release_plan = plan(component_id, options)?;

    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component)?;
    let monorepo = git::MonorepoContext::detect(&component.local_path, component_id);
    let pending_entries = extract_pending_entries(&release_plan);

    let mut state = ReleaseState::default();
    let mut results: Vec<ReleaseStepResult> = Vec::new();

    // Helper: return early if the last step we pushed failed. Tag/push failures
    // are genuinely show-stopping for the release so we bail; publish/github
    // failures are handled below with their own per-target logic.
    macro_rules! bail_on_failure {
        () => {
            if matches!(
                results.last().map(|r| &r.status),
                Some(ReleaseStepStatus::Failed)
            ) {
                return Ok(finalize(component_id, results, monorepo.as_ref()));
            }
        };
    }

    // 1. Version bump (+ optional changelog generation)
    results.push(executor::run_version(
        &component,
        &mut state,
        &options.bump_type,
        pending_entries.as_ref(),
    )?);
    bail_on_failure!();

    // 2. Git commit
    results.push(executor::run_git_commit(&component, component_id, &state)?);
    bail_on_failure!();

    // 3. Optional packaging (runs BEFORE tag so build failures don't leave
    //    orphan tags on the remote).
    let has_publish_targets = !get_publish_targets(&extensions).is_empty();
    let want_publish = !options.skip_publish && has_publish_targets;
    if want_publish {
        match executor::run_package(
            &extensions,
            &mut state,
            component_id,
            &component.local_path,
        ) {
            Ok(result) => results.push(result),
            Err(err) => results.push(failed_result("package", "package", err)),
        }
        bail_on_failure!();
    }

    // 4. Git tag
    let tag_name = match monorepo.as_ref() {
        Some(ctx) => ctx.format_tag(state.version.as_deref().unwrap_or("")),
        None => format!("v{}", state.version.as_deref().unwrap_or("")),
    };
    results.push(executor::run_git_tag(
        &component,
        component_id,
        &mut state,
        &tag_name,
    )?);
    bail_on_failure!();

    // 5. Git push (commits + tags)
    results.push(executor::run_git_push(&component, component_id)?);
    bail_on_failure!();

    // 6. GitHub Release (soft-fails on gh issues; skipped for non-GitHub remotes).
    if !options.skip_github_release && github_release_applies(&component) {
        match executor::run_github_release(&component, &state) {
            Ok(result) => results.push(result),
            Err(err) => results.push(failed_result("github.release", "github.release", err)),
        }
    }

    // 7. Publish to each configured target. Failures here mark the step failed
    //    but don't halt — other targets may still succeed.
    let mut publish_failed = false;
    if want_publish {
        for target in get_publish_targets(&extensions) {
            match executor::run_publish(
                &extensions,
                &state,
                component_id,
                &component.local_path,
                &target,
            ) {
                Ok(result) => {
                    if matches!(result.status, ReleaseStepStatus::Failed) {
                        publish_failed = true;
                    }
                    results.push(result);
                }
                Err(err) => {
                    publish_failed = true;
                    let step_id = format!("publish.{}", target);
                    results.push(failed_result(&step_id, &step_id, err));
                }
            }
        }
    }

    // 8. Cleanup staging dir. Skipped when --deploy is set (deploy needs the
    //    build artifact) and when publishing was skipped entirely.
    if want_publish && !options.deploy && !publish_failed {
        match executor::run_cleanup(&component) {
            Ok(result) => results.push(result),
            Err(err) => results.push(failed_result("cleanup", "cleanup", err)),
        }
    }

    // 9. Post-release hooks (always run if configured — not gated by --skip-publish).
    let post_release_hooks =
        crate::engine::hooks::resolve_hooks(&component, crate::engine::hooks::events::POST_RELEASE);
    if !post_release_hooks.is_empty() {
        match executor::run_post_release(&component, &post_release_hooks) {
            Ok(result) => results.push(result),
            Err(err) => results.push(failed_result("post_release", "post_release", err)),
        }
    }

    Ok(finalize(component_id, results, monorepo.as_ref()))
}

/// Convert a step error into a failed `ReleaseStepResult`.
fn failed_result(id: &str, step_type: &str, err: Error) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Failed,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: err.hints.clone(),
        data: Some(serde_json::json!({ "error_details": err.details })),
        error: Some(err.message),
    }
}

/// Read the auto-generated changelog entries embedded in the plan's `version`
/// step. `plan()` computes them during validation and stashes them here so
/// `execute()` can hand them straight to [`executor::run_version`] without
/// recomputing.
fn extract_pending_entries(
    plan: &ReleasePlan,
) -> Option<std::collections::HashMap<String, Vec<String>>> {
    let version_step = plan.steps.iter().find(|s| s.id == "version")?;
    let value = version_step.config.get("changelog_entries")?;
    serde_json::from_value(value.clone()).ok()
}

/// Wrap the accumulated step results into a `ReleaseRun` with an overall
/// status and a human-friendly summary.
fn finalize(
    component_id: &str,
    results: Vec<ReleaseStepResult>,
    _monorepo: Option<&git::MonorepoContext>,
) -> ReleaseRun {
    let status = derive_overall_status(&results);
    let summary = build_summary(&results, &status);

    ReleaseRun {
        component_id: component_id.to_string(),
        enabled: true,
        result: ReleaseRunResult {
            steps: results,
            status,
            warnings: Vec::new(),
            summary: Some(summary),
        },
    }
}

fn derive_overall_status(results: &[ReleaseStepResult]) -> ReleaseStepStatus {
    let has_success = results
        .iter()
        .any(|r| matches!(r.status, ReleaseStepStatus::Success));
    let has_failed = results
        .iter()
        .any(|r| matches!(r.status, ReleaseStepStatus::Failed));

    if has_failed && has_success {
        ReleaseStepStatus::PartialSuccess
    } else if has_failed {
        ReleaseStepStatus::Failed
    } else {
        ReleaseStepStatus::Success
    }
}

fn build_summary(
    results: &[ReleaseStepResult],
    status: &ReleaseStepStatus,
) -> ReleaseRunSummary {
    let succeeded = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Success))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Failed))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Skipped))
        .count();
    let missing = results
        .iter()
        .filter(|r| matches!(r.status, ReleaseStepStatus::Missing))
        .count();

    let next_actions = match status {
        ReleaseStepStatus::PartialSuccess | ReleaseStepStatus::Failed => vec![
            "Fix the issue and re-run (idempotent - completed steps will succeed again)"
                .to_string(),
        ],
        ReleaseStepStatus::Missing => {
            vec!["Install missing extensions or actions to resolve missing steps".to_string()]
        }
        _ => Vec::new(),
    };

    let success_summary = if matches!(status, ReleaseStepStatus::Success) {
        results.iter().filter_map(build_step_summary_line).collect()
    } else {
        Vec::new()
    };

    ReleaseRunSummary {
        total_steps: results.len(),
        succeeded,
        failed,
        skipped,
        missing,
        next_actions,
        success_summary,
    }
}

fn build_step_summary_line(result: &ReleaseStepResult) -> Option<String> {
    if !matches!(result.status, ReleaseStepStatus::Success) {
        return None;
    }

    let data = result.data.as_ref();

    match result.step_type.as_str() {
        "version" => data
            .and_then(|d| d.get("new_version"))
            .and_then(|v| v.as_str())
            .map(|ver| format!("Version bumped to {}", ver)),
        "git.commit" => {
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                Some("Working tree was clean".to_string())
            } else {
                Some("Committed release changes".to_string())
            }
        }
        "git.tag" => {
            let tag = data.and_then(|d| d.get("tag")).and_then(|v| v.as_str());
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match (tag, skipped) {
                (Some(t), true) => Some(format!("Tag {} already exists", t)),
                (Some(t), false) => Some(format!("Tagged {}", t)),
                (None, _) => Some("Tagged release".to_string()),
            }
        }
        "git.push" => Some("Pushed to origin (with tags)".to_string()),
        "package" => Some("Created release artifacts".to_string()),
        "cleanup" => None,
        "github.release" => {
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                None
            } else {
                data.and_then(|d| d.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|url| format!("Created GitHub Release: {}", url))
            }
        }
        "post_release" => {
            let all_succeeded = data
                .and_then(|d| d.get("all_succeeded"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if all_succeeded {
                Some("Post-release commands completed".to_string())
            } else {
                Some("Post-release commands completed (with warnings)".to_string())
            }
        }
        step if step.starts_with("publish.") => {
            let target = step.strip_prefix("publish.").unwrap_or("registry");
            Some(format!("Published to {}", target))
        }
        _ => None,
    }
}



/// Plan a release: run all preflight validations, then return a description
/// of the steps the executor will run. Used by `--dry-run` to preview work
/// without side effects and by [`run`] to drive validation + auto-generated
/// changelog entries.
///
/// Requires a clean working tree (uncommitted changes cause an error).
///
/// Core steps (always generated):
/// 1. Version bump + changelog finalization
/// 2. Git commit
/// 3. Git tag
/// 4. Git push (commits AND tags)
///
/// Extension-derived steps, added when applicable:
/// - `package` — component has an extension with `release.package` action
/// - `publish.<target>` — one per extension with `release.publish` action
/// - `cleanup` — after publish (skipped with `--deploy`)
/// - `github.release` — component's remote resolves to a github.com URL
/// - `post_release` — component defines post-release hook commands
pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component)?;

    let mut v = ValidationCollector::new();

    // === Stage 0: Working-tree check (fail-fast) ===
    //
    // Run this BEFORE remote sync (which fast-forwards and mutates the tree)
    // and BEFORE the lint/test gate (which can dump tens of thousands of lines
    // to stdout, drowning out the real reason a release was blocked).
    //
    // The full file-list comparison still happens in Stage 3 once we know the
    // resolved changelog and version-target paths — at this stage we only know
    // there's *some* dirty file we can't account for, but that's enough to
    // bail before doing expensive work. We filter out homeboy-managed
    // scratch paths (.homeboy-build/, .homeboy-bin/, .homeboy/) here too so
    // noisy build artifacts don't trigger the early exit.
    v.capture(validate_working_tree_fail_fast(&component), "working_tree");
    v.finish_if_errors()?;

    // === Stage 0.5: Remote sync check (preflight) ===
    v.capture(validate_remote_sync(&component), "remote_sync");

    // === Stage 0.6: Code quality checks (lint + test) ===
    if options.skip_checks {
        log_status!("release", "Skipping code quality checks (--skip-checks)");
    } else {
        v.capture(validate_code_quality(&component), "code_quality");
    }

    // === Stage 0.7: Auto-initialize changelog (first-release bootstrap) ===
    //
    // If `changelog_target` is configured but the file doesn't exist on disk,
    // synthesize a minimal changelog so downstream stages have something to
    // read and finalize. Without this, three stages below all fail with
    // "File not found" for the same root cause and none of their hints
    // mention `homeboy changelog init`. See #1172.
    //
    // Skipped in dry-run to avoid mutating the working tree during a preview.
    if !options.dry_run {
        v.capture(
            ensure_changelog_initialized(&component),
            "changelog_bootstrap",
        );
    }

    // Detect monorepo context for path-scoped commits and component-prefixed tags.
    let monorepo = git::MonorepoContext::detect(&component.local_path, component_id);

    // Build semver recommendation from commits early so both JSON output and
    // validation paths share a single source of truth.
    let semver_recommendation =
        build_semver_recommendation(&component, &options.bump_type, monorepo.as_ref())?;

    // === Stage 1: Determine changelog entries from conventional commits ===
    // Returns Some(entries) when commits need changelog entries generated.
    // Never writes to disk — entries are passed to the executor via step config.
    // When auto-generation will handle the changelog, skip downstream changelog
    // validations (they would false-fail since entries don't exist on disk yet).
    let pending_entries = v
        .capture(
            validate_commits_vs_changelog(&component, options.dry_run, monorepo.as_ref()),
            "commits",
        )
        .flatten();
    let will_auto_generate = pending_entries.is_some();

    if !will_auto_generate {
        v.capture(validate_changelog(&component), "changelog");
    }
    let version_info = v.capture(version::read_component_version(&component), "version");

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

    if !will_auto_generate {
        if let (Some(ref info), Some(ref new_ver)) = (&version_info, &new_version) {
            v.capture(
                version::validate_changelog_for_bump(&component, &info.version, new_ver),
                "changelog_sync",
            );
        }
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

    let mut steps = build_release_steps(
        &component,
        &extensions,
        &version_info.version,
        &new_version,
        options,
        monorepo.as_ref(),
        &mut warnings,
        &mut hints,
    )?;

    // Embed pending changelog entries in the version step config so the executor
    // can generate and finalize them atomically — no ## Unreleased disk round-trip.
    if let Some(ref entries) = pending_entries {
        if let Some(version_step) = steps.iter_mut().find(|s| s.id == "version") {
            version_step.config.insert(
                "changelog_entries".to_string(),
                changelog_entries_to_json(entries),
            );
        }
    }

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
                git::CommitCategory::Release => "release",
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

fn validate_changelog(component: &Component) -> Result<()> {
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
fn validate_remote_sync(component: &Component) -> Result<()> {
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
fn validate_code_quality(component: &Component) -> Result<()> {
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
fn validate_commits_vs_changelog(
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
/// Group conventional commits into changelog entries by type.
/// Returns a map of entry_type -> messages (e.g. "added" -> ["feature X", "feature Y"]).
/// Pure function — no I/O. Skips docs, chore, and merge commits.
fn group_commits_for_changelog(
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

    // If no entries generated (all docs/chore/merge/release), use first non-skip commit or fallback
    if entries_by_type.is_empty() {
        let fallback = commits
            .iter()
            .find(|c| {
                !matches!(
                    c.category,
                    git::CommitCategory::Docs
                        | git::CommitCategory::Chore
                        | git::CommitCategory::Merge
                        | git::CommitCategory::Release
                )
            })
            .map(|c| strip_pr_reference(git::strip_conventional_prefix(&c.subject)))
            .unwrap_or_else(|| "Internal improvements".to_string());

        entries_by_type.insert("changed".to_string(), vec![fallback]);
    }

    entries_by_type
}

/// Serialize changelog entries to JSON for embedding in step config.
fn changelog_entries_to_json(
    entries: &std::collections::HashMap<String, Vec<String>>,
) -> serde_json::Value {
    serde_json::to_value(entries).unwrap_or_default()
}

/// Return true if this component should get a GitHub Release created.
///
/// Resolves the remote URL from the component config (preferred) or from
/// `git remote get-url origin` in the component's local_path, then parses
/// it as a GitHub URL. Non-GitHub remotes (GitLab, self-hosted, etc.) fall
/// through cleanly — the step simply isn't added to the plan.
fn github_release_applies(component: &Component) -> bool {
    let remote_url = component
        .remote_url
        .clone()
        .or_else(|| {
            crate::deploy::release_download::detect_remote_url(std::path::Path::new(
                &component.local_path,
            ))
        });

    remote_url
        .as_deref()
        .and_then(crate::deploy::release_download::parse_github_url)
        .is_some()
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

    // 5a. GitHub Release (create tag+notes on github.com)
    // Runs in parallel with publish/cleanup — it only needs the tag to be on
    // the remote. Fails soft when `gh` isn't installed or authenticated.
    // Skipped entirely for non-GitHub remotes (no remote_url, or non-github URL).
    if !options.skip_github_release && github_release_applies(component) {
        steps.push(ReleasePlanStep {
            id: "github.release".to_string(),
            step_type: "github.release".to_string(),
            label: Some("Create GitHub Release".to_string()),
            needs: vec!["git.push".to_string()],
            config: std::collections::HashMap::new(),
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    }

    let mut publish_step_ids: Vec<String> = Vec::new();

    if !publish_targets.is_empty() && !options.skip_publish {
        // 6. Publish steps (all run independently after git.push)
        // Package already ran before tagging; publish needs the push to have
        // completed (e.g., crates.io/Homebrew need the tag on the remote).
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
                needs: publish_step_ids.clone(),
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
            if options.deploy {
                // When --deploy is set, cleanup was removed from the pipeline
                // (deploy needs the build artifact). Depend on the last publish
                // steps directly.
                publish_step_ids.clone()
            } else {
                vec!["cleanup".to_string()]
            }
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

/// Path prefixes that are always treated as homeboy-managed scratch space
/// and should never count as "dirty" during release/version-bump checks.
///
/// These are tool-owned artifacts a release should be able to leave on disk
/// without blocking the next run. Components are not required to gitignore
/// them (and many don't), so the release pipeline filters them out itself.
///
/// Currently:
/// - `.homeboy-build/` — build staging directory
/// - `.homeboy-bin/` — locally-built binaries
/// - `.homeboy/` — generic per-repo scratch directory
const HOMEBOY_MANAGED_PREFIXES: &[&str] = &[
    ".homeboy-build/",
    ".homeboy-build",
    ".homeboy-bin/",
    ".homeboy-bin",
    ".homeboy/",
    ".homeboy",
];

/// Returns true if a repo-relative path lives under homeboy-managed scratch space.
fn is_homeboy_managed_path(rel_path: &str) -> bool {
    HOMEBOY_MANAGED_PREFIXES
        .iter()
        .any(|prefix| rel_path == *prefix || rel_path.starts_with(prefix))
}

/// Filter out homeboy-managed scratch paths from a list of uncommitted files.
fn filter_homeboy_managed(files: Vec<String>) -> Vec<String> {
    files
        .into_iter()
        .filter(|f| !is_homeboy_managed_path(f))
        .collect()
}

/// Stage 0 fail-fast: refuse to run any release work when the working tree
/// has unexplained dirty files.
///
/// At this stage we don't yet know the resolved changelog path or version-target
/// paths (Stage 3 does the precise allow-list comparison), so we conservatively
/// allow only homeboy-managed scratch space. If anything else is dirty we bail
/// before lint/test/build can dump tens of thousands of lines and drown out
/// the real error.
fn validate_working_tree_fail_fast(component: &Component) -> Result<()> {
    let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
    if !uncommitted.has_changes {
        return Ok(());
    }

    let all_files: Vec<String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .cloned()
        .collect();

    let unexpected = filter_homeboy_managed(all_files);
    if unexpected.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "working_tree",
        "Uncommitted changes detected — refusing to release",
        None,
        Some(vec![
            "Commit, stash, or discard changes before releasing".to_string(),
            format!(
                "Unexpected dirty files ({}): {}{}",
                unexpected.len(),
                unexpected
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if unexpected.len() > 10 { ", …" } else { "" }
            ),
        ]),
    ))
}

/// First-release bootstrap: if the component's configured `changelog_target`
/// doesn't exist on disk (and no fallback candidate exists either), synthesize
/// a minimal changelog so the downstream release stages have a file to read
/// and finalize.
///
/// Without this, three stages below all fail with the same `File not found`
/// error — `commits`, `changelog`, and `changelog_sync` each read the
/// changelog independently — and none of their hints point at
/// `homeboy changelog init`. See #1172.
///
/// Writes a template matching `homeboy changelog init`: a `# Changelog` title
/// and a `## Unreleased` section. The downstream `finalize_with_generated_entries`
/// or `finalize_next_section` then replaces `## Unreleased` with the real
/// `## [x.y.z] - YYYY-MM-DD` section, so the template is transient — it
/// lands on disk only for the span of one release run.
///
/// No-op when:
/// - `changelog_target` is unset (resolver already fails with a teaching error),
/// - the configured path exists,
/// - a fallback candidate exists (resolver's discovery covers this case).
fn ensure_changelog_initialized(component: &Component) -> Result<()> {
    let Some(ref target) = component.changelog_target else {
        // No target configured — let resolve_changelog_path() emit its
        // existing "No changelog configured" teaching error downstream.
        return Ok(());
    };

    let configured_path = crate::paths::resolve_path(&component.local_path, target);
    if configured_path.exists() {
        return Ok(());
    }

    // If a fallback candidate exists, resolve_changelog_path() will pick
    // it up — don't overwrite it with a fresh template.
    let repo_root = std::path::Path::new(&component.local_path);
    if changelog::discover_changelog_relative_path(repo_root).is_some() {
        return Ok(());
    }

    // Create the parent directory (e.g. `docs/`) before writing the file.
    if let Some(parent) = configured_path.parent() {
        crate::engine::local_files::local().ensure_dir(parent)?;
    }

    let settings = changelog::resolve_effective_settings(Some(component));
    let template = format!("# Changelog\n\n## {}\n\n", settings.next_section_label,);

    crate::engine::local_files::local().write(&configured_path, &template)?;

    log_status!(
        "release",
        "Initialized changelog at {} (first release for {})",
        configured_path.display(),
        component.id
    );

    Ok(())
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
///
/// Homeboy-managed scratch paths (`.homeboy-build/`, `.homeboy-bin/`, etc.)
/// are filtered out here too, mirroring the Stage 0 fail-fast filter.
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
        .filter(|f| !is_homeboy_managed_path(f))
        .filter(|f| !allowed.iter().any(|a| f.ends_with(a) || a.ends_with(*f)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_changelog_initialized, filter_homeboy_managed, find_uncovered_commits,
        get_unexpected_uncommitted_files, is_homeboy_managed_path, normalize_changelog_text,
        strip_pr_reference,
    };
    use crate::component::Component;
    use crate::git::{CommitCategory, CommitInfo, UncommittedChanges};

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

    #[test]
    fn test_group_commits_strips_conventional_prefix_with_issue_scope() {
        use super::group_commits_for_changelog;

        let commits = vec![
            commit(
                "feat(#741): delete AgentType class — replace with string literals",
                CommitCategory::Feature,
            ),
            commit(
                "fix(#730): queue-add uses unified check-duplicate",
                CommitCategory::Fix,
            ),
        ];

        let entries = group_commits_for_changelog(&commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(
            added[0],
            "delete AgentType class — replace with string literals"
        );
        assert_eq!(fixed[0], "queue-add uses unified check-duplicate");
    }

    // ---- homeboy-managed scratch path filtering (issue #1162) ----

    #[test]
    fn homeboy_build_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy-build/artifact.zip"));
        assert!(is_homeboy_managed_path(".homeboy-build/"));
        assert!(is_homeboy_managed_path(".homeboy-build"));
    }

    #[test]
    fn homeboy_bin_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy-bin/homeboy"));
        assert!(is_homeboy_managed_path(".homeboy-bin"));
    }

    #[test]
    fn homeboy_scratch_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy/cache"));
    }

    #[test]
    fn user_paths_are_not_managed() {
        assert!(!is_homeboy_managed_path("src/main.rs"));
        assert!(!is_homeboy_managed_path("docs/changelog.md"));
        assert!(!is_homeboy_managed_path("homeboy.json"));
        assert!(!is_homeboy_managed_path(".gitignore"));
        // Defensive — a file that merely contains the string should not match.
        assert!(!is_homeboy_managed_path("src/.homeboy-build/foo"));
    }

    #[test]
    fn filter_homeboy_managed_drops_only_managed_paths() {
        let files = vec![
            ".homeboy-build/artifact.zip".to_string(),
            "src/main.rs".to_string(),
            ".homeboy-bin/homeboy".to_string(),
            "Cargo.toml".to_string(),
        ];
        let filtered = filter_homeboy_managed(files);
        assert_eq!(filtered, vec!["src/main.rs", "Cargo.toml"]);
    }

    fn uncommitted(staged: &[&str], unstaged: &[&str], untracked: &[&str]) -> UncommittedChanges {
        UncommittedChanges {
            has_changes: !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty(),
            staged: staged.iter().map(|s| s.to_string()).collect(),
            unstaged: unstaged.iter().map(|s| s.to_string()).collect(),
            untracked: untracked.iter().map(|s| s.to_string()).collect(),
            hint: None,
        }
    }

    /// The headline regression for #1162: a stale `.homeboy-build/` directory
    /// from a previous build must not block the next release.
    #[test]
    fn unexpected_files_skip_homeboy_build_dir() {
        let changes = uncommitted(&[], &[], &[".homeboy-build/data-machine-0.70.1.zip"]);
        let unexpected = get_unexpected_uncommitted_files(&changes, &[]);
        assert!(
            unexpected.is_empty(),
            "homeboy-managed scratch should never trigger working_tree error, got: {:?}",
            unexpected
        );
    }

    #[test]
    fn unexpected_files_still_catch_user_changes() {
        let changes = uncommitted(&["src/lib.rs"], &[], &[".homeboy-build/foo"]);
        let unexpected = get_unexpected_uncommitted_files(&changes, &[]);
        assert_eq!(unexpected, vec!["src/lib.rs"]);
    }

    #[test]
    fn unexpected_files_honor_allowed_list_alongside_homeboy_filter() {
        let changes = uncommitted(
            &["docs/changelog.md", "Cargo.toml"],
            &[],
            &[".homeboy-build/foo"],
        );
        let allowed = vec!["docs/changelog.md".to_string(), "Cargo.toml".to_string()];
        let unexpected = get_unexpected_uncommitted_files(&changes, &allowed);
        assert!(
            unexpected.is_empty(),
            "allowed files + homeboy scratch should yield clean result, got: {:?}",
            unexpected
        );
    }

    fn component_with_changelog_target(
        temp_dir: &tempfile::TempDir,
        target: Option<&str>,
    ) -> Component {
        Component {
            id: "test-component".to_string(),
            local_path: temp_dir.path().to_string_lossy().to_string(),
            remote_path: String::new(),
            changelog_target: target.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    /// Regression for #1172: first release with `changelog_target` configured
    /// but no file on disk. The preflight must create the file so downstream
    /// stages don't all fail with "File not found".
    #[test]
    fn ensure_changelog_initialized_creates_missing_file() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));

        let changelog_path = temp.path().join("CHANGELOG.md");
        assert!(!changelog_path.exists(), "precondition: no changelog yet");

        ensure_changelog_initialized(&component).expect("preflight should bootstrap");

        let content = std::fs::read_to_string(&changelog_path).expect("file created");
        assert!(content.contains("# Changelog"), "has title: {}", content);
        assert!(
            content.contains("## Unreleased"),
            "has unreleased: {}",
            content
        );
    }

    /// Nested targets like `docs/CHANGELOG.md` must have the parent directory
    /// created before the file is written.
    #[test]
    fn ensure_changelog_initialized_creates_parent_dir_for_nested_target() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("docs/CHANGELOG.md"));

        let docs_dir = temp.path().join("docs");
        assert!(!docs_dir.exists(), "precondition: no docs/ yet");

        ensure_changelog_initialized(&component).expect("preflight should bootstrap");

        assert!(docs_dir.is_dir(), "docs/ parent should be created");
        assert!(
            temp.path().join("docs/CHANGELOG.md").exists(),
            "changelog should land at docs/CHANGELOG.md"
        );
    }

    /// Idempotent: if the configured path already exists, leave it alone.
    /// A second release run must not overwrite an existing changelog.
    #[test]
    fn ensure_changelog_initialized_leaves_existing_file_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));
        let changelog_path = temp.path().join("CHANGELOG.md");
        let original = "# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Added\n- real release\n";
        std::fs::write(&changelog_path, original).unwrap();

        ensure_changelog_initialized(&component).expect("no-op on existing file");

        let after = std::fs::read_to_string(&changelog_path).unwrap();
        assert_eq!(after, original, "existing changelog must not be rewritten");
    }

    /// If a fallback candidate (e.g. `docs/CHANGELOG.md`) exists but the
    /// configured target points elsewhere, prefer the fallback (the resolver
    /// already does this) instead of creating a second changelog alongside.
    #[test]
    fn ensure_changelog_initialized_defers_to_existing_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));

        // Create a fallback candidate at a different location.
        std::fs::create_dir_all(temp.path().join("docs")).unwrap();
        let fallback = temp.path().join("docs/CHANGELOG.md");
        std::fs::write(&fallback, "# Changelog\n\n## [0.1.0] - 2026-01-01\n").unwrap();

        ensure_changelog_initialized(&component).expect("defer to fallback");

        // The configured target should NOT have been created — the resolver
        // will pick up the fallback instead.
        assert!(
            !temp.path().join("CHANGELOG.md").exists(),
            "should not create duplicate when fallback exists"
        );
    }

    /// If no `changelog_target` is configured at all, the preflight is a
    /// no-op — `resolve_changelog_path()` downstream emits its existing
    /// "No changelog configured" teaching error.
    #[test]
    fn ensure_changelog_initialized_is_noop_without_configured_target() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, None);

        ensure_changelog_initialized(&component).expect("no-op without target");

        // No file created.
        for entry in std::fs::read_dir(temp.path()).unwrap() {
            let path = entry.unwrap().path();
            panic!("should have created nothing, but found: {}", path.display());
        }
    }

    #[test]
    fn test_group_commits_strips_pr_references() {
        use super::group_commits_for_changelog;

        let commits = vec![
            commit(
                "feat: agent-first scoping — Phase 1 schema (#738)",
                CommitCategory::Feature,
            ),
            commit(
                "fix: rename $class param — fixes bootstrap crash (#711)",
                CommitCategory::Fix,
            ),
        ];

        let entries = group_commits_for_changelog(&commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(added[0], "agent-first scoping — Phase 1 schema");
        assert_eq!(fixed[0], "rename $class param — fixes bootstrap crash");
    }
}
