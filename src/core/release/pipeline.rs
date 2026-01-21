use std::collections::HashMap;

use crate::changelog;
use crate::component::{self, Component};
use crate::core::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::module::ModuleManifest;
use crate::pipeline::{self, PipelineStep};
use crate::version;

use super::executor::ReleaseStepExecutor;
use super::resolver::{resolve_modules, ReleaseCapabilityResolver};
use super::types::{
    ReleaseConfig, ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
    ReleaseStep, ReleaseStepType,
};

pub fn resolve_component_release(component: &Component) -> Option<ReleaseConfig> {
    component.release.clone()
}

fn validate_plan_prerequisites(component: &Component) -> Vec<String> {
    let mut warnings = Vec::new();

    match changelog::resolve_changelog_path(component) {
        Ok(changelog_path) => {
            let status = crate::core::local_files::local()
                .read(&changelog_path)
                .ok()
                .and_then(|content| {
                    let settings = changelog::resolve_effective_settings(Some(component));
                    changelog::check_next_section_content(&content, &settings.next_section_aliases)
                        .ok()
                        .flatten()
                });
            if let Some(status) = status {
                match status.as_str() {
                    "empty" => {
                        warnings.push(
                            "No unreleased changelog entries. Run `homeboy changelog add` first."
                                .to_string(),
                        );
                    }
                    "subsection_headers_only" => {
                        warnings.push(
                            "Changelog has subsection headers but no items. Add entries with `homeboy changelog add`."
                                .to_string(),
                        );
                    }
                    _ => {}
                }
            }
        }
        Err(_) => {
            warnings.push("No changelog configured for this component.".to_string());
        }
    }

    warnings
}

fn validate_step_dependencies(steps: &[ReleaseStep]) -> Result<()> {
    let has_version = steps
        .iter()
        .any(|s| s.step_type == ReleaseStepType::Version);
    let has_tag = steps.iter().any(|s| s.step_type == ReleaseStepType::GitTag);

    if has_tag && !has_version {
        return Err(Error::validation_invalid_argument(
            "release.steps",
            "git.tag step requires version step to set release context",
            None,
            Some(vec![
                "Add version step before git.tag in release config".to_string(),
                "Or specify tag explicitly in git.tag config: { \"name\": \"v1.2.3\" }".to_string(),
            ]),
        ));
    }

    if has_tag && has_version {
        let version_idx = steps
            .iter()
            .position(|s| s.step_type == ReleaseStepType::Version);
        let tag_idx = steps
            .iter()
            .position(|s| s.step_type == ReleaseStepType::GitTag);

        if let (Some(v_idx), Some(t_idx)) = (version_idx, tag_idx) {
            if v_idx > t_idx {
                return Err(Error::validation_invalid_argument(
                    "release.steps",
                    "version step must come before git.tag step",
                    Some(format!(
                        "version is at position {}, git.tag is at position {}",
                        v_idx + 1,
                        t_idx + 1
                    )),
                    Some(vec![
                        "Reorder steps so version runs before git.tag".to_string(),
                    ]),
                ));
            }
        }
    }

    Ok(())
}

pub fn plan(component_id: &str, module_id: Option<&str>) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, module_id)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let release = resolve_component_release(&component).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            "Release configuration is missing",
            Some(component_id.to_string()),
            None,
        )
        .with_hint(format!(
            "Use 'homeboy component set {} --json' to add a release block",
            component_id
        ))
        .with_hint("See 'homeboy docs commands/release' for examples")
    })?;

    let enabled = release.enabled.unwrap_or(true);

    validate_step_dependencies(&release.steps)?;

    let (release_steps, commit_auto_inserted) = auto_insert_commit_step(release.steps);
    let pipeline_steps: Vec<PipelineStep> = release_steps
        .iter()
        .cloned()
        .map(PipelineStep::from)
        .collect();
    let pipeline_plan = pipeline::plan(&pipeline_steps, &resolver, enabled, "release.steps")?;
    let steps: Vec<ReleasePlanStep> = pipeline_plan
        .steps
        .into_iter()
        .map(ReleasePlanStep::from)
        .collect();

    let mut warnings = pipeline_plan.warnings;
    warnings.extend(validate_plan_prerequisites(&component));

    let mut hints = build_plan_hints(component_id, &steps, &modules);
    if commit_auto_inserted {
        hints.insert(
            0,
            "git.commit step auto-inserted before git.tag".to_string(),
        );
    }
    hints.push(format!(
        "Review changes first with: homeboy changes {}",
        component_id
    ));

    Ok(ReleasePlan {
        component_id: component_id.to_string(),
        enabled,
        steps,
        warnings,
        hints,
    })
}

pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    let component = component::load(component_id)?;

    // 1. Run pre_version_bump_commands
    if !component.pre_version_bump_commands.is_empty() {
        version::run_pre_bump_commands(&component.pre_version_bump_commands, &component.local_path)?;
    }

    // 2. Auto-stage changelog changes if only changelog is uncommitted
    if let Some(ref changelog_target) = component.changelog_target {
        let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
        if uncommitted.has_changes {
            let all_uncommitted: Vec<&str> = uncommitted
                .staged
                .iter()
                .chain(uncommitted.unstaged.iter())
                .map(|s| s.as_str())
                .collect();

            let only_changelog = !all_uncommitted.is_empty()
                && all_uncommitted
                    .iter()
                    .all(|f| *f == changelog_target || f.ends_with(changelog_target));

            if only_changelog {
                eprintln!(
                    "[release] Auto-staging changelog changes: {}",
                    changelog_target
                );
                crate::git::stage_files(&component.local_path, &[changelog_target.as_str()])?;
            }
        }
    }

    // 3. Auto-commit uncommitted changes (or error if --no-commit)
    let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
    if uncommitted.has_changes {
        if options.no_commit {
            let mut details = vec![];
            if !uncommitted.staged.is_empty() {
                details.push(format!("Staged: {}", uncommitted.staged.join(", ")));
            }
            if !uncommitted.unstaged.is_empty() {
                details.push(format!("Unstaged: {}", uncommitted.unstaged.join(", ")));
            }
            if !uncommitted.untracked.is_empty() {
                details.push(format!("Untracked: {}", uncommitted.untracked.join(", ")));
            }
            return Err(Error::validation_invalid_argument(
                "workingTree",
                "Working tree has uncommitted changes (--no-commit specified)",
                Some(details.join("\n")),
                Some(vec![
                    "Commit your changes manually before releasing.".to_string(),
                    "Or remove --no-commit to auto-commit pre-release changes.".to_string(),
                ]),
            ));
        } else {
            let message = options
                .commit_message
                .clone()
                .unwrap_or_else(|| "pre-release changes".to_string());

            eprintln!("[release] Committing pre-release changes: {}...", message);

            let commit_options = crate::git::CommitOptions {
                staged_only: false,
                files: None,
                exclude: None,
                amend: false,
            };

            let commit_output =
                crate::git::commit(Some(component_id), Some(&message), commit_options)?;

            if !commit_output.success {
                return Err(Error::other(format!(
                    "Pre-release commit failed: {}",
                    commit_output.stderr
                )));
            }
        }
    }

    // 4. Load release config and modules
    let modules = resolve_modules(&component, None)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let release = resolve_component_release(&component).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            "Release configuration is missing",
            Some(component_id.to_string()),
            None,
        )
        .with_hint(format!(
            "Use 'homeboy component set {} --json' to add a release block",
            component_id
        ))
        .with_hint("See 'homeboy docs commands/release' for examples")
    })?;

    let enabled = release.enabled.unwrap_or(true);

    validate_step_dependencies(&release.steps)?;

    // 5. Build steps with auto-inserted commit
    let (release_steps, _commit_auto_inserted) = auto_insert_commit_step(release.steps);

    let executor = ReleaseStepExecutor::new(component_id.to_string(), modules.clone());

    let pipeline_steps: Vec<PipelineStep> =
        release_steps.into_iter().map(PipelineStep::from).collect();

    // 6. Execute pipeline (respects step dependencies via needs)
    let run_result = pipeline::run(
        &pipeline_steps,
        std::sync::Arc::new(executor),
        std::sync::Arc::new(resolver),
        enabled,
        "release.steps",
    )?;

    Ok(ReleaseRun {
        component_id: component_id.to_string(),
        enabled,
        result: run_result,
    })
}

fn auto_insert_commit_step(steps: Vec<ReleaseStep>) -> (Vec<ReleaseStep>, bool) {
    let has_tag = steps.iter().any(|s| s.step_type == ReleaseStepType::GitTag);
    let has_commit = steps.iter().any(|s| s.step_type == ReleaseStepType::GitCommit);

    if !has_tag || has_commit {
        return (steps, false);
    }

    let mut result = Vec::with_capacity(steps.len() + 1);
    let mut inserted = false;

    for step in steps {
        if step.step_type == ReleaseStepType::GitTag && !inserted {
            let commit_step = ReleaseStep {
                id: "git.commit".to_string(),
                step_type: ReleaseStepType::GitCommit,
                label: Some("Commit release changes".to_string()),
                needs: step.needs.clone(),
                config: HashMap::new(),
            };
            result.push(commit_step);
            inserted = true;

            let mut tag_step = step;
            tag_step.needs = vec!["git.commit".to_string()];
            result.push(tag_step);
        } else {
            result.push(step);
        }
    }

    (result, inserted)
}

fn build_plan_hints(
    component_id: &str,
    steps: &[ReleasePlanStep],
    modules: &[ModuleManifest],
) -> Vec<String> {
    let mut hints = Vec::new();
    if steps.is_empty() {
        hints.push("Release plan has no steps".to_string());
    }

    if steps
        .iter()
        .any(|step| matches!(step.status, ReleasePlanStatus::Missing))
    {
        if modules.is_empty() {
            hints.push("Configure component modules to resolve release actions".to_string());
        } else {
            let module_names: Vec<String> =
                modules.iter().map(|module| module.id.clone()).collect();
            hints.push(format!(
                "Release actions are resolved from modules: {}",
                module_names.join(", ")
            ));
        }
    }

    if !hints.is_empty() {
        hints.push(format!(
            "Update release config with: homeboy component set {} --json",
            component_id
        ));
    }

    hints
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

pub fn plan_unified(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
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
