//! Local-frame daemon recovery actions.
//!
//! Every `homeboy daemon *` recovery invocation is defined here exactly once,
//! as argv. `DaemonFreshnessReport::repair_plan` renders its operator text from
//! these builders and `homeboy daemon recover` dispatches on the same codes, so
//! the plan an operator reads and the plan the dispatcher executes cannot drift
//! apart, and no executor ever has to parse a rendered command back into
//! arguments (#11103, #11105).
//!
//! These are the *local* frame. A report that has crossed an SSH boundary must
//! be restated in controller frame first — running `homeboy daemon stop` on the
//! controller stops the controller's daemon, not the runner's.

use uuid::Uuid;

use crate::error::{ActionSafety, ExecutableAction};

/// Stop the daemon recorded in the local state file.
pub const DAEMON_STOP: &str = "daemon_stop";
/// Start a replacement daemon.
pub const DAEMON_START: &str = "daemon_start";
/// Adopt one proven-dead lease and reconcile the jobs it owned.
pub const DAEMON_ADOPT_ORPHAN: &str = "daemon_adopt_orphan";
/// Reconcile durable jobs that outlived the lease that owned them.
pub const DAEMON_RECONCILE_LEASELESS_ORPHANS: &str = "daemon_reconcile_leaseless_orphans";
/// Reconcile an exact PID-less job set after a proven unexpected daemon exit.
pub const DAEMON_RECONCILE_DEAD_LEASE_ORPHANS: &str = "daemon_reconcile_dead_lease_orphans";
/// Re-read daemon evidence. The step emitted when nothing else is authorized.
pub const DAEMON_DIAGNOSE: &str = "daemon_diagnose";

/// The operator attestation that cannot be synthesized from any report.
///
/// `reconcile-dead-lease-orphans` exists precisely because the daemon died
/// before persisting any child identity, so the store holds no PID to inspect
/// and nothing in-process can observe whether the workloads are still running.
/// A dispatcher must surface this as a required confirmation, never supply it.
pub const CONFIRM_WORKLOAD_PROCESSES_ABSENT: &str = "confirm-workload-processes-absent";

fn action(
    id: &str,
    label: String,
    args: impl IntoIterator<Item = String>,
    safety: ActionSafety,
) -> ExecutableAction {
    ExecutableAction::new(id, label, "homeboy", args, safety)
}

pub fn stop() -> ExecutableAction {
    action(
        "daemon.stop",
        "stop the local daemon".to_string(),
        ["daemon".to_string(), "stop".to_string()],
        ActionSafety::Mutating,
    )
}

pub fn start() -> ExecutableAction {
    action(
        "daemon.start",
        "start a replacement local daemon".to_string(),
        ["daemon".to_string(), "start".to_string()],
        ActionSafety::Mutating,
    )
}

pub fn adopt_orphan(lease_id: &str) -> ExecutableAction {
    action(
        "daemon.adopt_orphan",
        format!("adopt proven-dead daemon lease {lease_id}"),
        [
            "daemon".to_string(),
            "adopt-orphan".to_string(),
            "--lease-id".to_string(),
            lease_id.to_string(),
            // Released spelling. The flag is a no-op — adoption proves PID death
            // itself, under the lifecycle lock — but it is retained so the
            // rendered command stays copy-pasteable against a released binary.
            "--confirm-pid-dead".to_string(),
        ],
        ActionSafety::Mutating,
    )
}

pub fn reconcile_leaseless_orphans() -> ExecutableAction {
    action(
        "daemon.reconcile_leaseless_orphans",
        "reconcile durable jobs with no owning daemon lease".to_string(),
        [
            "daemon".to_string(),
            "reconcile-leaseless-orphans".to_string(),
            "--confirm-no-daemon-owner".to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// The exact PID-less job set is a compare-and-swap over the destructive scope,
/// so every job id is named. The store recomputes the active set and refuses any
/// mismatch; a dispatcher may therefore fill the ids from the report it read.
pub fn reconcile_dead_lease_orphans(lease_id: &str, job_ids: &[Uuid]) -> ExecutableAction {
    let mut args = vec![
        "daemon".to_string(),
        "reconcile-dead-lease-orphans".to_string(),
        "--lease-id".to_string(),
        lease_id.to_string(),
    ];
    for job_id in job_ids {
        args.push("--job-id".to_string());
        args.push(job_id.to_string());
    }
    args.push(format!("--{CONFIRM_WORKLOAD_PROCESSES_ABSENT}"));
    action(
        "daemon.reconcile_dead_lease_orphans",
        format!(
            "reconcile {} PID-less durable job(s) owned by dead lease {lease_id}",
            job_ids.len()
        ),
        args,
        ActionSafety::Mutating,
    )
    .requiring_confirmation(CONFIRM_WORKLOAD_PROCESSES_ABSENT)
}

/// Read-only. Emitted when the evidence authorizes no mutation, so that a
/// report matching no recovery branch still hands back something to run.
pub fn diagnose() -> ExecutableAction {
    action(
        "daemon.status",
        "re-read daemon lease, process, and durable job evidence".to_string(),
        ["daemon".to_string(), "status".to_string()],
        ActionSafety::ReadOnly,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_text_is_derived_from_argv() {
        assert_eq!(stop().render_command(), "homeboy daemon stop");
        assert_eq!(start().render_command(), "homeboy daemon start");
        assert_eq!(
            adopt_orphan("lease-dead").render_command(),
            "homeboy daemon adopt-orphan --lease-id lease-dead --confirm-pid-dead"
        );
        assert_eq!(
            reconcile_leaseless_orphans().render_command(),
            "homeboy daemon reconcile-leaseless-orphans --confirm-no-daemon-owner"
        );
        assert_eq!(diagnose().render_command(), "homeboy daemon status");
    }

    #[test]
    fn an_exact_job_set_is_named_argument_by_argument() {
        let first = Uuid::nil();
        let second = Uuid::from_u128(2);
        let action = reconcile_dead_lease_orphans("lease-dead", &[first, second]);

        assert_eq!(
            action.args,
            vec![
                "daemon",
                "reconcile-dead-lease-orphans",
                "--lease-id",
                "lease-dead",
                "--job-id",
                &first.to_string(),
                "--job-id",
                &second.to_string(),
                "--confirm-workload-processes-absent",
            ]
        );
    }

    /// The workload attestation is unverifiable by construction, so it must
    /// travel as a declared confirmation an operator has to supply rather than
    /// as an argument a dispatcher can fill in from a report.
    #[test]
    fn the_workload_attestation_is_declared_as_an_operator_confirmation() {
        assert_eq!(
            reconcile_dead_lease_orphans("lease-dead", &[Uuid::nil()]).required_confirmations,
            vec![CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string()]
        );
        assert!(stop().required_confirmations.is_empty());
    }

    #[test]
    fn diagnosis_is_the_only_read_only_action() {
        assert_eq!(diagnose().safety, ActionSafety::ReadOnly);
        for action in [
            stop(),
            start(),
            adopt_orphan("lease"),
            reconcile_leaseless_orphans(),
        ] {
            assert_eq!(action.safety, ActionSafety::Mutating, "{}", action.id);
        }
    }
}
