use super::builders::{disabled_step, ready_step, string_config, StepConfig};
use crate::core::extension::ExtensionManifest;
use crate::core::plan::PlanStep;
use crate::core::quality::{build_quality_steps as build_shared_quality_steps, QualityPlanOptions};
use crate::core::release::types::{ReleaseOptions, ReleaseSemverRecommendation};

pub(in crate::core::release) fn build_preflight_steps(
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

    steps.extend(build_extension_release_preflight_steps(extensions));

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

fn build_extension_release_preflight_steps(extensions: &[ExtensionManifest]) -> Vec<PlanStep> {
    extensions
        .iter()
        .flat_map(|extension| {
            extension.release_preflights.iter().map(|preflight| {
                let step_id = format!("preflight.extension.{}.{}", extension.id, preflight.id);
                ready_step(
                    &step_id,
                    &step_id,
                    preflight.label.clone(),
                    preflight.needs.clone(),
                    StepConfig::new()
                        .string("extension", extension.id.clone())
                        .string("action", preflight.action.clone())
                        .string("preflight", preflight.id.clone()),
                )
            })
        })
        .collect()
}

fn build_quality_steps(options: &ReleaseOptions) -> Vec<PlanStep> {
    build_shared_quality_steps(
        &QualityPlanOptions::release_preflight("release", options.skip_checks)
            .with_granular_skips(&options.skip_checks_granular),
    )
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
