use crate::changelog;
use crate::component::{self, Component};
use crate::core::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::git::{self, UncommittedChanges};
use crate::module::ModuleManifest;
use crate::engine::pipeline::{self, PipelineStep};
use crate::utils::validation::ValidationCollector;
use crate::version;

use super::executor::ReleaseStepExecutor;
use super::resolver::{resolve_modules, ReleaseCapabilityResolver};
use super::types::{ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun};

/// Execute a release by computing the plan and executing it.
/// What you preview (dry-run) is what you execute.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    let release_plan = plan(component_id, options)?;

    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, None)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let executor = ReleaseStepExecutor::new(component_id.to_string(), modules);

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

/// Plan a release with built-in core steps and module-derived publish targets.
///
/// Requires a clean working tree (uncommitted changes will cause an error).
///
/// Core steps (always generated, non-configurable):
/// 1. Version bump + changelog finalization
/// 2. Git commit
/// 3. Git tag
/// 4. Git push (commits AND tags)
///
/// Publish steps (derived from modules):
/// - From component's modules that have `release.publish` action
/// - Or explicit `release.publish` array if configured
pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, None)?;

    let mut v = ValidationCollector::new();

    // === Stage 0: Remote sync check (preflight) ===
    v.capture(validate_remote_sync(&component), "remote_sync");

    // === Stage 1: Independent validations ===
    v.capture(validate_commits_vs_changelog(&component), "commits");
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
            } else if uncommitted.has_changes {
                // Only changelog/version files are uncommitted — auto-stage them
                // so the release commit includes them (e.g., after `homeboy changelog add`)
                eprintln!("[release] Auto-staging changelog/version files for release commit");
                let all_files: Vec<&String> = uncommitted.staged.iter()
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

    // All validations passed - unwrap safely
    let version_info = version_info.unwrap();
    let new_version = new_version.unwrap();

    let mut warnings = Vec::new();
    let mut hints = Vec::new();

    let steps = build_release_steps(
        &component,
        &modules,
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
        warnings,
        hints,
    })
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

/// Validate that commits since the last tag have corresponding changelog entries.
/// Returns Ok(()) if validation passes, or Err if commits exist without entries.
fn validate_commits_vs_changelog(component: &Component) -> Result<()> {
    // Get latest tag
    let latest_tag = git::get_latest_tag(&component.local_path)?;

    // Get commits since tag
    let commits = git::get_commits_since_tag(&component.local_path, latest_tag.as_deref())?;

    // If no commits, nothing to validate
    if commits.is_empty() {
        return Ok(());
    }

    // Count unreleased changelog entries
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let entry_count =
        changelog::count_unreleased_entries(&changelog_content, &settings.next_section_aliases);

    // If entries exist, validation passes
    if entry_count > 0 {
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

    // Auto-generate changelog entries from conventional commits
    auto_generate_changelog_entries(component, &commits)?;
    Ok(())
}

/// Generate changelog entries from conventional commit messages.
fn auto_generate_changelog_entries(component: &Component, commits: &[git::CommitInfo]) -> Result<()> {
    let settings = changelog::resolve_effective_settings(Some(component));

    // Group commits by changelog entry type (skips docs, chore, merge)
    let mut entries_by_type: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();

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
            .find(|c| !matches!(
                c.category,
                git::CommitCategory::Docs | git::CommitCategory::Chore | git::CommitCategory::Merge
            ))
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
            component,
            &settings,
            &messages,
            entry_type,
        )?;
    }

    Ok(())
}

/// Derive publish targets from modules that have `release.publish` action.
fn get_publish_targets(modules: &[ModuleManifest]) -> Vec<String> {
    modules
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.publish"))
        .map(|m| m.id.clone())
        .collect()
}

/// Check if any module provides the `release.package` action.
fn has_package_capability(modules: &[ModuleManifest]) -> bool {
    modules
        .iter()
        .any(|m| m.actions.iter().any(|a| a.id == "release.package"))
}

/// Build all release steps: core steps (non-configurable) + publish steps (module-derived).
fn build_release_steps(
    component: &Component,
    modules: &[ModuleManifest],
    current_version: &str,
    new_version: &str,
    options: &ReleaseOptions,
    warnings: &mut Vec<String>,
    _hints: &mut Vec<String>,
) -> Result<Vec<ReleasePlanStep>> {
    let mut steps = Vec::new();
    let publish_targets = get_publish_targets(modules);

    // === WARNING: No package capability ===
    if !publish_targets.is_empty() && !has_package_capability(modules) {
        warnings.push(
            "Publish targets derived from modules but no module provides 'release.package'. \
             Add a module like 'rust' that provides packaging."
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

    // === PUBLISH STEPS (module-derived, only if publish targets exist) ===

    if !publish_targets.is_empty() {
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
    }

    // === POST-RELEASE STEP (optional, runs after everything else) ===
    if !component.post_release_commands.is_empty() {
        let post_release_needs = if !publish_targets.is_empty() {
            vec!["cleanup".to_string()]
        } else {
            vec!["git.push".to_string()]
        };

        steps.push(ReleasePlanStep {
            id: "post_release".to_string(),
            step_type: "post_release".to_string(),
            label: Some("Run post-release commands".to_string()),
            needs: post_release_needs,
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert(
                    "commands".to_string(),
                    serde_json::Value::Array(
                        component
                            .post_release_commands
                            .iter()
                            .map(|s| serde_json::Value::String(s.clone()))
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
fn get_release_allowed_files(changelog_path: &std::path::Path, version_targets: &[String], repo_root: &std::path::Path) -> Vec<String> {
    let mut allowed = Vec::new();

    // Add changelog (convert to relative path)
    if let Ok(relative) = changelog_path.strip_prefix(repo_root) {
        allowed.push(relative.to_string_lossy().to_string());
    }

    // Add version targets (convert to relative paths)
    for target in version_targets {
        if let Ok(relative) = std::path::Path::new(target).strip_prefix(repo_root) {
            allowed.push(relative.to_string_lossy().to_string());
        }
    }

    allowed
}

/// Get uncommitted files that are NOT in the allowed list.
fn get_unexpected_uncommitted_files(uncommitted: &UncommittedChanges, allowed: &[String]) -> Vec<String> {
    let all_uncommitted: Vec<&String> = uncommitted.staged.iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .collect();

    all_uncommitted.into_iter()
        .filter(|f| !allowed.iter().any(|a| f.ends_with(a) || a.ends_with(*f)))
        .cloned()
        .collect()
}
