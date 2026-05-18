use std::collections::HashSet;

use crate::core::error::Result;
use crate::core::plan::PlanStep;

use super::context::{load_component, resolve_extensions};
use super::execution_dispatch::{
    execute_release_plan_step, release_step_is_show_stopper, ReleaseExecutionContext,
};
use super::plan_steps::build_preflight_steps;
use super::types::{ReleaseOptions, ReleasePlan, ReleaseState, ReleaseStepResult};

pub(super) fn build_initial_preflight_plan(
    component_id: &str,
    options: &ReleaseOptions,
) -> ReleasePlan {
    let steps = build_preflight_steps(options, None)
        .into_iter()
        .filter(|step| initial_executable_preflight_ids().contains(&step.id.as_str()))
        .collect();

    ReleasePlan::new(component_id, true, steps, None, Vec::new(), Vec::new())
}

pub(super) fn initial_executable_preflight_ids() -> &'static [&'static str] {
    &[
        "preflight.default_branch",
        "preflight.git_identity",
        "preflight.working_tree",
        "preflight.remote_sync",
        "preflight.lint",
        "preflight.test",
        "preflight.changelog_bootstrap",
    ]
}

pub(super) fn execute_plan_steps(
    steps: &[PlanStep],
    component_id: &str,
    options: &ReleaseOptions,
    results: &mut Vec<ReleaseStepResult>,
    skip_step_ids: &HashSet<&'static str>,
) -> Result<bool> {
    if steps.is_empty() {
        return Ok(false);
    }

    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component)?;
    let mut context = ReleaseExecutionContext {
        component: &component,
        extensions: &extensions,
        component_id,
        options,
        state: ReleaseState::default(),
        publish_failed: false,
    };

    for step in steps {
        if skip_step_ids.contains(step.id.as_str()) {
            continue;
        }

        if let Some(result) = execute_release_plan_step(step, &mut context)? {
            let should_stop = release_step_is_show_stopper(&result);
            results.push(result);
            if should_stop {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{
        build_initial_preflight_plan, execute_plan_steps, initial_executable_preflight_ids,
    };
    use crate::core::plan::PlanStepStatus;
    use crate::core::release::types::ReleaseOptions;
    use std::collections::HashSet;

    #[test]
    fn test_build_initial_preflight_plan() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };

        let plan = build_initial_preflight_plan("fixture", &options);
        let ids: Vec<&str> = plan
            .plan
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect();

        assert_eq!(ids, initial_executable_preflight_ids().to_vec());
        assert!(plan.semver_recommendation().is_none());
        assert!(plan
            .plan
            .steps
            .iter()
            .any(|step| step.id == "preflight.git_identity"
                && step.status == PlanStepStatus::Disabled));
    }

    #[test]
    fn test_initial_executable_preflight_ids() {
        assert_eq!(
            initial_executable_preflight_ids(),
            &[
                "preflight.default_branch",
                "preflight.git_identity",
                "preflight.working_tree",
                "preflight.remote_sync",
                "preflight.lint",
                "preflight.test",
                "preflight.changelog_bootstrap",
            ]
        );
    }

    #[test]
    fn test_execute_plan_steps() {
        let mut results = Vec::new();

        let stopped = execute_plan_steps(
            &[],
            "missing-component-is-not-loaded-for-empty-plan",
            &ReleaseOptions::default(),
            &mut results,
            &HashSet::new(),
        )
        .expect("empty plan should be a no-op");

        assert!(!stopped);
        assert!(results.is_empty());
    }
}
