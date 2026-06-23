use super::builders::{ready_step, StepConfig};
use crate::core::plan::PlanStep;
use crate::core::release::types::ReleaseChangelogPlan;

pub(super) fn build_changelog_steps(
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
