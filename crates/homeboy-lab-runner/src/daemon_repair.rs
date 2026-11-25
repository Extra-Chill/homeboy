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
//! string a report carries is rendered from that argv, so the `repair_plan`
//! step an operator reads and the arguments an executor would run can never drift apart, and no consumer has to
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
    /// A recovery command a runner advertised as text. It has no argv behind it,
    /// so it is surfaced to the operator rather than executed.
    pub const STALE_DAEMON_RECOVERY: &str = "stale_daemon_recovery";
    /// The read-only step emitted when the evidence authorizes no mutation. A
    /// report matching no repair branch must still hand back something to run.
    pub const RUNNER_DIAGNOSE: &str = "runner_diagnose";
}

pub(crate) use codes::{
    RUNNER_CONNECT, RUNNER_DIAGNOSE, RUNNER_DISCONNECT, RUNNER_REFRESH_HOMEBOY,
    STALE_DAEMON_RECOVERY,
};

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

/// `homeboy runner refresh-homeboy <id>`.
pub(crate) fn refresh_homeboy_action(runner_id: &str) -> ExecutableAction {
    refresh_homeboy_action_for_ref(runner_id, None)
}

/// `homeboy runner refresh-homeboy <id> [--ref <ref>]`.
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
    runner_action(
        "runner.refresh_homeboy",
        format!("refresh runner {runner_id}"),
        args,
        ActionSafety::Mutating,
    )
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

/// The explicit restart repair for a restartable daemon: stop it and connect
/// afresh. Re-establishing only the transport is never this plan; that is
/// `ensure_transport`, which cannot restart a daemon.
pub(crate) fn restart_plan(runner_id: &str) -> Vec<DaemonRepairStep> {
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
    // A mismatch identifies an incompatible binary, but not necessarily the
    // daemon that owns it. Do not turn that observation into a refresh until
    // the remote probe has established ownership well enough to authorize it.
    if !report.has_recovery_ownership_proof() {
        return Vec::new();
    }
    // A dead generation, or durable jobs that outlived the lease that owned
    // them, is repaired by starting the runner's daemon: its startup
    // recovery reconciles every in-scope job its dead predecessor left,
    // under the owner lock (#17429). `runner connect` is that start.
    let proven_dead = report.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
        && report.lease_id.is_some()
        && report.pid.is_some();
    let orphaned_jobs = report.active_jobs > 0
        && matches!(
            report.stale_reason_code,
            Some(DaemonStaleReasonCode::LeaseMissing | DaemonStaleReasonCode::LeaseCorrupt)
        );
    if proven_dead || orphaned_jobs {
        return vec![action_step(RUNNER_CONNECT, connect_action(runner_id))];
    }
    // An identity drift is a binary problem, not a lease problem: reconnecting
    // the same runner-side binary would reproduce the mismatch.
    if matches!(
        report.stale_reason_code,
        Some(DaemonStaleReasonCode::LabContractVersionMismatch)
    ) {
        return vec![action_step(
            RUNNER_REFRESH_HOMEBOY,
            refresh_homeboy_action(runner_id),
        )];
    }
    if report.restartable {
        return restart_plan(runner_id);
    }
    // A stale report that matched no branch names a real problem but authorizes
    // no mutation. The report's ownership evidence is the terminal diagnostic;
    // prescribing another probe here can make doctor recommend itself forever.
    Vec::new()
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
    use homeboy_core::daemon::DaemonRecoveryEvidence;

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
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs,
            termination_evidence: None,
            // A daemon-authored plan in the daemon's own frame. It must never
            // survive the crossing: `homeboy daemon stop` on the controller
            // stops the controller's daemon.
            repair_plan: vec![
                DaemonRepairStep::text("daemon_stop", "homeboy daemon stop".to_string()),
                DaemonRepairStep::text(
                    "daemon_ensure_running",
                    "homeboy daemon ensure-running".to_string(),
                ),
            ],
        }
    }

    fn plan(report: &DaemonFreshnessReport) -> Vec<(String, String)> {
        controller_frame_plan("homeboy-lab", report)
            .into_iter()
            .map(|step| (step.code, step.command))
            .collect()
    }

    /// Each typed-evidence branch maps to its controller-frame repair; a
    /// fresh report or one without ownership proof authorizes nothing.
    #[test]
    fn controller_frame_plan_follows_the_typed_evidence() {
        let connect = || {
            (
                RUNNER_CONNECT.to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            )
        };
        let cases = [
            ("fresh", None, 0, false, None, None, vec![]),
            (
                "dead generation",
                Some(DaemonStaleReasonCode::PidDead),
                1,
                false,
                Some(DaemonRecoveryEvidence::ProvenDead),
                None,
                vec![connect()],
            ),
            (
                "durable jobs without an owning lease",
                Some(DaemonStaleReasonCode::LeaseMissing),
                2,
                false,
                Some(DaemonRecoveryEvidence::ProvenDead),
                None,
                vec![connect()],
            ),
            (
                "identity drift refreshes the binary instead of reconnecting",
                Some(DaemonStaleReasonCode::LabContractVersionMismatch),
                0,
                true,
                Some(DaemonRecoveryEvidence::Recoverable),
                None,
                vec![(
                    RUNNER_REFRESH_HOMEBOY.to_string(),
                    "homeboy runner refresh-homeboy homeboy-lab".to_string(),
                )],
            ),
            (
                "restartable lease rebuilds the controller session",
                Some(DaemonStaleReasonCode::RuntimePathsDrift),
                0,
                true,
                Some(DaemonRecoveryEvidence::Recoverable),
                None,
                vec![
                    (
                        RUNNER_DISCONNECT.to_string(),
                        "homeboy runner disconnect homeboy-lab".to_string(),
                    ),
                    connect(),
                ],
            ),
            (
                "version skew alone must not authorize a daemon mutation",
                Some(DaemonStaleReasonCode::LabContractVersionMismatch),
                0,
                false,
                Some(DaemonRecoveryEvidence::Unavailable),
                Some("daemon lease owner could not be established".to_string()),
                vec![],
            ),
            // Insufficient ownership evidence stays terminal: a read-only
            // doctor fallback would be self-referential when doctor itself
            // projected this plan.
            (
                "missing ownership evidence must not authorize reconnect",
                Some(DaemonStaleReasonCode::TransportUnreachable),
                0,
                true,
                None,
                None,
                vec![],
            ),
            (
                "unavailable ownership evidence must not authorize reconnect",
                Some(DaemonStaleReasonCode::TransportUnreachable),
                0,
                true,
                Some(DaemonRecoveryEvidence::Unavailable),
                None,
                vec![],
            ),
        ];
        for (name, code, active_jobs, restartable, evidence, ownership, expected) in cases {
            let mut report = report(code, active_jobs, restartable);
            report.recovery_evidence = evidence;
            report.ownership_evidence = ownership;
            assert_eq!(plan(&report), expected, "{name}");
        }
    }

    /// Every branch must carry `homeboy runner` argv, or the executor is back
    /// to shell-parsing rendered text — and the daemon-authored local-frame
    /// plan in [`report`] must never survive the crossing.
    #[test]
    fn every_controller_frame_step_carries_executable_argv() {
        for code in [
            DaemonStaleReasonCode::PidDead,
            DaemonStaleReasonCode::LeaseMissing,
            DaemonStaleReasonCode::LeaseCorrupt,
            DaemonStaleReasonCode::LabContractVersionMismatch,
            DaemonStaleReasonCode::RuntimePathsDrift,
            DaemonStaleReasonCode::TransportUnreachable,
            DaemonStaleReasonCode::LeaseSchemaMismatch,
        ] {
            for restartable in [true, false] {
                for active_jobs in [0, 1] {
                    let mut report = report(Some(code), active_jobs, restartable);
                    report.recovery_evidence = Some(DaemonRecoveryEvidence::ProvenDead);
                    let plan = controller_frame_plan("homeboy-lab", &report);
                    if plan.is_empty() {
                        // Unavailable ownership evidence is terminal, not an
                        // implicit read-only or mutating recovery command.
                        continue;
                    }
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
}
