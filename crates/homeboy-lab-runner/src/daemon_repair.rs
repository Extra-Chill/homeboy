//! Controller-frame daemon repair steps.
//!
//! A daemon composes its own `repair_plan` in its own local frame
//! (`homeboy daemon stop`). That is correct for `homeboy daemon status` on the
//! machine running the daemon, and wrong the moment the report crosses an SSH
//! boundary: running `homeboy daemon stop` on the controller stops the
//! *controller's* daemon, not the runner's. Every plan a lab-runner constructor
//! emits is therefore restated here in controller-side `homeboy runner`
//! commands bound to an explicit runner id.
//!
//! Each builder produces an [`ExecutableAction`] — argv, not text. The command
//! string a report carries is rendered from that argv, so the `adoption_command`
//! a report advertises, the `repair_plan` step an operator reads, and the
//! arguments an executor would run can never drift apart, and no consumer has to
//! parse a rendered shell command back into arguments to act on it (#11103).

use homeboy_core::daemon::{DaemonFreshnessReport, DaemonRepairStep, DaemonStaleReasonCode};
use homeboy_core::error::{ActionSafety, ExecutableAction};

/// The step codes a repair executor dispatches on.
///
/// Public because dispatch crosses a crate boundary: `homeboy runner doctor
/// --repair` lives in the CLI and executes the plan this crate composed. The
/// code is the contract — an executor matches on it and supplies typed values
/// from the report, and never parses the rendered command string (#11103).
pub mod codes {
    pub const RUNNER_DISCONNECT: &str = "runner_disconnect";
    pub const RUNNER_CONNECT: &str = "runner_connect";
    pub const RUNNER_REFRESH_HOMEBOY: &str = "runner_refresh_homeboy";
    pub const RUNNER_ADOPT_ORPHAN_LEASE: &str = "runner_adopt_orphan_lease";
    pub const RUNNER_RECONCILE_LEASELESS_ORPHANS: &str = "runner_reconcile_leaseless_orphans";
    /// A recovery command a runner advertised as text. It has no argv behind it,
    /// so it is surfaced to the operator rather than executed.
    pub const STALE_DAEMON_RECOVERY: &str = "stale_daemon_recovery";
    /// The read-only step emitted when the evidence authorizes no mutation. A
    /// report matching no repair branch must still hand back something to run.
    pub const RUNNER_DIAGNOSE: &str = "runner_diagnose";
}

pub(crate) use codes::{
    RUNNER_ADOPT_ORPHAN_LEASE, RUNNER_CONNECT, RUNNER_DIAGNOSE, RUNNER_DISCONNECT,
    RUNNER_RECONCILE_LEASELESS_ORPHANS, RUNNER_REFRESH_HOMEBOY, STALE_DAEMON_RECOVERY,
};

/// A step known only as text. Nothing may execute it implicitly.
pub(crate) fn step(code: &str, command: String) -> DaemonRepairStep {
    DaemonRepairStep::text(code, command)
}

/// A step whose argv is authoritative; its text is rendered from the action.
pub(crate) fn action_step(code: &str, action: ExecutableAction) -> DaemonRepairStep {
    DaemonRepairStep::executable(code, action)
}

fn runner_action(
    id: &str,
    label: String,
    args: impl IntoIterator<Item = String>,
    safety: ActionSafety,
) -> ExecutableAction {
    ExecutableAction::new(id, label, "homeboy", args, safety)
}

/// `homeboy runner disconnect <id>`.
pub(crate) fn disconnect_action(runner_id: &str) -> ExecutableAction {
    runner_action(
        "runner.disconnect",
        format!("disconnect runner {runner_id}"),
        [
            "runner".to_string(),
            "disconnect".to_string(),
            runner_id.to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// `homeboy runner connect <id>`.
pub(crate) fn connect_action(runner_id: &str) -> ExecutableAction {
    runner_action(
        "runner.connect",
        format!("connect runner {runner_id}"),
        [
            "runner".to_string(),
            "connect".to_string(),
            runner_id.to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// `homeboy runner connect <id> --adopt-orphan-lease <lease> --confirm-pid-dead`.
pub(crate) fn adopt_orphan_lease_action(runner_id: &str, lease_id: &str) -> ExecutableAction {
    runner_action(
        "runner.adopt_orphan_lease",
        format!("adopt proven-dead lease {lease_id} on runner {runner_id}"),
        [
            "runner".to_string(),
            "connect".to_string(),
            runner_id.to_string(),
            "--adopt-orphan-lease".to_string(),
            lease_id.to_string(),
            "--confirm-pid-dead".to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// `homeboy runner connect <id> --reconcile-leaseless-orphans --confirm-no-daemon-owner`.
pub(crate) fn reconcile_leaseless_orphans_action(runner_id: &str) -> ExecutableAction {
    runner_action(
        "runner.reconcile_leaseless_orphans",
        format!("reconcile lease-less durable jobs on runner {runner_id}"),
        [
            "runner".to_string(),
            "connect".to_string(),
            runner_id.to_string(),
            "--reconcile-leaseless-orphans".to_string(),
            "--confirm-no-daemon-owner".to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// `homeboy runner refresh-homeboy <id> --reconnect`.
pub(crate) fn refresh_homeboy_action(runner_id: &str) -> ExecutableAction {
    refresh_homeboy_action_for_ref(runner_id, None)
}

/// `homeboy runner refresh-homeboy <id> [--ref <ref>] --reconnect`.
///
/// The recovery ref, when one is known, names the exact commit the runner's
/// configured job binary should be rebuilt at. Reconnecting the same drifted
/// binary would only reproduce the mismatch.
pub(crate) fn refresh_homeboy_action_for_ref(
    runner_id: &str,
    recovery_ref: Option<&str>,
) -> ExecutableAction {
    let mut args = vec![
        "runner".to_string(),
        "refresh-homeboy".to_string(),
        runner_id.to_string(),
    ];
    if let Some(recovery_ref) = recovery_ref {
        args.push("--ref".to_string());
        args.push(recovery_ref.to_string());
    }
    args.push("--reconnect".to_string());
    runner_action(
        "runner.refresh_homeboy",
        format!("refresh runner {runner_id}"),
        args,
        ActionSafety::Mutating,
    )
}

/// `homeboy runner refresh-homeboy <id> --ref <commit> --reconnect --allow-downgrade`.
///
/// Controller convergence can require moving the runner *back* to the
/// controller's commit, which the ordinary refresh refuses.
pub(crate) fn refresh_homeboy_downgrade_action(
    runner_id: &str,
    controller_commit: &str,
) -> ExecutableAction {
    let mut action = refresh_homeboy_action_for_ref(runner_id, Some(controller_commit));
    action.args.push("--allow-downgrade".to_string());
    action
}

/// `homeboy runner doctor <id> --scope lab-offload`.
///
/// Read-only, and deliberately not a repair: it is what an operator (or an
/// automated repairer) should run when the report authorizes no mutation.
pub(crate) fn diagnose_action(runner_id: &str) -> ExecutableAction {
    runner_action(
        "runner.doctor",
        format!("re-probe runner {runner_id} daemon evidence"),
        [
            "runner".to_string(),
            "doctor".to_string(),
            runner_id.to_string(),
            "--scope".to_string(),
            "lab-offload".to_string(),
        ],
        ActionSafety::ReadOnly,
    )
}

/// The last-resort controller-side repair: drop the session and rebuild it.
///
/// This is the plan used when nothing specific is known about the runner's
/// daemon, so it stays deliberately generic.
pub(crate) fn reconnect_plan(runner_id: &str) -> Vec<DaemonRepairStep> {
    vec![
        action_step(RUNNER_DISCONNECT, disconnect_action(runner_id)),
        action_step(RUNNER_CONNECT, connect_action(runner_id)),
    ]
}

/// Restate a daemon-authored freshness report's repair plan in controller frame.
///
/// The daemon's own plan is discarded rather than translated step by step: the
/// controller cannot execute `homeboy daemon *` against a remote lease, and only
/// the typed evidence in the report (stale reason, lease, PID, durable job
/// count) is frame-independent enough to rebuild an action from.
pub(crate) fn controller_frame_plan(
    runner_id: &str,
    report: &DaemonFreshnessReport,
) -> Vec<DaemonRepairStep> {
    if report.fresh && report.stale_reason_code.is_none() {
        return Vec::new();
    }
    // Adoption eligibility is authored by the daemon after its process and
    // candidate checks. `PidDead` alone is insufficient once the report has
    // crossed the runner boundary: a conflicting candidate deliberately clears
    // this command and must not have it reconstructed here.
    if report.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
        && report.adoption_command.is_some()
    {
        if let Some(lease_id) = report.lease_id.as_deref().filter(|_| report.pid.is_some()) {
            return vec![action_step(
                RUNNER_ADOPT_ORPHAN_LEASE,
                adopt_orphan_lease_action(runner_id, lease_id),
            )];
        }
    }
    // Durable jobs outlived the lease that owned them. Reconciliation is the
    // only action that can retire them, and it is the same explicit,
    // operator-confirmed command the disconnected remote-recovery path emits.
    if report.active_jobs > 0
        && matches!(
            report.stale_reason_code,
            Some(
                DaemonStaleReasonCode::LeaseMissing
                    | DaemonStaleReasonCode::LeaseCorrupt
                    | DaemonStaleReasonCode::VersionMismatch
            )
        )
    {
        return vec![action_step(
            RUNNER_RECONCILE_LEASELESS_ORPHANS,
            reconcile_leaseless_orphans_action(runner_id),
        )];
    }
    // An identity drift is a binary problem, not a lease problem: reconnecting
    // the same runner-side binary would reproduce the mismatch.
    if matches!(
        report.stale_reason_code,
        Some(
            DaemonStaleReasonCode::VersionMismatch
                | DaemonStaleReasonCode::BuildIdentityMismatch
                | DaemonStaleReasonCode::BinaryHashMismatch
        )
    ) {
        return vec![action_step(
            RUNNER_REFRESH_HOMEBOY,
            refresh_homeboy_action(runner_id),
        )];
    }
    if report.restartable {
        return reconnect_plan(runner_id);
    }
    // A stale report that matched no branch above named a real problem and no
    // authorized mutation. Returning an empty plan hands the operator nothing
    // at all, so the honest answer is the read-only re-probe: the evidence that
    // would let a later pass match a branch (#11103).
    vec![action_step(RUNNER_DIAGNOSE, diagnose_action(runner_id))]
}

/// Render a plan as the `&&`-joined shell text used in operator prose.
///
/// Structured steps are the contract; this is presentation only.
pub(crate) fn render(steps: &[DaemonRepairStep]) -> String {
    steps
        .iter()
        .map(|step| step.command.as_str())
        .collect::<Vec<_>>()
        .join(" && ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        stale_reason_code: Option<DaemonStaleReasonCode>,
        active_jobs: usize,
        restartable: bool,
    ) -> DaemonFreshnessReport {
        DaemonFreshnessReport {
            fresh: stale_reason_code.is_none(),
            stale_reason_code,
            restartable,
            lease_id: Some("lease-remote".to_string()),
            pid: Some(4545),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs,
            termination_evidence: None,
            // A daemon-authored plan in the daemon's own frame. It must never
            // survive the crossing: `homeboy daemon stop` on the controller
            // stops the controller's daemon.
            repair_plan: vec![
                step("daemon_stop", "homeboy daemon stop".to_string()),
                step("daemon_start", "homeboy daemon start".to_string()),
            ],
        }
    }

    fn plan(report: &DaemonFreshnessReport) -> Vec<(String, String)> {
        controller_frame_plan("homeboy-lab", report)
            .into_iter()
            .map(|step| (step.code, step.command))
            .collect()
    }

    #[test]
    fn a_fresh_report_needs_no_repair() {
        assert!(plan(&report(None, 0, false)).is_empty());
    }

    #[test]
    fn a_dead_lease_becomes_an_explicit_runner_side_adoption() {
        let mut report = report(Some(DaemonStaleReasonCode::PidDead), 1, false);
        report.adoption_command =
            Some("homeboy daemon adopt-orphan --lease-id lease-remote".to_string());
        assert_eq!(
            plan(&report),
            vec![(
                RUNNER_ADOPT_ORPHAN_LEASE.to_string(),
                "homeboy runner connect homeboy-lab --adopt-orphan-lease lease-remote --confirm-pid-dead".to_string()
            )]
        );
    }

    #[test]
    fn a_dead_lease_with_conflicting_candidates_does_not_rebuild_adoption() {
        assert!(
            plan(&report(Some(DaemonStaleReasonCode::PidDead), 0, false))
                .iter()
                .all(|(code, _)| code != RUNNER_ADOPT_ORPHAN_LEASE)
        );
    }

    #[test]
    fn durable_jobs_without_an_owning_lease_become_explicit_reconciliation() {
        assert_eq!(
            plan(&report(Some(DaemonStaleReasonCode::LeaseMissing), 2, false)),
            vec![(
                RUNNER_RECONCILE_LEASELESS_ORPHANS.to_string(),
                "homeboy runner connect homeboy-lab --reconcile-leaseless-orphans --confirm-no-daemon-owner".to_string()
            )]
        );
    }

    #[test]
    fn identity_drift_refreshes_the_runner_binary_rather_than_reconnecting_it() {
        assert_eq!(
            plan(&report(
                Some(DaemonStaleReasonCode::BuildIdentityMismatch),
                0,
                true
            )),
            vec![(
                RUNNER_REFRESH_HOMEBOY.to_string(),
                "homeboy runner refresh-homeboy homeboy-lab --reconnect".to_string()
            )]
        );
    }

    #[test]
    fn a_restartable_lease_rebuilds_the_controller_session() {
        assert_eq!(
            plan(&report(
                Some(DaemonStaleReasonCode::RuntimePathsDrift),
                0,
                true
            )),
            vec![
                (
                    RUNNER_DISCONNECT.to_string(),
                    "homeboy runner disconnect homeboy-lab".to_string()
                ),
                (
                    RUNNER_CONNECT.to_string(),
                    "homeboy runner connect homeboy-lab".to_string()
                ),
            ]
        );
    }

    /// #11103: a report that matches no repair branch used to return an empty
    /// plan, so the operator was handed a stale daemon and nothing to do about
    /// it. The fallback is read-only rather than a guessed mutation.
    #[test]
    fn a_report_matching_no_branch_still_produces_an_actionable_step() {
        let plan = controller_frame_plan(
            "homeboy-lab",
            &report(Some(DaemonStaleReasonCode::TransportUnreachable), 0, false),
        );

        assert_eq!(plan.len(), 1, "an unmatched stale report is never empty");
        assert_eq!(plan[0].code, RUNNER_DIAGNOSE);
        assert_eq!(
            plan[0].command,
            "homeboy runner doctor homeboy-lab --scope lab-offload"
        );
        let action = plan[0].action.as_ref().expect("fallback carries argv");
        assert_eq!(action.safety, ActionSafety::ReadOnly);
        assert_eq!(action.args[0], "runner");
        assert_eq!(action.args[1], "doctor");
    }

    /// Every branch must carry argv, or the executor is back to shell-parsing
    /// the rendered text it was handed.
    #[test]
    fn every_controller_frame_step_carries_executable_argv() {
        for code in [
            DaemonStaleReasonCode::PidDead,
            DaemonStaleReasonCode::LeaseMissing,
            DaemonStaleReasonCode::LeaseCorrupt,
            DaemonStaleReasonCode::VersionMismatch,
            DaemonStaleReasonCode::BuildIdentityMismatch,
            DaemonStaleReasonCode::BinaryHashMismatch,
            DaemonStaleReasonCode::RuntimePathsDrift,
            DaemonStaleReasonCode::TransportUnreachable,
            DaemonStaleReasonCode::LeaseSchemaMismatch,
        ] {
            for restartable in [true, false] {
                for active_jobs in [0, 1] {
                    let mut report = report(Some(code), active_jobs, restartable);
                    report.adoption_command = Some("daemon-authored".to_string());
                    let plan = controller_frame_plan("homeboy-lab", &report);
                    assert!(!plan.is_empty(), "{code:?} produced no step at all");
                    for step in plan {
                        let action = step.action.as_ref().unwrap_or_else(|| {
                            panic!("{code:?} step {} carries no argv", step.code)
                        });
                        assert_eq!(
                            step.command,
                            action.render_command(),
                            "{code:?} step {} rendered text drifted from its argv",
                            step.code
                        );
                        assert_eq!(action.program, "homeboy");
                        assert_eq!(action.args.first().map(String::as_str), Some("runner"));
                    }
                }
            }
        }
    }

    #[test]
    fn a_daemon_authored_local_frame_plan_never_survives_the_crossing() {
        for code in [
            DaemonStaleReasonCode::PidDead,
            DaemonStaleReasonCode::LeaseMissing,
            DaemonStaleReasonCode::VersionMismatch,
            DaemonStaleReasonCode::RuntimePathsDrift,
            DaemonStaleReasonCode::TransportUnreachable,
        ] {
            for command in plan(&report(Some(code), 1, true))
                .into_iter()
                .map(|(_, command)| command)
            {
                assert!(
                    command.starts_with("homeboy runner "),
                    "{code:?} produced a non-controller-frame command: {command}"
                );
            }
        }
    }

    #[test]
    fn render_matches_the_operator_prose_format() {
        assert_eq!(
            render(&reconnect_plan("homeboy-lab")),
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        );
    }
}
