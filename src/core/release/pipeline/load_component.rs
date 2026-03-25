//! load_component — extracted from pipeline.rs.

use crate::component::{self, Component};
use crate::engine::pipeline::{self, PipelineStep};
use crate::engine::validation::ValidationCollector;
use crate::error::{Error, Result};
use crate::git::{self, UncommittedChanges};
use crate::release::changelog;
use crate::version;
use super::super::executor::ReleaseStepExecutor;
use super::super::resolver::{resolve_extensions, ReleaseCapabilityResolver};
use super::super::types::{
    ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
    ReleaseSemverCommit, ReleaseSemverRecommendation,
};
use crate::engine::local_files::FileSystem;
use crate::engine::run_dir::{self, RunDir};
use crate::extension::{self, ExtensionManifest};
use super::get_release_allowed_files;
use super::build_semver_recommendation;
use super::get_unexpected_uncommitted_files;
use super::build_release_steps;
use super::validate_remote_sync;
use super::changelog_entries_to_json;
use super::validate_commits_vs_changelog;
use super::validate_changelog;
use super::validate_code_quality;


/// Load a component with portable config fallback when path_override is set.
/// In CI environments, the component may not be registered — only homeboy.json exists.
pub fn load_component(component_id: &str, options: &ReleaseOptions) -> Result<Component> {
    component::resolve_effective(Some(component_id), options.path_override.as_deref(), None)
}

/// Execute a release by computing the plan and executing it.
/// What you preview (dry-run) is what you execute.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    let release_plan = plan(component_id, options)?;

    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component, None)?;
    let resolver = ReleaseCapabilityResolver::new(extensions.clone());
    let executor = ReleaseStepExecutor::new(component_id.to_string(), component, extensions);

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
