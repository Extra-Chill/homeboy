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
use homeboy_core::worktree_provider::WorktreeTerminalDisposition;

/// Execute a release end-to-end.
///
/// Runs the executable preflight validations, rebuilds the full release plan
/// after those preflights, then walks the planned release steps in order.
pub fn run(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    // The public convenience entry: callers without roots of their own get one
    // resolution here, which `run_with_plan` then threads through the release.
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    run_with_plan(&roots, component_id, options).map(|(_plan, run, _workspace)| run)
}

/// Execute a release and return the plan that drove it alongside the run.
pub(crate) fn run_with_plan(
    roots: &homeboy_core::paths::PathRoots,
    component_id: &str,
    options: &ReleaseOptions,
) -> Result<(ReleasePlan, ReleaseRun, Option<ReleaseWorkspaceOutput>)> {
    // Roots arrive from the caller and cover the entire release: workspace
    // provisioning and its finalization record, both plan phases, packaging,
    // cleanup, and the deploy checkpoint. A release therefore cannot package
    // into one home and then record its state against another (#7505).
    let mut workspace_options = options.clone();
    let component = super::context::load_component(component_id, options)?;
    let mut control_plane = (!options.dry_run)
        .then(|| super::control_plane::ReleaseControlPlaneObservation::start(roots, component_id))
        .transpose()?;
    workspace_options.control_plane = control_plane.as_ref().map(|run| run.context());
    let mut workspace =
        super::workspace::ReleaseWorkspace::select(roots, &component, options.pipeline.head)?;
    workspace_options.path_override = Some(workspace.component.local_path.clone());
    let checkout_guard =
        super::checkout_guard::ReleaseCheckoutGuard::capture(&workspace.component)?;

    let staging_source_sha = workspace.source_sha();
    match run_with_plan_inner(
        roots,
        component_id,
        &workspace_options,
        checkout_guard.as_ref(),
        staging_source_sha.as_deref(),
    ) {
        Ok((plan, mut run)) => {
            let disposition = if matches!(run.result.status, ReleaseStepStatus::Success) {
                WorktreeTerminalDisposition::Succeeded
            } else {
                WorktreeTerminalDisposition::Failed
            };
            let output = workspace.finalize(disposition, release_was_pushed(&run.result.steps));
            if let Some(error) = &output.finalization_error {
                run.result.warnings.push(format!(
                    "Release completed, but workspace finalization is pending: {error}. Reconcile owner reference `{}`.",
                    output.reconciliation_ref.as_deref().unwrap_or("unavailable")
                ));
            }
            if let Some(control_plane) = control_plane.as_mut() {
                control_plane.finish(&run)?;
            }
            Ok((plan, run, (output.kind != "in_place").then_some(output)))
        }
        Err(err) => {
            // A provisioned workspace must remain recoverable even when checkout
            // rollback itself fails. Attempt both terminal operations and retain
            // both errors rather than silently dropping the provider finalizer.
            let finalization = workspace.finalize(WorktreeTerminalDisposition::Interrupted, false);
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
    roots: &homeboy_core::paths::PathRoots,
    component_id: &str,
    options: &ReleaseOptions,
    checkout_guard: Option<&super::checkout_guard::ReleaseCheckoutGuard>,
    staging_source_sha: Option<&str>,
) -> Result<(ReleasePlan, ReleaseRun)> {
    let mut results: Vec<ReleaseStepResult> = Vec::new();
    ensure_readiness_passed(options)?;

    let initial_plan = build_initial_preflight_plan(component_id, options);
    let mut timer = PhaseTimer::new();
    let initial_stop = timer.time("package_preflight", || {
        super::execution_plan::execute_plan_steps_at_source(
            roots,
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

    // The portable portion of preflight is now complete. Freeze its source
    // identity before building the mutation plan; any later checkout movement
    // invalidates its evidence and must block controller-owned mutation.
    let preflight_component = super::context::load_component(component_id, options)?;
    let preflight_source = options
        .readiness
        .as_ref()
        .map(|readiness| readiness.source.clone())
        .unwrap_or(super::preflight_identity::capture(&preflight_component)?);

    // Rebuild the full plan after executable preflights. `preflight.remote_sync`
    // may fast-forward HEAD and `preflight.changelog_bootstrap` may create the
    // first changelog file; changelog/version planning must observe those
    // changes instead of stale checkout state.
    let release_plan = plan(component_id, options)?;
    let completed_preflights: HashSet<&'static str> =
        initial_executable_preflight_ids().iter().copied().collect();

    super::preflight_identity::revalidate(&preflight_component, &preflight_source)?;
    timer.time("package", || {
        super::execution_plan::execute_plan_steps_at_source(
            roots,
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

fn ensure_readiness_passed(options: &ReleaseOptions) -> Result<()> {
    let Some(readiness) = options.readiness.as_ref() else {
        return Ok(());
    };
    if super::types::readiness_is_valid(readiness) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "release.preflight",
        format!("Portable release preflight has invalid selected gate evidence"),
        Some(readiness.source.commit.clone()),
        Some(
            readiness
                .evidence_refs
                .iter()
                .map(|reference| format!("Inspect durable readiness evidence: {reference}"))
                .collect(),
        ),
    ))
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
        // Restoring HEAD discards the release commit, which leaves any tag this
        // run created pointing at an object no longer reachable from the branch.
        // Reporting that as `local_tag_created` and stopping is what made the
        // next release refuse to run at all: it saw a tag ahead of the source
        // version and unreachable from HEAD, and declined to reconcile an
        // orphaned release identity. Rollback owns undoing what the run did, so
        // delete the tag here rather than leaving an operator to find it (#14577).
        let tag_state = match created_local_tag(run) {
            Some(tag) => match homeboy_core::git::delete_local_tag(checkout_guard.path(), &tag) {
                Ok(output) if output.success => "deleted",
                // A tag that survives is the operator's problem to see, not to
                // discover later through an unrelated failure.
                _ => {
                    run.result.warnings.push(format!(
                        "Release rollback could not delete local tag {tag}; delete it before the next release: git tag -d {tag}"
                    ));
                    "local_tag_retained"
                }
            },
            None => "not_created",
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

/// The tag this run created, if it created one.
///
/// A tag step that skipped because the tag already existed and pointed at HEAD
/// did not create anything, so rollback must leave that tag alone.
fn created_local_tag(run: &ReleaseRun) -> Option<String> {
    run.result
        .steps
        .iter()
        .filter(|step| {
            step.step_type == "git.tag" && matches!(step.status, ReleaseStepStatus::Success)
        })
        .filter_map(|step| step.data.as_ref())
        .find(|data| data.get("skipped").and_then(serde_json::Value::as_bool) != Some(true))
        .and_then(|data| data.get("tag"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn release_was_pushed(steps: &[ReleaseStepResult]) -> bool {
    steps.iter().any(|step| {
        step.step_type == "git.push" && matches!(step.status, ReleaseStepStatus::Success)
    })
}

#[cfg(test)]
mod tests {
    use super::{release_was_pushed, restore_checkout_after_failed_run};
    use crate::release::checkout_guard::ReleaseCheckoutGuard;
    use crate::release::types::{
        ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
    };
    use homeboy_core::component::Component;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q", "--initial-branch", "main"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(dir.join("file.txt"), "main\n").expect("write");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "Initial commit"]);
        temp
    }

    fn tag_step(data: serde_json::Value) -> ReleaseStepResult {
        ReleaseStepResult {
            id: "git.tag".to_string(),
            step_type: "git.tag".to_string(),
            status: ReleaseStepStatus::Success,
            data: Some(data),
            ..Default::default()
        }
    }

    fn failed_run(steps: Vec<ReleaseStepResult>) -> ReleaseRun {
        ReleaseRun {
            component_id: "fixture".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps,
                status: ReleaseStepStatus::Failed,
                warnings: Vec::new(),
                summary: None,
                phase_timings: None,
                rollback: None,
            },
        }
    }

    /// Rolling back discards the release commit, so a tag this run created is
    /// left pointing at an unreachable object. Leaving it behind made the next
    /// release refuse to start at all, seeing a tag ahead of the source version
    /// and unreachable from HEAD.
    #[test]
    fn rollback_deletes_the_tag_the_failed_run_created() {
        let temp = init_repo();
        let dir = temp.path();
        let original_head = git_stdout(dir, &["rev-parse", "HEAD"]);
        let guard = ReleaseCheckoutGuard::capture(&Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("capture")
        .expect("git repo");

        // Stand in for the release: a version commit, then its tag.
        std::fs::write(dir.join("file.txt"), "released\n").expect("write");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "release: v1.2.3"]);
        run_git(dir, &["tag", "-a", "v1.2.3", "-m", "Release v1.2.3"]);
        assert_eq!(git_stdout(dir, &["tag", "-l", "v1.2.3"]), "v1.2.3");

        let mut run = failed_run(vec![tag_step(serde_json::json!({"tag": "v1.2.3"}))]);
        restore_checkout_after_failed_run(Some(&guard), &mut run).expect("rollback");

        assert_eq!(git_stdout(dir, &["rev-parse", "HEAD"]), original_head);
        assert_eq!(
            git_stdout(dir, &["tag", "-l", "v1.2.3"]),
            "",
            "a rolled-back release must not leave its tag behind to block the next one"
        );
        let rollback = run.result.rollback.expect("rollback evidence");
        assert_eq!(rollback.tag_state, "deleted");
    }

    /// A tag the run found already pointing at HEAD is not the run's to remove.
    #[test]
    fn rollback_keeps_a_tag_the_run_did_not_create() {
        let temp = init_repo();
        let dir = temp.path();
        run_git(dir, &["tag", "-a", "v1.2.3", "-m", "Pre-existing"]);
        let guard = ReleaseCheckoutGuard::capture(&Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("capture")
        .expect("git repo");

        let mut run = failed_run(vec![tag_step(
            serde_json::json!({"tag": "v1.2.3", "skipped": true}),
        )]);
        restore_checkout_after_failed_run(Some(&guard), &mut run).expect("rollback");

        assert_eq!(
            git_stdout(dir, &["tag", "-l", "v1.2.3"]),
            "v1.2.3",
            "rollback must not delete a tag that existed before the release ran"
        );
        let rollback = run.result.rollback.expect("rollback evidence");
        assert_eq!(rollback.tag_state, "not_created");
    }

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
