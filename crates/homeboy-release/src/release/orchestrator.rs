//! Release execution orchestration.
//!
//! The planner builds the `ReleasePlan`; this module runs that plan and wraps
//! the accumulated step results into the public release run shape.

use homeboy_core::error::{Error, Result};
use homeboy_core::phase_timing::PhaseTimer;
use std::collections::HashSet;

use super::execution_plan::{build_initial_preflight_plan, initial_executable_preflight_ids};
use super::pipeline_summary::{build_summary, derive_overall_status};
use super::planner::plan;
use super::types::{
    ReleaseOptions, ReleasePlan, ReleaseRollbackEvidence, ReleaseRun, ReleaseRunResult,
    ReleaseStepResult, ReleaseStepStatus, ReleaseWorkspaceOutput,
};
use homeboy_core::worktree_providers::WorktreeProviderTerminalDisposition;

/// Execute a release end-to-end.
///
/// Runs the executable preflight validations, rebuilds the full release plan
/// after those preflights, then walks the planned release steps in order.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    run_with_plan(component_id, options).map(|(_plan, run, _workspace)| run)
}

/// Execute a release and return the plan that drove it alongside the run.
pub(crate) fn run_with_plan(
    component_id: &str,
    options: &ReleaseOptions,
) -> Result<(ReleasePlan, ReleaseRun, Option<ReleaseWorkspaceOutput>)> {
    let component = super::context::load_component(component_id, options)?;
    let mut workspace = super::workspace::ReleaseWorkspace::select(&component)?;
    let mut workspace_options = options.clone();
    workspace_options.path_override = Some(workspace.component.local_path.clone());
    let checkout_guard =
        super::checkout_guard::ReleaseCheckoutGuard::capture(&workspace.component)?;

    let staging_source_sha = workspace.source_sha();
    match run_with_plan_inner(
        component_id,
        &workspace_options,
        checkout_guard.as_ref(),
        staging_source_sha.as_deref(),
    ) {
        Ok((plan, mut run)) => {
            let disposition = if matches!(run.result.status, ReleaseStepStatus::Success) {
                WorktreeProviderTerminalDisposition::Succeeded
            } else {
                WorktreeProviderTerminalDisposition::Failed
            };
            let output = workspace.finalize(disposition, release_was_pushed(&run.result.steps));
            if let Some(error) = &output.finalization_error {
                run.result.warnings.push(format!(
                    "Release completed, but workspace finalization is pending: {error}. Reconcile owner reference `{}`.",
                    output.reconciliation_ref.as_deref().unwrap_or("unavailable")
                ));
            }
            Ok((plan, run, (output.kind != "in_place").then_some(output)))
        }
        Err(err) => {
            // A provisioned workspace must remain recoverable even when checkout
            // rollback itself fails. Attempt both terminal operations and retain
            // both errors rather than silently dropping the provider finalizer.
            let finalization =
                workspace.finalize(WorktreeProviderTerminalDisposition::Interrupted, false);
            let finalization_error = finalization.finalization_error;
            let restore_error = checkout_guard
                .as_ref()
                .map(|guard| guard.restore_after_failure())
                .transpose()
                .err();
            if finalization_error.is_none() && restore_error.is_none() {
                return Err(err);
            }
            Err(Error::validation_invalid_argument(
                "release.workspace",
                format!(
                    "{err}; checkout restoration: {}; workspace finalization: {}",
                    restore_error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "completed".to_string()),
                    finalization_error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "completed".to_string()),
                ),
                None,
                None,
            ))
        }
    }
}

fn run_with_plan_inner(
    component_id: &str,
    options: &ReleaseOptions,
    checkout_guard: Option<&super::checkout_guard::ReleaseCheckoutGuard>,
    staging_source_sha: Option<&str>,
) -> Result<(ReleasePlan, ReleaseRun)> {
    let mut results: Vec<ReleaseStepResult> = Vec::new();

    let initial_plan = build_initial_preflight_plan(component_id, options);
    let mut timer = PhaseTimer::new();
    let initial_stop = timer.time("package_preflight", || {
        super::execution_plan::execute_plan_steps_at_source(
            &initial_plan.plan.steps,
            component_id,
            options,
            &mut results,
            &HashSet::new(),
            staging_source_sha,
        )
    })?;

    if initial_stop {
        let mut run = finalize(component_id, results, timer.into_report());
        restore_checkout_after_failed_run(checkout_guard, &mut run)?;
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
        super::execution_plan::execute_plan_steps_at_source(
            &release_plan.plan.steps,
            component_id,
            options,
            &mut results,
            &completed_preflights,
            staging_source_sha,
        )
    })?;

    let mut run = finalize(component_id, results, timer.into_report());
    restore_checkout_after_failed_run(checkout_guard, &mut run)?;

    Ok((release_plan, run))
}

/// Wrap the accumulated step results into a `ReleaseRun` with an overall
/// status and a human-friendly summary.
fn finalize(
    component_id: &str,
    results: Vec<ReleaseStepResult>,
    phase_timings: homeboy_core::phase_timing::PhaseTimingReport,
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
            rollback: None,
        },
    }
}

fn restore_checkout_after_failed_run(
    checkout_guard: Option<&super::checkout_guard::ReleaseCheckoutGuard>,
    run: &mut ReleaseRun,
) -> Result<()> {
    if matches!(run.result.status, ReleaseStepStatus::Success)
        || release_was_pushed(&run.result.steps)
    {
        return Ok(());
    }

    if let Some(checkout_guard) = checkout_guard {
        let evidence = checkout_guard.restore_after_failure()?;
        let restored = evidence.restored;
        let recovery_action =
            (!restored).then(|| format!("homeboy release {} --apply", run.component_id));
        let tag_state = if run.result.steps.iter().any(|step| {
            step.step_type == "git.tag" && matches!(step.status, ReleaseStepStatus::Success)
        }) {
            "local_tag_created"
        } else {
            "not_created"
        };
        run.result.rollback = Some(ReleaseRollbackEvidence {
            status: if restored { "restored" } else { "interrupted" }.to_string(),
            original_head: evidence.original_head,
            release_commit: evidence.temporary_head.clone(),
            temporary_head: evidence.temporary_head,
            final_head: evidence.final_head,
            tag_state: tag_state.to_string(),
            error: evidence.error,
            recovery_action: recovery_action.clone(),
        });
        if let Some(summary) = &mut run.result.summary {
            if let Some(action) = recovery_action {
                summary.next_actions.push(action);
            } else {
                summary.next_actions.push(
                    "Inspect remote branch and tag state before retrying: git ls-remote --heads --tags origin"
                        .to_string(),
                );
            }
        }
        if !restored {
            run.result.status = ReleaseStepStatus::Failed;
            run.result.warnings.push(
                "Release rollback was interrupted; checkout recovery is still required".to_string(),
            );
        }
    }

    Ok(())
}

fn release_was_pushed(steps: &[ReleaseStepResult]) -> bool {
    steps.iter().any(|step| {
        step.step_type == "git.push" && matches!(step.status, ReleaseStepStatus::Success)
    })
}

#[cfg(test)]
mod tests {
    use super::release_was_pushed;
    use crate::release::types::{ReleaseStepResult, ReleaseStepStatus};

    #[test]
    fn published_push_prevents_checkout_rollback_after_deploy_failure() {
        assert!(release_was_pushed(&[
            ReleaseStepResult {
                id: "git.push".to_string(),
                step_type: "git.push".to_string(),
                status: ReleaseStepStatus::Success,
                ..Default::default()
            },
            ReleaseStepResult {
                id: "deploy".to_string(),
                step_type: "deploy".to_string(),
                status: ReleaseStepStatus::Failed,
                ..Default::default()
            },
        ]));
    }
}
