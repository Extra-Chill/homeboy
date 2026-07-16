//! Release execution orchestration.
//!
//! The planner builds the `ReleasePlan`; this module runs that plan and wraps
//! the accumulated step results into the public release run shape.

use crate::error::Result;
use crate::phase_timing::PhaseTimer;
use std::collections::HashSet;

use super::execution_plan::{
    build_initial_preflight_plan, execute_plan_steps, initial_executable_preflight_ids,
};
use super::pipeline_summary::{build_summary, derive_overall_status};
use super::planner::plan;
use super::types::{
    ReleaseOptions, ReleasePlan, ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
};

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
    let component = super::context::load_component(component_id, options)?;
    let checkout_guard = super::checkout_guard::ReleaseCheckoutGuard::capture(&component)?;

    match run_with_plan_inner(component_id, options, checkout_guard.as_ref()) {
        Ok(result) => Ok(result),
        Err(err) => {
            if let Some(checkout_guard) = checkout_guard.as_ref() {
                checkout_guard.restore_after_failure()?;
            }
            Err(err)
        }
    }
}

fn run_with_plan_inner(
    component_id: &str,
    options: &ReleaseOptions,
    checkout_guard: Option<&super::checkout_guard::ReleaseCheckoutGuard>,
) -> Result<(ReleasePlan, ReleaseRun)> {
    let mut results: Vec<ReleaseStepResult> = Vec::new();

    let initial_plan = build_initial_preflight_plan(component_id, options);
    let mut timer = PhaseTimer::new();
    let initial_stop = timer.time("package_preflight", || {
        execute_plan_steps(
            &initial_plan.plan.steps,
            component_id,
            options,
            &mut results,
            &HashSet::new(),
        )
    })?;

    if initial_stop {
        let run = finalize(component_id, results, timer.into_report());
        restore_checkout_after_failed_run(checkout_guard, &run)?;
        return Ok((initial_plan, run));
    }

    // Rebuild the full plan after executable preflights. `preflight.remote_sync`
    // may fast-forward HEAD and `preflight.changelog_bootstrap` may create the
    // first changelog file; changelog/version planning must observe those
    // changes instead of stale checkout state.
    let release_plan = plan(component_id, options)?;
    let completed_preflights: HashSet<&'static str> =
        initial_executable_preflight_ids().iter().copied().collect();

    timer.time("package", || {
        execute_plan_steps(
            &release_plan.plan.steps,
            component_id,
            options,
            &mut results,
            &completed_preflights,
        )
    })?;

    let run = finalize(component_id, results, timer.into_report());
    restore_checkout_after_failed_run(checkout_guard, &run)?;

    Ok((release_plan, run))
}

/// Wrap the accumulated step results into a `ReleaseRun` with an overall
/// status and a human-friendly summary.
fn finalize(
    component_id: &str,
    results: Vec<ReleaseStepResult>,
    phase_timings: crate::phase_timing::PhaseTimingReport,
) -> ReleaseRun {
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
            phase_timings: Some(phase_timings),
        },
    }
}

fn restore_checkout_after_failed_run(
    checkout_guard: Option<&super::checkout_guard::ReleaseCheckoutGuard>,
    run: &ReleaseRun,
) -> Result<()> {
    if matches!(run.result.status, ReleaseStepStatus::Success) {
        return Ok(());
    }

    if let Some(checkout_guard) = checkout_guard {
        checkout_guard.restore_after_failure()?;
    }

    Ok(())
}
