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
    ReleaseStepType,
};

pub fn resolve_component_release(component: &Component) -> Option<ReleaseConfig> {
    component.release.clone()
}

/// Execute a release by computing the plan and executing it.
/// What you preview (dry-run) is what you execute.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    // 1. Compute the plan (same as dry-run)
    let release_plan = plan(component_id, options)?;

    // 2. Load component and modules for execution
    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, None)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let executor = ReleaseStepExecutor::new(component_id.to_string(), modules);

    // 3. Convert plan steps to pipeline steps
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

    // 4. Execute pipeline
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

fn has_publish_targets(component: &Component) -> bool {
    if let Some(release) = &component.release {
        release.steps.iter().any(|step| {
            matches!(
                step.step_type,
                ReleaseStepType::GitPush | ReleaseStepType::ModuleAction(_) | ReleaseStepType::ModuleRun
            )
        })
    } else {
        false
    }
}

pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;

    let changelog_path = changelog::resolve_changelog_path(&component)?;
    let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(&component));

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
                        "Add changelog entries: homeboy changelog add <component> -m \"...\"".to_string(),
                    ]),
                ));
            }
            "subsection_headers_only" => {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Changelog has subsection headers but no items",
                    None,
                    Some(vec![
                        "Add changelog entries: homeboy changelog add <component> -m \"...\"".to_string(),
                    ]),
                ));
            }
            _ => {}
        }
    }

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
    let needs_pre_commit = uncommitted.has_changes && !options.no_commit;

    let has_publish = has_publish_targets(&component);
    let will_push = !options.no_push;
    let will_publish = has_publish && !options.no_push;

    let mut steps = Vec::new();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();

    if needs_pre_commit {
        let pre_commit_message = options
            .commit_message
            .clone()
            .unwrap_or_else(|| "pre-release changes".to_string());
        steps.push(ReleasePlanStep {
            id: "pre-release.commit".to_string(),
            step_type: "git.commit".to_string(),
            label: Some(format!("Commit pre-release changes: {}", pre_commit_message)),
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
    } else if uncommitted.has_changes && options.no_commit {
        warnings.push("Working tree has uncommitted changes (--no-commit will cause release to fail)".to_string());
    }

    let version_needs = if needs_pre_commit {
        vec!["pre-release.commit".to_string()]
    } else {
        vec![]
    };
    steps.push(ReleasePlanStep {
        id: "version".to_string(),
        step_type: "version".to_string(),
        label: Some(format!(
            "Bump version {} → {} ({})",
            version_info.version, new_version, options.bump_type
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
                serde_json::Value::String(version_info.version.clone()),
            );
            config.insert(
                "to".to_string(),
                serde_json::Value::String(new_version.clone()),
            );
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    steps.push(ReleasePlanStep {
        id: "git.commit".to_string(),
        step_type: "git.commit".to_string(),
        label: Some(format!("Commit release: v{}", new_version)),
        needs: vec!["version".to_string()],
        config: std::collections::HashMap::new(),
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    if !options.no_tag {
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
    }

    if will_push {
        let needs = if options.no_tag {
            vec!["git.commit".to_string()]
        } else {
            vec!["git.tag".to_string()]
        };
        steps.push(ReleasePlanStep {
            id: "git.push".to_string(),
            step_type: "git.push".to_string(),
            label: Some("Push to remote".to_string()),
            needs,
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert("tags".to_string(), serde_json::Value::Bool(!options.no_tag));
                config
            },
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    }

    if will_publish {
        if let Some(release) = &component.release {
            for step in &release.steps {
                if matches!(
                    step.step_type,
                    ReleaseStepType::ModuleAction(_) | ReleaseStepType::ModuleRun
                ) {
                    let needs = if will_push {
                        vec!["git.push".to_string()]
                    } else if !options.no_tag {
                        vec!["git.tag".to_string()]
                    } else {
                        vec!["git.commit".to_string()]
                    };
                    steps.push(ReleasePlanStep {
                        id: step.id.clone(),
                        step_type: step.step_type.as_str().to_string(),
                        label: step.label.clone(),
                        needs,
                        config: step.config.clone(),
                        status: ReleasePlanStatus::Ready,
                        missing: vec![],
                    });
                }
            }
        }
    }

    if options.no_push {
        hints.push("Skipping push and publish (--no-push)".to_string());
    }

    if options.no_tag {
        hints.push("Skipping tag creation (--no-tag)".to_string());
    }

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
