use crate::core::component::Component;
use crate::core::extension::ExtensionManifest;
use crate::core::git;
use crate::core::plan::{PlanStep, PlanValues};
use crate::core::quality::{build_quality_steps as build_shared_quality_steps, QualityPlanOptions};
use crate::core::release::pipeline_capabilities::{
    get_publish_targets, has_package_capability, has_prepare_capability,
};
use crate::core::release::types::{
    ReleaseChangelogPlan, ReleaseOptions, ReleaseSemverRecommendation,
};
use crate::core::Result;

type StepConfig = PlanValues;

/// Return true if this component should get a GitHub Release created.
///
/// Resolves the remote URL from the component config (preferred) or from
/// `git remote get-url origin` in the component's local_path, then parses
/// it as a GitHub URL. Non-GitHub remotes (GitLab, self-hosted, etc.) fall
/// through cleanly — the step simply isn't added to the plan.
pub(super) fn github_release_applies(component: &Component) -> bool {
    let remote_url = component.remote_url.clone().or_else(|| {
        crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
            &component.local_path,
        ))
    });

    remote_url
        .as_deref()
        .and_then(crate::core::deploy::release_download::parse_github_url)
        .is_some()
}

fn ready_step(
    id: &str,
    step_type: &str,
    label: impl Into<String>,
    needs: Vec<String>,
    config: StepConfig,
) -> PlanStep {
    PlanStep::ready_labeled(id, step_type, label, needs, config)
}

fn disabled_step(
    id: &str,
    step_type: &str,
    label: impl Into<String>,
    config: StepConfig,
) -> PlanStep {
    PlanStep::disabled(id, step_type)
        .label(label)
        .inputs(config)
        .build()
}

fn string_config(key: &str, value: impl Into<String>) -> StepConfig {
    StepConfig::new().string(key, value)
}

pub(super) fn build_preflight_steps(
    options: &ReleaseOptions,
    semver_recommendation: Option<&ReleaseSemverRecommendation>,
    extensions: &[ExtensionManifest],
) -> Vec<PlanStep> {
    let default_branch_step = if options.pipeline.head {
        disabled_step(
            "preflight.default_branch",
            "preflight.default_branch",
            "Validate default branch",
            string_config("reason", "head-release"),
        )
    } else {
        ready_step(
            "preflight.default_branch",
            "preflight.default_branch",
            "Validate default branch",
            vec![],
            StepConfig::new(),
        )
    };
    let working_tree_step = if options.pipeline.head && options.pipeline.from_artifacts.is_some() {
        disabled_step(
            "preflight.working_tree",
            "preflight.working_tree",
            "Validate working tree",
            string_config("reason", "head-release-artifacts"),
        )
    } else {
        ready_step(
            "preflight.working_tree",
            "preflight.working_tree",
            "Validate working tree",
            vec!["preflight.git_identity".to_string()],
            StepConfig::new(),
        )
    };

    let mut steps = vec![
        default_branch_step,
        working_tree_step,
        ready_step(
            "preflight.remote_sync",
            "preflight.remote_sync",
            "Validate remote sync",
            vec!["preflight.working_tree".to_string()],
            StepConfig::new(),
        ),
        build_bump_policy_step(options, semver_recommendation),
    ];

    if has_wordpress_release_publish_target(extensions) {
        steps.push(ready_step(
            "preflight.wordpress_publish_token",
            "preflight.wordpress_publish_token",
            "Validate WordPress release publish token",
            vec!["preflight.bump_policy".to_string()],
            StepConfig::new(),
        ));
    }

    if let Some(identity) = options.git_identity.as_ref() {
        steps.insert(
            1,
            ready_step(
                "preflight.git_identity",
                "preflight.git_identity",
                "Configure git identity",
                vec!["preflight.default_branch".to_string()],
                string_config("identity", identity.as_str()),
            ),
        );
    } else {
        steps.insert(
            1,
            disabled_step(
                "preflight.git_identity",
                "preflight.git_identity",
                "Configure git identity",
                string_config("reason", "not-requested"),
            ),
        );
    }

    steps.extend(build_quality_steps(options));

    if !options.pipeline.head {
        steps.push(ready_step(
            "preflight.changelog_bootstrap",
            "preflight.changelog_bootstrap",
            "Ensure changelog exists",
            vec!["preflight.test".to_string()],
            StepConfig::new().bool("dry_run", options.dry_run),
        ));
    }

    steps
}

fn has_wordpress_release_publish_target(extensions: &[ExtensionManifest]) -> bool {
    extensions.iter().any(|extension| {
        extension.id == "wordpress"
            && extension
                .actions
                .iter()
                .any(|action| action.id == "release.publish")
    })
}

fn build_quality_steps(options: &ReleaseOptions) -> Vec<PlanStep> {
    build_shared_quality_steps(&QualityPlanOptions::release_preflight(
        "release",
        options.skip_checks,
    ))
}

fn build_bump_policy_step(
    options: &ReleaseOptions,
    semver_recommendation: Option<&ReleaseSemverRecommendation>,
) -> PlanStep {
    let Some(recommendation) = semver_recommendation else {
        return disabled_step(
            "preflight.bump_policy",
            "preflight.bump_policy",
            "Validate bump policy",
            string_config("reason", "no-releasable-commits"),
        );
    };

    let mut config = StepConfig::new()
        .string("requested", recommendation.requested_bump.clone())
        .bool("underbump", recommendation.is_underbump)
        .bool("force_lower_bump", options.bump_policy.force_lower_bump);
    if let Some(recommended) = recommendation.recommended_bump.as_ref() {
        config = config.string("recommended", recommended.clone());
    }

    if recommendation.is_underbump {
        config = config.string(
            "policy",
            if options.dry_run {
                "preview-lower-bump".to_string()
            } else if options.bump_policy.force_lower_bump {
                "forced-lower-bump".to_string()
            } else {
                "requires-force-lower-bump".to_string()
            },
        );
    }

    ready_step(
        "preflight.bump_policy",
        "preflight.bump_policy",
        "Validate bump policy",
        vec!["preflight.remote_sync".to_string()],
        config,
    )
}

/// Build all release steps: core steps (non-configurable) + publish steps (extension-derived).
pub(super) fn build_release_steps(
    component: &Component,
    extensions: &[ExtensionManifest],
    current_version: &str,
    new_version: &str,
    changelog_plan: &ReleaseChangelogPlan,
    options: &ReleaseOptions,
    monorepo: Option<&git::MonorepoContext>,
    warnings: &mut Vec<String>,
    _hints: &mut Vec<String>,
) -> Result<Vec<PlanStep>> {
    let mut steps = Vec::new();
    let publish_targets = get_publish_targets(extensions);

    add_release_extension_diagnostics(component, extensions, &publish_targets, options, warnings);

    if options.pipeline.head {
        return Ok(build_head_release_steps(
            component,
            extensions,
            new_version,
            options,
            monorepo,
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

    steps.extend(build_changelog_steps(
        changelog_plan,
        current_version,
        new_version,
        package_preflight_step_id
            .as_deref()
            .unwrap_or("preflight.changelog_bootstrap"),
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

    let tag_name = match monorepo {
        Some(ctx) => ctx.format_tag(new_version),
        None => format!("v{}", new_version),
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
        log_status!("release", "Skipping publish/package steps (--skip-publish)");
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
    monorepo: Option<&git::MonorepoContext>,
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

    if !options.pipeline.skip_publish {
        if let Some(dir) = options.pipeline.from_artifacts.as_ref() {
            steps.push(ready_step(
                "artifacts.inventory",
                "artifacts.inventory",
                "Inventory existing release artifacts",
                vec![artifact_need.clone()],
                string_config("dir", dir),
            ));
            artifact_need = "artifacts.inventory".to_string();
        } else if has_package_capability(extensions) {
            steps.push(ready_step(
                "package",
                "package",
                "Package release artifacts",
                vec![artifact_need.clone()],
                StepConfig::new(),
            ));
            artifact_need = "package".to_string();
        }
    } else if options.pipeline.skip_publish && !publish_targets.is_empty() {
        log_status!("release", "Skipping publish/package steps (--skip-publish)");
    }

    if !options.skip_github_release && github_release_applies(component) {
        let tag_name = match monorepo {
            Some(ctx) => ctx.format_tag(version),
            None => format!("v{}", version),
        };
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

fn build_changelog_steps(
    changelog_plan: &ReleaseChangelogPlan,
    current_version: &str,
    new_version: &str,
    initial_need: &str,
) -> Vec<PlanStep> {
    let policy_config = StepConfig::new()
        .string("policy", changelog_plan.policy.clone())
        .string("path", changelog_plan.path.clone())
        .bool("dry_run", changelog_plan.dry_run);

    let generate_config = StepConfig::new()
        .string("source", "commits")
        .number("entry_count", changelog_plan.entry_count as u64);

    let finalize_config = StepConfig::new()
        .string("path", changelog_plan.path.clone())
        .string("from", current_version)
        .string("to", new_version)
        .json("entries", &changelog_plan.entries)
        .number("entry_count", changelog_plan.entry_count as u64)
        .string("mode", "version-step");

    vec![
        ready_step(
            "changelog.policy",
            "changelog.policy",
            "Resolve changelog policy",
            vec![initial_need.to_string()],
            policy_config,
        ),
        ready_step(
            "changelog.generate",
            "changelog.generate",
            "Generate changelog entries from commits",
            vec!["changelog.policy".to_string()],
            generate_config,
        ),
        ready_step(
            "changelog.finalize",
            "changelog.finalize",
            "Finalize changelog entries into release section",
            vec!["changelog.generate".to_string()],
            finalize_config,
        ),
    ]
}

fn string_array_config(key: &str, values: &[String]) -> StepConfig {
    StepConfig::new().json(key, values)
}

#[cfg(test)]
mod tests {
    use super::{build_preflight_steps, build_release_steps, github_release_applies};
    use crate::core::component::{Component, ScopedExtensionConfig};
    use crate::core::extension::ExtensionManifest;
    use crate::core::plan::PlanStepStatus;
    use crate::core::release::types::{
        ReleaseBumpPolicyOptions, ReleaseChangelogPlan, ReleaseOptions, ReleasePipelineOptions,
        ReleaseSemverRecommendation,
    };

    #[test]
    fn test_build_preflight_steps() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let steps = build_preflight_steps(&options, None, &[]);
        let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();

        assert_eq!(
            ids,
            vec![
                "preflight.default_branch",
                "preflight.git_identity",
                "preflight.working_tree",
                "preflight.remote_sync",
                "preflight.bump_policy",
                "preflight.audit",
                "preflight.lint",
                "preflight.test",
                "preflight.changelog_bootstrap"
            ]
        );
        assert_eq!(steps[0].status, PlanStepStatus::Ready);
        assert_eq!(steps[1].status, PlanStepStatus::Disabled);
        assert_eq!(steps[2].needs, vec!["preflight.git_identity"]);
    }

    #[test]
    fn release_plan_marks_git_identity_ready_when_requested() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            git_identity: Some("Release Bot <bot@example.com>".to_string()),
            ..Default::default()
        };

        let steps = build_preflight_steps(&options, None, &[]);
        let identity = steps
            .iter()
            .find(|step| step.id == "preflight.git_identity")
            .expect("git identity step");

        assert_eq!(identity.status, PlanStepStatus::Ready);
        assert_eq!(identity.needs, vec!["preflight.default_branch"]);
        assert_eq!(
            identity
                .inputs
                .get("identity")
                .and_then(|value| value.as_str()),
            Some("Release Bot <bot@example.com>")
        );
    }

    #[test]
    fn release_plan_adds_wordpress_publish_token_preflight_for_wordpress_publish() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "WordPress",
            "version": "1.0.0",
            "actions": [
                {
                    "id": "release.publish",
                    "label": "Publish release",
                    "type": "command",
                    "command": "true"
                }
            ]
        }))
        .expect("extension manifest");
        extension.id = "wordpress".to_string();

        let steps = build_preflight_steps(&options, None, &[extension]);
        let token = steps
            .iter()
            .find(|step| step.id == "preflight.wordpress_publish_token")
            .expect("wordpress publish token preflight");

        assert_eq!(token.status, PlanStepStatus::Ready);
        assert_eq!(token.needs, vec!["preflight.bump_policy"]);
    }

    #[test]
    fn release_plan_marks_quality_preflights_disabled_when_checks_are_skipped() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            skip_checks: true,
            ..Default::default()
        };

        let steps = build_preflight_steps(&options, None, &[]);
        for step_id in ["preflight.audit", "preflight.lint", "preflight.test"] {
            let quality = steps
                .iter()
                .find(|step| step.id == step_id)
                .expect("quality step");

            assert_eq!(quality.status, PlanStepStatus::Disabled);
            assert_eq!(
                quality
                    .inputs
                    .get("reason")
                    .and_then(|value| value.as_str()),
                Some("--skip-checks")
            );
        }
    }

    #[test]
    fn head_release_with_artifacts_skips_branch_and_working_tree_checks() {
        let options = ReleaseOptions {
            bump_type: "head".to_string(),
            pipeline: ReleasePipelineOptions {
                head: true,
                from_artifacts: Some("artifacts".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let steps = build_preflight_steps(&options, None, &[]);
        let default_branch = steps
            .iter()
            .find(|step| step.id == "preflight.default_branch")
            .expect("default branch step");

        assert_eq!(default_branch.status, PlanStepStatus::Disabled);
        assert_eq!(
            default_branch
                .inputs
                .get("reason")
                .and_then(|value| value.as_str()),
            Some("head-release")
        );

        let working_tree = steps
            .iter()
            .find(|step| step.id == "preflight.working_tree")
            .expect("working tree step");

        assert_eq!(working_tree.status, PlanStepStatus::Disabled);
        assert_eq!(
            working_tree
                .inputs
                .get("reason")
                .and_then(|value| value.as_str()),
            Some("head-release-artifacts")
        );
    }

    #[test]
    fn release_plan_records_explicit_quality_preflights() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let steps = build_preflight_steps(&options, None, &[]);
        let audit = steps
            .iter()
            .find(|step| step.id == "preflight.audit")
            .expect("audit step");
        let lint = steps
            .iter()
            .find(|step| step.id == "preflight.lint")
            .expect("lint step");
        let test = steps
            .iter()
            .find(|step| step.id == "preflight.test")
            .expect("test step");
        let changelog_bootstrap = steps
            .iter()
            .find(|step| step.id == "preflight.changelog_bootstrap")
            .expect("changelog bootstrap step");

        assert_eq!(audit.status, PlanStepStatus::Disabled);
        assert_eq!(
            audit.inputs.get("reason").and_then(|value| value.as_str()),
            Some("no-release-audit-policy")
        );
        assert_eq!(lint.status, PlanStepStatus::Ready);
        assert_eq!(lint.needs, vec!["preflight.bump_policy"]);
        assert_eq!(test.status, PlanStepStatus::Ready);
        assert_eq!(test.needs, vec!["preflight.lint"]);
        assert_eq!(changelog_bootstrap.needs, vec!["preflight.test"]);
    }

    #[test]
    fn release_plan_records_unforced_lower_bump_policy() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let recommendation = semver_recommendation("minor", "patch", true);

        let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
        let bump_policy = steps
            .iter()
            .find(|step| step.id == "preflight.bump_policy")
            .expect("bump policy step");

        assert_eq!(bump_policy.status, PlanStepStatus::Ready);
        assert_eq!(bump_policy.needs, vec!["preflight.remote_sync"]);
        assert_eq!(
            bump_policy
                .inputs
                .get("recommended")
                .and_then(|value| value.as_str()),
            Some("minor")
        );
        assert_eq!(
            bump_policy
                .inputs
                .get("requested")
                .and_then(|value| value.as_str()),
            Some("patch")
        );
        assert_eq!(
            bump_policy
                .inputs
                .get("policy")
                .and_then(|value| value.as_str()),
            Some("requires-force-lower-bump")
        );
        assert_eq!(
            bump_policy
                .inputs
                .get("force_lower_bump")
                .and_then(|value| value.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn release_plan_records_forced_lower_bump_policy() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            bump_policy: ReleaseBumpPolicyOptions {
                force_lower_bump: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let recommendation = semver_recommendation("minor", "patch", true);

        let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
        let bump_policy = steps
            .iter()
            .find(|step| step.id == "preflight.bump_policy")
            .expect("bump policy step");

        assert_eq!(
            bump_policy
                .inputs
                .get("policy")
                .and_then(|value| value.as_str()),
            Some("forced-lower-bump")
        );
        assert_eq!(
            bump_policy
                .inputs
                .get("force_lower_bump")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn release_plan_records_dry_run_lower_bump_preview() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            dry_run: true,
            ..Default::default()
        };
        let recommendation = semver_recommendation("minor", "patch", true);

        let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
        let bump_policy = steps
            .iter()
            .find(|step| step.id == "preflight.bump_policy")
            .expect("bump policy step");

        assert_eq!(
            bump_policy
                .inputs
                .get("policy")
                .and_then(|value| value.as_str()),
            Some("preview-lower-bump")
        );
    }

    #[test]
    fn test_build_release_steps() {
        let component = fixture_component();
        let extension = serde_json::from_value(serde_json::json!({
            "name": "Fixture",
            "version": "1.0.0",
            "actions": [
                {
                    "id": "release.prepare",
                    "label": "Prepare release",
                    "type": "command",
                    "command": "true"
                }
            ]
        }))
        .expect("extension manifest");
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let steps = build_release_steps(
            &component,
            &[extension],
            "1.0.0",
            "1.0.1",
            &fixture_changelog_plan(),
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        let changelog_policy_index = step_index(&ids, "changelog.policy");
        let changelog_index = step_index(&ids, "changelog.generate");
        let changelog_finalize_index = step_index(&ids, "changelog.finalize");
        let version_index = step_index(&ids, "version");
        let prepare_index = step_index(&ids, "release.prepare");
        let commit_index = step_index(&ids, "git.commit");

        assert!(changelog_policy_index < changelog_index);
        assert!(changelog_index < version_index);
        assert!(changelog_finalize_index < version_index);
        assert!(version_index < prepare_index);
        assert!(prepare_index < commit_index);
        assert_eq!(steps[changelog_index].needs, vec!["changelog.policy"]);
        assert_eq!(
            steps[changelog_finalize_index].needs,
            vec!["changelog.generate"]
        );
        assert_eq!(steps[version_index].needs, vec!["changelog.finalize"]);
        assert_eq!(steps[prepare_index].needs, vec!["version"]);
        assert_eq!(steps[commit_index].needs, vec!["release.prepare"]);
    }

    #[test]
    fn release_plan_runs_package_preflight_before_mutating_release_steps() {
        let mut component = fixture_component();
        component.extensions = Some(std::collections::HashMap::from([(
            "fixture-packager".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Fixture Packager",
            "version": "1.0.0",
            "actions": [
                {
                    "id": "release.package",
                    "label": "Package release",
                    "type": "command",
                    "command": "true"
                },
                {
                    "id": "release.publish",
                    "label": "Publish release",
                    "type": "command",
                    "command": "true"
                }
            ]
        }))
        .expect("extension manifest");
        extension.id = "fixture-packager".to_string();
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let steps = build_release_steps(
            &component,
            &[extension],
            "1.0.0",
            "1.0.1",
            &fixture_changelog_plan(),
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        let package_preflight_index = step_index(&ids, "preflight.package");
        let changelog_finalize_index = step_index(&ids, "changelog.finalize");
        let version_index = step_index(&ids, "version");
        let commit_index = step_index(&ids, "git.commit");

        assert!(package_preflight_index < changelog_finalize_index);
        assert!(package_preflight_index < version_index);
        assert!(package_preflight_index < commit_index);

        let package_preflight = &steps[package_preflight_index];
        assert_eq!(
            package_preflight.needs,
            vec!["preflight.changelog_bootstrap"]
        );

        let changelog_policy = steps
            .iter()
            .find(|step| step.id == "changelog.policy")
            .expect("changelog policy step");
        assert_eq!(changelog_policy.needs, vec!["preflight.package"]);
    }

    #[test]
    fn release_plan_records_changelog_contract() {
        let component = fixture_component();
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let changelog_plan = fixture_changelog_plan();

        let steps = build_release_steps(
            &component,
            &[],
            "1.0.0",
            "1.0.1",
            &changelog_plan,
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        let policy = steps
            .iter()
            .find(|step| step.id == "changelog.policy")
            .expect("changelog policy step");
        let generate = steps
            .iter()
            .find(|step| step.id == "changelog.generate")
            .expect("changelog generate step");
        let finalize = steps
            .iter()
            .find(|step| step.id == "changelog.finalize")
            .expect("changelog finalize step");

        assert_eq!(
            policy.inputs.get("policy").and_then(|value| value.as_str()),
            Some("generated")
        );
        assert_eq!(
            policy.inputs.get("path").and_then(|value| value.as_str()),
            Some("CHANGELOG.md")
        );
        assert_eq!(
            generate
                .inputs
                .get("entry_count")
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        assert_eq!(
            finalize.inputs.get("mode").and_then(|value| value.as_str()),
            Some("version-step")
        );
        assert_eq!(
            finalize
                .inputs
                .get("entries")
                .and_then(|value| value.as_object())
                .and_then(|entries| entries.get("Fixed"))
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn release_plan_includes_deploy_intent_when_requested() {
        let component = fixture_component();
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            pipeline: ReleasePipelineOptions {
                deploy: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let steps = build_release_steps(
            &component,
            &[],
            "1.0.0",
            "1.0.1",
            &fixture_changelog_plan(),
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        let deploy = steps
            .iter()
            .find(|step| step.id == "deploy")
            .expect("deploy step");
        assert_eq!(deploy.needs, vec!["git.push"]);
        assert_eq!(
            deploy
                .inputs
                .get("execution")
                .and_then(|value| value.as_str()),
            Some("release_plan")
        );
    }

    #[test]
    fn head_release_plan_skips_mutation_steps_and_uses_existing_artifacts() {
        let mut component = fixture_component();
        component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
        let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "WordPress",
            "version": "1.0.0",
            "actions": [
                {
                    "id": "release.publish",
                    "label": "Publish release",
                    "type": "command",
                    "command": "true"
                }
            ]
        }))
        .expect("extension manifest");
        extension.id = "wordpress".to_string();
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "head".to_string(),
            pipeline: ReleasePipelineOptions {
                head: true,
                from_artifacts: Some("artifacts".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let steps = build_release_steps(
            &component,
            &[extension],
            "1.0.1",
            "1.0.1",
            &fixture_changelog_plan(),
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        assert!(!ids.contains(&"changelog.finalize"));
        assert!(!ids.contains(&"version"));
        assert!(!ids.contains(&"git.commit"));
        assert!(!ids.contains(&"git.tag"));
        assert!(!ids.contains(&"git.push"));
        assert_eq!(
            ids,
            vec![
                "artifacts.inventory",
                "github.release",
                "publish.wordpress",
                "cleanup"
            ]
        );
        assert_eq!(
            steps[0].inputs.get("dir").and_then(|value| value.as_str()),
            Some("artifacts")
        );
        assert_eq!(steps[1].needs, vec!["artifacts.inventory"]);
        assert_eq!(steps[2].needs, vec!["artifacts.inventory"]);
    }

    #[test]
    fn release_plan_warns_when_configured_extensions_have_no_publish_action() {
        let mut component = fixture_component();
        component.extensions = Some(std::collections::HashMap::from([(
            "wordpress".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "WordPress",
            "version": "1.0.0",
            "actions": [
                {
                    "id": "release.package",
                    "label": "Package release",
                    "type": "command",
                    "command": "true"
                }
            ]
        }))
        .expect("extension manifest");
        extension.id = "wordpress".to_string();
        let mut warnings = Vec::new();
        let mut hints = Vec::new();
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let _steps = build_release_steps(
            &component,
            &[extension],
            "1.0.0",
            "1.0.1",
            &fixture_changelog_plan(),
            &options,
            None,
            &mut warnings,
            &mut hints,
        )
        .expect("steps");

        assert!(warnings.iter().any(|warning| {
            warning.contains("configured extensions (wordpress)")
                && warning.contains("release.package")
                && warning.contains("no extension provides 'release.publish'")
        }));
    }

    #[test]
    fn test_github_release_applies() {
        let mut github_component = fixture_component();
        github_component.remote_url =
            Some("https://github.com/Extra-Chill/homeboy.git".to_string());
        let mut non_github_component = fixture_component();
        non_github_component.remote_url =
            Some("https://gitlab.example.com/acme/tool.git".to_string());

        assert!(github_release_applies(&github_component));
        assert!(!github_release_applies(&non_github_component));
    }

    fn fixture_component() -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: "/tmp/fixture".to_string(),
            ..Default::default()
        }
    }

    fn fixture_changelog_plan() -> ReleaseChangelogPlan {
        ReleaseChangelogPlan {
            policy: "generated".to_string(),
            path: "CHANGELOG.md".to_string(),
            dry_run: false,
            entries: std::collections::HashMap::from([(
                "Fixed".to_string(),
                vec!["Correct release output".to_string()],
            )]),
            entry_count: 1,
        }
    }

    fn semver_recommendation(
        recommended: &str,
        requested: &str,
        is_underbump: bool,
    ) -> ReleaseSemverRecommendation {
        ReleaseSemverRecommendation {
            latest_tag: Some("v1.0.0".to_string()),
            range: "v1.0.0..HEAD".to_string(),
            commits: vec![],
            recommended_bump: Some(recommended.to_string()),
            requested_bump: requested.to_string(),
            is_underbump,
            reasons: vec![],
        }
    }

    fn step_index(ids: &[&str], id: &str) -> usize {
        ids.iter()
            .position(|candidate| *candidate == id)
            .unwrap_or_else(|| panic!("missing {id} step"))
    }
}
