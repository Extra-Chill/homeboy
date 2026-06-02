//! Release execution orchestration.
//!
//! The planner builds the `ReleasePlan`; this module runs that plan and wraps
//! the accumulated step results into the public release run shape.

use crate::core::error::Result;
use std::collections::HashSet;

use super::execution_plan::{
    build_initial_preflight_plan, execute_plan_steps, initial_executable_preflight_ids,
};
use super::pipeline_summary::{build_summary, derive_overall_status};
use super::planner::plan;
use super::types::{ReleaseOptions, ReleasePlan, ReleaseRun, ReleaseRunResult, ReleaseStepResult};

/// Execute a release end-to-end.
///
/// Runs the executable preflight validations, rebuilds the full release plan
/// after those preflights, then walks the planned release steps in order.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    run_with_plan(component_id, options).map(|(_plan, run)| run)
}

/// Execute a release and return the plan that drove it alongside the run.
pub(crate) fn run_with_plan(
    component_id: &str,
    options: &ReleaseOptions,
) -> Result<(ReleasePlan, ReleaseRun)> {
    let mut results: Vec<ReleaseStepResult> = Vec::new();

    let initial_plan = build_initial_preflight_plan(component_id, options);
    let initial_stop = execute_plan_steps(
        &initial_plan.plan.steps,
        component_id,
        options,
        &mut results,
        &HashSet::new(),
    )?;

    if initial_stop {
        return Ok((initial_plan, finalize(component_id, results)));
    }

    // Rebuild the full plan after executable preflights. `preflight.remote_sync`
    // may fast-forward HEAD and `preflight.changelog_bootstrap` may create the
    // first changelog file; changelog/version planning must observe those
    // changes instead of stale checkout state.
    let release_plan = plan(component_id, options)?;
    let completed_preflights: HashSet<&'static str> =
        initial_executable_preflight_ids().iter().copied().collect();

    execute_plan_steps(
        &release_plan.plan.steps,
        component_id,
        options,
        &mut results,
        &completed_preflights,
    )?;

    Ok((release_plan, finalize(component_id, results)))
}

/// Wrap the accumulated step results into a `ReleaseRun` with an overall
/// status and a human-friendly summary.
fn finalize(component_id: &str, results: Vec<ReleaseStepResult>) -> ReleaseRun {
    let status = derive_overall_status(&results);
    let summary = build_summary(component_id, &results, &status);

    ReleaseRun {
        component_id: component_id.to_string(),
        enabled: true,
        result: ReleaseRunResult {
            steps: results,
            status,
            warnings: Vec::new(),
            summary: Some(summary),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{run, run_with_plan};
    use crate::core::release::types::ReleaseOptions;

    #[test]
    fn test_run() {
        let err = run(
            "missing-component-is-reported-by-orchestrator",
            &ReleaseOptions::default(),
        )
        .expect_err("orchestrator should report missing components");

        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn test_run_with_plan() {
        let err = run_with_plan(
            "missing-component-is-reported-by-orchestrator-plan-run",
            &ReleaseOptions::default(),
        )
        .expect_err("orchestrator should report missing components before returning a plan/run");

        assert!(!err.to_string().is_empty());
    }
}
