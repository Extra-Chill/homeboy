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
//! These builders are the single definition of each command string, so the
//! `adoption_command` a report carries and the `repair_plan` step that executes
//! it can never drift apart.

use homeboy_core::daemon::{DaemonFreshnessReport, DaemonRepairStep, DaemonStaleReasonCode};
use homeboy_core::engine::shell;

pub(crate) const RUNNER_DISCONNECT: &str = "runner_disconnect";
pub(crate) const RUNNER_CONNECT: &str = "runner_connect";
pub(crate) const RUNNER_REFRESH_HOMEBOY: &str = "runner_refresh_homeboy";
pub(crate) const RUNNER_ADOPT_ORPHAN_LEASE: &str = "runner_adopt_orphan_lease";
pub(crate) const RUNNER_RECONCILE_LEASELESS_ORPHANS: &str = "runner_reconcile_leaseless_orphans";
pub(crate) const STALE_DAEMON_RECOVERY: &str = "stale_daemon_recovery";

pub(crate) fn step(code: &str, command: String) -> DaemonRepairStep {
    DaemonRepairStep {
        code: code.to_string(),
        command,
    }
}

/// `homeboy runner disconnect <id>`.
pub(crate) fn disconnect_command(runner_id: &str) -> String {
    format!("homeboy runner disconnect {}", shell::quote_arg(runner_id))
}

/// `homeboy runner connect <id>`.
pub(crate) fn connect_command(runner_id: &str) -> String {
    format!("homeboy runner connect {}", shell::quote_arg(runner_id))
}

/// `homeboy runner connect <id> --adopt-orphan-lease <lease> --confirm-pid-dead`.
pub(crate) fn adopt_orphan_lease_command(runner_id: &str, lease_id: &str) -> String {
    format!(
        "homeboy runner connect {} --adopt-orphan-lease {} --confirm-pid-dead",
        shell::quote_arg(runner_id),
        shell::quote_arg(lease_id)
    )
}

/// `homeboy runner connect <id> --reconcile-leaseless-orphans --confirm-no-daemon-owner`.
pub(crate) fn reconcile_leaseless_orphans_command(runner_id: &str) -> String {
    format!(
        "homeboy runner connect {} --reconcile-leaseless-orphans --confirm-no-daemon-owner",
        shell::quote_arg(runner_id)
    )
}

/// `homeboy runner refresh-homeboy <id> --reconnect`.
pub(crate) fn refresh_homeboy_command(runner_id: &str) -> String {
    format!(
        "homeboy runner refresh-homeboy {} --reconnect",
        shell::quote_arg(runner_id)
    )
}

/// The last-resort controller-side repair: drop the session and rebuild it.
///
/// This is the plan used when nothing specific is known about the runner's
/// daemon, so it stays deliberately generic.
pub(crate) fn reconnect_plan(runner_id: &str) -> Vec<DaemonRepairStep> {
    vec![
        step(RUNNER_DISCONNECT, disconnect_command(runner_id)),
        step(RUNNER_CONNECT, connect_command(runner_id)),
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
            return vec![step(
                RUNNER_ADOPT_ORPHAN_LEASE,
                adopt_orphan_lease_command(runner_id, lease_id),
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
        return vec![step(
            RUNNER_RECONCILE_LEASELESS_ORPHANS,
            reconcile_leaseless_orphans_command(runner_id),
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
        return vec![step(
            RUNNER_REFRESH_HOMEBOY,
            refresh_homeboy_command(runner_id),
        )];
    }
    if report.restartable {
        return reconnect_plan(runner_id);
    }
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
        assert!(plan(&report(Some(DaemonStaleReasonCode::PidDead), 0, false)).is_empty());
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
