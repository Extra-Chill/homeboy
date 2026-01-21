use crate::changelog;
use crate::component::{self, Component};
use crate::core::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::pipeline::{self, PipelineStep};
use crate::version;

use super::executor::ReleaseStepExecutor;
use super::resolver::{resolve_modules, ReleaseCapabilityResolver};
use super::types::{
    ReleaseConfig, ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
};

pub fn resolve_component_release(component: &Component) -> Option<ReleaseConfig> {
    component.release.clone()
}

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

/// Plan a release with built-in core steps and config-driven publish targets.
///
/// Core steps (always generated, non-configurable):
/// 1. Pre-commit uncommitted changes (if any)
/// 2. Version bump + changelog finalization
/// 3. Git commit
/// 4. Git tag
/// 5. Git push (commits AND tags)
///
/// Publish steps (config-driven):
/// - From release.publish array: ["github", "homebrew", "rust"]
pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;

    validate_changelog(&component)?;

    let version_info = version::read_version(Some(component_id))?;
    let new_version = version::increment_version(&version_info.version, &options.bump_type)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "version",
                format!("Invalid version format: {}", version_info.version),
                None,
                None,
            )
        })?;

    version::validate_changelog_for_bump(&component, &version_info.version, &new_version)?;

    let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
    let warnings = Vec::new();
    let mut hints = Vec::new();

    let steps = build_release_steps(
        &component,
        &version_info.version,
        &new_version,
        options,
        uncommitted.has_changes,
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

/// Build all release steps: core steps (non-configurable) + publish steps (config-driven).
fn build_release_steps(
    component: &Component,
    current_version: &str,
    new_version: &str,
    options: &ReleaseOptions,
    has_uncommitted: bool,
    hints: &mut Vec<String>,
) -> Result<Vec<ReleasePlanStep>> {
    let mut steps = Vec::new();

    // === CORE STEPS (non-configurable, always present) ===

    // 1. Pre-release commit for uncommitted changes (if any)
    if has_uncommitted {
        let pre_commit_message = options
            .commit_message
            .clone()
            .unwrap_or_else(|| "pre-release changes".to_string());
        steps.push(ReleasePlanStep {
            id: "pre-release.commit".to_string(),
            step_type: "git.commit".to_string(),
            label: Some(format!(
                "Commit pre-release changes: {}",
                pre_commit_message
            )),
            needs: vec![],
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert(
                    "message".to_string(),
                    serde_json::Value::String(pre_commit_message),
                );
                config
            },
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
        hints.push("Will auto-commit uncommitted changes before release".to_string());
    }

    // 2. Version bump
    let version_needs = if has_uncommitted {
        vec!["pre-release.commit".to_string()]
    } else {
        vec![]
    };
    steps.push(ReleasePlanStep {
        id: "version".to_string(),
        step_type: "version".to_string(),
        label: Some(format!(
            "Bump version {} → {} ({})",
            current_version, new_version, options.bump_type
        )),
        needs: version_needs,
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

    // 3. Git commit
    steps.push(ReleasePlanStep {
        id: "git.commit".to_string(),
        step_type: "git.commit".to_string(),
        label: Some(format!("Commit release: v{}", new_version)),
        needs: vec!["version".to_string()],
        config: std::collections::HashMap::new(),
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // 4. Git tag
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

    // === PUBLISH STEPS (config-driven) ===

    if let Some(release) = &component.release {
        let mut previous_step = "git.push".to_string();

        for target in &release.publish {
            let step_id = format!("publish.{}", target);
            let step_type = format!("publish.{}", target);

            steps.push(ReleasePlanStep {
                id: step_id.clone(),
                step_type,
                label: Some(format!("Publish to {}", target)),
                needs: vec![previous_step.clone()],
                config: std::collections::HashMap::new(),
                status: ReleasePlanStatus::Ready,
                missing: vec![],
            });

            previous_step = step_id;
        }
    }

    Ok(steps)
}
