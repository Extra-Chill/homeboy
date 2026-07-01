use super::builders::{ready_step, string_array_config, string_config, StepConfig};
use super::changelog::build_changelog_steps;
use super::hints::{github_release_applies, push_publish_vs_github_release_hints};
use crate::core::component::Component;
use crate::core::extension::ExtensionManifest;
use crate::core::plan::PlanStep;
use crate::core::release::pipeline_capabilities::{
    get_publish_targets, has_package_capability, has_prepare_capability,
};
use crate::core::release::scope::ReleaseScope;
use crate::core::release::types::{ReleaseChangelogPlan, ReleaseOptions};
use crate::core::Result;

/// Build all release steps: core steps (non-configurable) + publish steps (extension-derived).
#[allow(clippy::too_many_arguments)]
pub(in crate::core::release) fn build_release_steps(
    component: &Component,
    extensions: &[ExtensionManifest],
    current_version: &str,
    new_version: &str,
    changelog_plan: &ReleaseChangelogPlan,
    options: &ReleaseOptions,
    release_scope: &ReleaseScope,
    warnings: &mut Vec<String>,
    hints: &mut Vec<String>,
) -> Result<Vec<PlanStep>> {
    let mut steps = Vec::new();
    let publish_targets = get_publish_targets(extensions);

    push_publish_vs_github_release_hints(component, options, &publish_targets, hints);

    add_release_extension_diagnostics(component, extensions, &publish_targets, options, warnings);

    if options.pipeline.head {
        return Ok(build_head_release_steps(
            component,
            extensions,
            new_version,
            options,
            release_scope,
            &publish_targets,
            warnings,
        ));
    }

    if !publish_targets.is_empty() && !has_package_capability(extensions) {
        warnings.push(
            "Publish targets derived from extensions but no extension provides 'release.package'. \
             Add an extension that provides packaging."
                .to_string(),
        );
    }

    let package_preflight_step_id = add_package_preflight_step(
        &mut steps,
        extensions,
        &publish_targets,
        options,
        "preflight.changelog_bootstrap",
    );

    let tag_name = release_scope.tag_name(new_version);

    let tag_preflight_needs = package_preflight_step_id
        .as_deref()
        .unwrap_or("preflight.changelog_bootstrap");
    steps.push(ready_step(
        "preflight.tag_availability",
        "preflight.tag_availability",
        format!("Check release tag {} is available", tag_name),
        vec![tag_preflight_needs.to_string()],
        string_config("name", tag_name.clone()),
    ));

    steps.extend(build_changelog_steps(
        changelog_plan,
        current_version,
        new_version,
        "preflight.tag_availability",
    ));

    let version_config = StepConfig::new()
        .string("bump", options.bump_type.clone())
        .string("from", current_version)
        .string("to", new_version);
    steps.push(ready_step(
        "version",
        "version",
        format!(
            "Bump version {} → {} ({})",
            current_version, new_version, options.bump_type
        ),
        vec!["changelog.finalize".to_string()],
        version_config,
    ));

    let commit_needs = if has_prepare_capability(extensions) {
        steps.push(ready_step(
            "release.prepare",
            "release.prepare",
            "Prepare release files",
            vec!["version".to_string()],
            StepConfig::new(),
        ));
        vec!["release.prepare".to_string()]
    } else {
        vec!["version".to_string()]
    };

    steps.push(ready_step(
        "git.commit",
        "git.commit",
        format!("Commit release: v{}", new_version),
        commit_needs,
        StepConfig::new(),
    ));

    let tag_needs = if !publish_targets.is_empty() && !options.pipeline.skip_publish {
        steps.push(ready_step(
            "package",
            "package",
            "Package release artifacts",
            vec!["git.commit".to_string()],
            StepConfig::new(),
        ));
        vec!["package".to_string()]
    } else {
        vec!["git.commit".to_string()]
    };

    steps.push(ready_step(
        "git.tag",
        "git.tag",
        format!("Tag {}", tag_name),
        tag_needs,
        string_config("name", tag_name),
    ));

    steps.push(ready_step(
        "git.push",
        "git.push",
        "Push to remote",
        vec!["git.tag".to_string()],
        StepConfig::new().bool("tags", true),
    ));

    if !options.skip_github_release && github_release_applies(component) {
        steps.push(ready_step(
            "github.release",
            "github.release",
            "Create GitHub Release",
            vec!["git.push".to_string()],
            StepConfig::new(),
        ));
    }

    let mut publish_step_ids: Vec<String> = Vec::new();
    if !publish_targets.is_empty() && !options.pipeline.skip_publish {
        for target in &publish_targets {
            let step_id = format!("publish.{}", target);
            publish_step_ids.push(step_id.clone());
            steps.push(ready_step(
                &step_id,
                &step_id,
                format!("Publish to {}", target),
                vec!["git.push".to_string()],
                StepConfig::new(),
            ));
        }

        if !options.pipeline.deploy {
            steps.push(ready_step(
                "cleanup",
                "cleanup",
                "Clean up release artifacts",
                publish_step_ids.clone(),
                StepConfig::new(),
            ));
        }
    } else if options.pipeline.skip_publish && !publish_targets.is_empty() {
        log_status!(
            "release",
            "Skipping registry/package publishing (--skip-publish); GitHub Release is unaffected"
        );
    }

    let post_release_hooks = crate::core::engine::hooks::resolve_hooks(
        component,
        crate::core::engine::hooks::events::POST_RELEASE,
    );
    if !post_release_hooks.is_empty() {
        let post_release_needs = if !options.pipeline.skip_publish && !publish_targets.is_empty() {
            if options.pipeline.deploy {
                publish_step_ids.clone()
            } else {
                vec!["cleanup".to_string()]
            }
        } else {
            vec!["git.push".to_string()]
        };

        steps.push(ready_step(
            "post_release",
            "post_release",
            "Run post-release hooks",
            post_release_needs,
            string_array_config("commands", &post_release_hooks),
        ));
    }

    if options.pipeline.deploy {
        let deploy_needs = if !post_release_hooks.is_empty() {
            vec!["post_release".to_string()]
        } else if !options.pipeline.skip_publish && !publish_step_ids.is_empty() {
            publish_step_ids
        } else {
            vec!["git.push".to_string()]
        };

        steps.push(ready_step(
            "deploy",
            "deploy",
            "Deploy released component",
            deploy_needs,
            string_config("execution", "release_plan"),
        ));
    }

    Ok(steps)
}

fn build_head_release_steps(
    component: &Component,
    extensions: &[ExtensionManifest],
    version: &str,
    options: &ReleaseOptions,
    release_scope: &ReleaseScope,
    publish_targets: &[String],
    warnings: &mut Vec<String>,
) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    let mut artifact_need = "preflight.remote_sync".to_string();

    if !publish_targets.is_empty()
        && !options.pipeline.skip_publish
        && options.pipeline.from_artifacts.is_none()
        && !has_package_capability(extensions)
    {
        warnings.push(
            "Publish targets derived from extensions but no extension provides 'release.package'. \
             Add an extension that provides packaging or use --from-artifacts."
                .to_string(),
        );
    }

    if let Some(dir) = options.pipeline.from_artifacts.as_ref() {
        steps.push(ready_step(
            "artifacts.inventory",
            "artifacts.inventory",
            "Inventory existing release artifacts",
            vec![artifact_need.clone()],
            string_config("dir", dir),
        ));
        artifact_need = "artifacts.inventory".to_string();
    } else if !options.pipeline.skip_publish && has_package_capability(extensions) {
        steps.push(ready_step(
            "package",
            "package",
            "Package release artifacts",
            vec![artifact_need.clone()],
            StepConfig::new(),
        ));
        artifact_need = "package".to_string();
    }

    if options.pipeline.skip_publish && !publish_targets.is_empty() {
        log_status!(
            "release",
            "Skipping registry/package publishing (--skip-publish); GitHub Release is unaffected"
        );
    }

    if !options.skip_github_release && github_release_applies(component) {
        let tag_name = release_scope.tag_name(version);
        steps.push(ready_step(
            "github.release",
            "github.release",
            "Create GitHub Release",
            vec![artifact_need.clone()],
            string_config("tag", tag_name),
        ));
    }

    let mut publish_step_ids: Vec<String> = Vec::new();
    if !publish_targets.is_empty() && !options.pipeline.skip_publish {
        for target in publish_targets {
            let step_id = format!("publish.{}", target);
            publish_step_ids.push(step_id.clone());
            steps.push(ready_step(
                &step_id,
                &step_id,
                format!("Publish to {}", target),
                vec![artifact_need.clone()],
                StepConfig::new(),
            ));
        }

        if !options.pipeline.deploy {
            steps.push(ready_step(
                "cleanup",
                "cleanup",
                "Clean up release artifacts",
                publish_step_ids.clone(),
                StepConfig::new(),
            ));
        }
    }

    let post_release_hooks = crate::core::engine::hooks::resolve_hooks(
        component,
        crate::core::engine::hooks::events::POST_RELEASE,
    );
    if !post_release_hooks.is_empty() {
        let post_release_needs = if !options.pipeline.skip_publish && !publish_targets.is_empty() {
            if options.pipeline.deploy {
                publish_step_ids.clone()
            } else {
                vec!["cleanup".to_string()]
            }
        } else if !options.skip_github_release && github_release_applies(component) {
            vec!["github.release".to_string()]
        } else {
            vec![artifact_need.clone()]
        };

        steps.push(ready_step(
            "post_release",
            "post_release",
            "Run post-release hooks",
            post_release_needs,
            string_array_config("commands", &post_release_hooks),
        ));
    }

    if options.pipeline.deploy {
        let deploy_needs = if !post_release_hooks.is_empty() {
            vec!["post_release".to_string()]
        } else if !options.pipeline.skip_publish && !publish_step_ids.is_empty() {
            publish_step_ids
        } else if !options.skip_github_release && github_release_applies(component) {
            vec!["github.release".to_string()]
        } else {
            vec![artifact_need]
        };

        steps.push(ready_step(
            "deploy",
            "deploy",
            "Deploy released component",
            deploy_needs,
            string_config("execution", "release_plan"),
        ));
    }

    steps
}

fn add_package_preflight_step(
    steps: &mut Vec<PlanStep>,
    extensions: &[ExtensionManifest],
    publish_targets: &[String],
    options: &ReleaseOptions,
    needs: &str,
) -> Option<String> {
    if publish_targets.is_empty()
        || options.pipeline.skip_publish
        || !has_package_capability(extensions)
    {
        return None;
    }

    let step_id = "preflight.package".to_string();
    steps.push(ready_step(
        &step_id,
        "preflight.package",
        "Validate package tooling",
        vec![needs.to_string()],
        StepConfig::new(),
    ));
    Some(step_id)
}

fn add_release_extension_diagnostics(
    component: &Component,
    extensions: &[ExtensionManifest],
    publish_targets: &[String],
    options: &ReleaseOptions,
    warnings: &mut Vec<String>,
) {
    if options.pipeline.skip_publish || !publish_targets.is_empty() {
        return;
    }

    let Some(configured) = component.extensions.as_ref() else {
        return;
    };
    if configured.is_empty() {
        return;
    }

    let mut configured_ids: Vec<String> = configured.keys().cloned().collect();
    configured_ids.sort();
    if !configured_extension_has_release_actions(&configured_ids, extensions)
        && !has_package_capability(extensions)
    {
        return;
    }

    let loaded = extensions
        .iter()
        .map(|extension| {
            let mut action_ids: Vec<&str> = extension
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            action_ids.sort_unstable();
            format!("{} [{}]", extension.id, action_ids.join(", "))
        })
        .collect::<Vec<_>>();

    warnings.push(format!(
        "Release publish planning found configured extensions ({}) but no extension provides \
         'release.publish'. Loaded extension actions: {}.",
        configured_ids.join(", "),
        if loaded.is_empty() {
            "none".to_string()
        } else {
            loaded.join("; ")
        }
    ));
}

fn configured_extension_has_release_actions(
    configured_ids: &[String],
    extensions: &[ExtensionManifest],
) -> bool {
    extensions.iter().any(|extension| {
        configured_ids.iter().any(|id| id == &extension.id)
            && extension
                .actions
                .iter()
                .any(|action| action.id.starts_with("release."))
    })
}
