use crate::core::plan::PlanStep;

use super::types::{ReleaseOptions, ReleasePlan, ReleaseSemverRecommendation};

pub(super) fn release_skip_plan(
    component_id: &str,
    options: &ReleaseOptions,
    semver_recommendation: Option<ReleaseSemverRecommendation>,
) -> Option<ReleasePlan> {
    if semver_recommendation.is_none() && !options.bump_policy.force_empty_release {
        return Some(skipped_release_plan(
            component_id,
            "no-releasable-commits",
            "No releasable commits since last tag",
            "Use --bump to force a release when this is intentional",
            None,
        ));
    }

    if options.bump_policy.require_explicit_major {
        return Some(skipped_release_plan(
            component_id,
            "major-requires-flag",
            "Breaking changes require an explicit major bump",
            &format!("Re-run with: homeboy release {} --bump major", component_id),
            semver_recommendation,
        ));
    }

    None
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
        let plan = release_skip_plan("demo", &ReleaseOptions::default(), None)
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
        assert_eq!(
            plan.plan.hints,
            vec!["Use --bump to force a release when this is intentional"]
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

        assert!(release_skip_plan("demo", &options, None).is_none());
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

        let plan = release_skip_plan("demo", &options, Some(recommendation))
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
