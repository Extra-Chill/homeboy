use crate::core::plan::PlanStep;

use super::types::{ReleaseOptions, ReleasePlan, ReleaseSemverRecommendation};

pub(super) fn release_skip_plan(
    component_id: &str,
    options: &ReleaseOptions,
    semver_recommendation: Option<ReleaseSemverRecommendation>,
    release_already_at_head: Option<&str>,
) -> Option<ReleasePlan> {
    // Detect "release already exists at HEAD" before other skip reasons so a
    // forced re-run after a prior partial release surfaces a clear message
    // instead of a confusing next-version changelog contract error (#4316).
    if let Some(tag) = release_already_at_head {
        return Some(skipped_release_plan(
            component_id,
            "release-already-at-head",
            &format!("Release {} already published at HEAD", tag),
            &format!(
                "Release {tag} already exists at HEAD: tag is published and the release commit \
                 is checked out. No new tag, release commit, or GitHub Release is needed. \
                 To finish/repair publish steps for this tag, run: \
                 homeboy release {component_id} --head. \
                 To make a new release on top, commit new work first.",
                tag = tag,
                component_id = component_id,
            ),
            None,
        ));
    }

    if semver_recommendation.is_none() && !options.bump_policy.force_empty_release {
        // Echo the operator's currently-set flags in the actionable force hint
        // so docs-only / guidance-only releases can be forced without guessing
        // which flags the previous invocation passed (issue #4316).
        let force_command = force_release_command_hint(component_id, options, "patch");
        return Some(skipped_release_plan(
            component_id,
            "no-releasable-commits",
            "No releasable commits since last tag",
            &format!(
                "No release was created. No tag created, no release commit created, no GitHub Release created. \
                 Force a release when intentional with: {}",
                force_command
            ),
            None,
        ));
    }

    if options.bump_policy.require_explicit_major {
        return Some(skipped_release_plan(
            component_id,
            "major-requires-flag",
            "Breaking changes require an explicit major bump",
            &format!(
                "Re-run with: {}",
                force_release_command_hint(component_id, options, "major")
            ),
            semver_recommendation,
        ));
    }

    None
}

/// Build the exact `homeboy release` command the operator should run to force a
/// release, echoing the flags already passed on the current invocation. This is
/// what the issue asks for: when `--skip-checks` was used and the only blocker
/// is `no-releasable-commits`, the hint should be copy-pasteable instead of a
/// vague "Use --bump to force a release".
fn force_release_command_hint(
    component_id: &str,
    options: &ReleaseOptions,
    bump_keyword: &str,
) -> String {
    let mut parts: Vec<String> = vec![
        "homeboy release".to_string(),
        component_id.to_string(),
        format!("--bump {}", bump_keyword),
    ];
    if let Some(ref path) = options.path_override {
        parts.push(format!("--path {}", quote_if_needed(path)));
    }
    if options.skip_checks {
        parts.push("--skip-checks".to_string());
    }
    if options.pipeline.skip_publish {
        parts.push("--skip-publish".to_string());
    }
    if options.skip_github_release {
        parts.push("--no-github-release".to_string());
    }
    if let Some(ref identity) = options.git_identity {
        parts.push(format!("--git-identity {}", quote_if_needed(identity)));
    }
    parts.join(" ")
}

fn quote_if_needed(value: &str) -> String {
    if value.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn skipped_release_plan(
    component_id: &str,
    reason: &str,
    label: &str,
    hint: &str,
    semver_recommendation: Option<ReleaseSemverRecommendation>,
) -> ReleasePlan {
    ReleasePlan::new(
        component_id,
        false,
        vec![
            PlanStep::disabled_with_reason("release.skip", "release.skip", reason)
                .label(label)
                .build(),
        ],
        semver_recommendation,
        Vec::new(),
        vec![hint.to_string()],
    )
}

#[cfg(test)]
mod tests {
    use super::release_skip_plan;
    use crate::core::plan::PlanStepStatus;
    use crate::core::release::types::{
        ReleaseBumpPolicyOptions, ReleaseOptions, ReleaseSemverRecommendation,
    };

    #[test]
    fn test_release_skip_plan() {
        let plan = release_skip_plan("demo", &ReleaseOptions::default(), None, None)
            .expect("no releasable commits should skip");

        assert!(!plan.enabled());
        assert_eq!(plan.component_id(), Some("demo"));
        assert_eq!(plan.plan.steps.len(), 1);
        assert_eq!(plan.plan.steps[0].id, "release.skip");
        assert_eq!(plan.plan.steps[0].kind, "release.skip");
        assert_eq!(plan.plan.steps[0].status, PlanStepStatus::Disabled);
        assert_eq!(
            plan.plan.steps[0]
                .inputs
                .get("reason")
                .and_then(|v| v.as_str()),
            Some("no-releasable-commits")
        );
        let hint = plan
            .plan
            .hints
            .first()
            .expect("skip plan should ship one hint")
            .as_str();
        assert!(
            hint.contains("No release was created"),
            "skip hint should explicitly state no release artifacts were produced: {hint}"
        );
        assert!(
            hint.contains("No tag created"),
            "skip hint should explicitly say no tag was created: {hint}"
        );
        assert!(
            hint.contains("homeboy release demo --bump patch"),
            "skip hint should provide the exact force command: {hint}"
        );
    }

    #[test]
    fn skip_hint_echoes_currently_set_flags_for_docs_only_release() {
        let options = ReleaseOptions {
            path_override: Some("/tmp/dm-worktree".to_string()),
            skip_checks: true,
            ..Default::default()
        };

        let plan = release_skip_plan("data-machine-code", &options, None, None)
            .expect("docs-only invocation should still skip");

        let hint = plan
            .plan
            .hints
            .first()
            .expect("skip plan should ship one hint")
            .as_str();
        assert!(
            hint.contains("--skip-checks"),
            "skip hint should echo --skip-checks: {hint}"
        );
        assert!(
            hint.contains("--path /tmp/dm-worktree"),
            "skip hint should echo --path: {hint}"
        );
        assert!(
            hint.contains("homeboy release data-machine-code --bump patch"),
            "skip hint should provide a fully-qualified force command: {hint}"
        );
    }

    #[test]
    fn skip_plan_allows_forced_empty_release() {
        let options = ReleaseOptions {
            bump_policy: ReleaseBumpPolicyOptions {
                force_empty_release: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(release_skip_plan("demo", &options, None, None).is_none());
    }

    #[test]
    fn skip_plan_records_major_requires_flag_reason() {
        let options = ReleaseOptions {
            bump_policy: ReleaseBumpPolicyOptions {
                require_explicit_major: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let recommendation = semver_recommendation("major", "major");

        let plan = release_skip_plan("demo", &options, Some(recommendation), None)
            .expect("implicit major should skip");

        assert!(!plan.enabled());
        assert!(plan.semver_recommendation().is_some());
        assert_eq!(
            plan.plan.steps[0]
                .inputs
                .get("reason")
                .and_then(|v| v.as_str()),
            Some("major-requires-flag")
        );
        assert_eq!(
            plan.plan.hints,
            vec!["Re-run with: homeboy release demo --bump major"]
        );
    }

    #[test]
    fn major_skip_hint_echoes_currently_set_flags() {
        let options = ReleaseOptions {
            skip_checks: true,
            bump_policy: ReleaseBumpPolicyOptions {
                require_explicit_major: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let recommendation = semver_recommendation("major", "major");

        let plan = release_skip_plan("demo", &options, Some(recommendation), None)
            .expect("implicit major should skip");

        let hint = plan
            .plan
            .hints
            .first()
            .expect("major-requires-flag plan should ship one hint")
            .as_str();
        assert!(
            hint.contains("homeboy release demo --bump major"),
            "major hint should provide a fully-qualified force command: {hint}"
        );
        assert!(
            hint.contains("--skip-checks"),
            "major hint should echo --skip-checks when set: {hint}"
        );
    }

    #[test]
    fn skip_plan_detects_release_already_at_head_before_other_reasons() {
        // Even when there are no releasable commits and a forced bump is set —
        // exactly the state that produced the confusing next-version changelog
        // contract error in issue #4316 — the planner should report
        // "release-already-at-head" as the primary skip reason.
        let options = ReleaseOptions {
            skip_checks: true,
            bump_policy: ReleaseBumpPolicyOptions {
                force_empty_release: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let plan = release_skip_plan("data-machine-code", &options, None, Some("v0.47.121"))
            .expect("tag-already-at-HEAD should produce a skip plan");

        assert!(!plan.enabled());
        assert_eq!(
            plan.plan.steps[0]
                .inputs
                .get("reason")
                .and_then(|v| v.as_str()),
            Some("release-already-at-head"),
            "release-already-at-head takes precedence over no-releasable-commits / forced empty"
        );
        let hint = plan
            .plan
            .hints
            .first()
            .expect("release-already-at-head plan should ship a hint")
            .as_str();
        assert!(
            hint.contains("v0.47.121"),
            "hint should name the existing tag: {hint}"
        );
        assert!(
            hint.contains("--head"),
            "hint should point operator at --head finalization: {hint}"
        );
        assert!(
            hint.contains("homeboy release data-machine-code --head"),
            "hint should provide an actionable command: {hint}"
        );
    }

    fn semver_recommendation(recommended: &str, requested: &str) -> ReleaseSemverRecommendation {
        ReleaseSemverRecommendation {
            latest_tag: Some("v1.0.0".to_string()),
            range: "v1.0.0..HEAD".to_string(),
            commits: vec![],
            recommended_bump: Some(recommended.to_string()),
            requested_bump: requested.to_string(),
            is_underbump: false,
            reasons: vec![],
        }
    }
}
