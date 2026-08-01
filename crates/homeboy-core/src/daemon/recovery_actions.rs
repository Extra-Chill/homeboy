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

use serde::Serialize;
use uuid::Uuid;

use super::{DaemonRepairStep, DaemonStatus};
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

/// The recovery a dispatcher resolved from one `homeboy daemon status` read.
///
/// Every argument in `steps` is already filled in from that report. The point
/// of the type is that an operator never transcribes a lease id, a PID, an
/// endpoint, or a job id out of a prior status output by hand (#11105).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonRecoveryPlan {
    pub steps: Vec<DaemonRepairStep>,
    /// Why this plan, in the evidence's own terms. Non-empty even when there
    /// are no steps, so "nothing to do" and "nothing is authorized" are
    /// distinguishable.
    pub reason: String,
    /// Attestations only an operator can make. A dispatcher surfaces these and
    /// refuses to execute until they are supplied; it never synthesizes them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_confirmations: Vec<String>,
    /// Whether every step is a mutation the plan can carry out on its own.
    pub executable: bool,
}

impl DaemonRecoveryPlan {
    fn nothing(reason: impl Into<String>) -> Self {
        Self {
            steps: Vec::new(),
            reason: reason.into(),
            required_confirmations: Vec::new(),
            executable: false,
        }
    }
}

/// Resolve the recovery for one daemon status report.
///
/// The freshness report already computes the local-frame repair for every case
/// it can authorize, so this dispatches to that plan rather than restating the
/// table. It adds the two cases the freshness report deliberately leaves empty:
/// an exact PID-less job set behind a dead lease, which needs an operator
/// attestation, and the unauthorized remainder, which gets an explicit reason
/// and a read-only step instead of nothing at all.
pub fn plan_recovery(status: &DaemonStatus) -> DaemonRecoveryPlan {
    let freshness = &status.freshness;
    if freshness.fresh && freshness.stale_reason_code.is_none() {
        return DaemonRecoveryPlan::nothing(
            "daemon lease is fresh; there is nothing to recover".to_string(),
        );
    }

    if !freshness.repair_plan.is_empty() {
        let reason = match freshness.stale_reason_code {
            Some(code) => format!(
                "daemon is stale ({code:?}) with {} active job(s); the freshness report authorizes this repair",
                freshness.active_jobs
            ),
            None => "daemon reported a repair plan without a stale reason code".to_string(),
        };
        let required_confirmations = freshness
            .repair_plan
            .iter()
            .filter_map(|step| step.action.as_ref())
            .flat_map(|action| action.required_confirmations.iter().cloned())
            .collect();
        return DaemonRecoveryPlan {
            steps: freshness.repair_plan.clone(),
            reason,
            required_confirmations,
            executable: true,
        };
    }

    // The lease named a daemon that is gone and durable jobs outlived it, but
    // the daemon died before persisting any child identity, so the store holds
    // no PID to check. The exact job set is a compare-and-swap over the
    // destructive scope, and the store refuses any mismatch — so the ids can be
    // filled from the report, while the workload attestation cannot be.
    let job_ids: Vec<Uuid> = status
        .active_job_recovery_evidence
        .iter()
        .map(|evidence| evidence.job_id)
        .collect();
    if let Some(lease_id) = freshness.lease_id.as_deref() {
        if !job_ids.is_empty() {
            let action = reconcile_dead_lease_orphans(lease_id, &job_ids);
            let required_confirmations = action.required_confirmations.clone();
            return DaemonRecoveryPlan {
                steps: vec![DaemonRepairStep::executable(
                    DAEMON_RECONCILE_DEAD_LEASE_ORPHANS,
                    action,
                )],
                reason: format!(
                    "lease `{lease_id}` is gone and {} durable job(s) have no recorded child identity; the exact job set is named from the report, but workload absence is unverifiable in process",
                    job_ids.len()
                ),
                required_confirmations,
                executable: true,
            };
        }
    }

    // Nothing is authorized. Say so, and hand back the read-only step that
    // produces the evidence a later pass would need, rather than an empty plan.
    let reason = match freshness.stale_reason_code {
        Some(code) => format!(
            "daemon is stale ({code:?}) but the evidence authorizes no automatic recovery: {}",
            freshness
                .ownership_evidence
                .as_deref()
                .unwrap_or("no ownership evidence was recorded")
        ),
        None => "daemon is not fresh and recorded no stale reason code".to_string(),
    };
    DaemonRecoveryPlan {
        steps: vec![DaemonRepairStep::executable(DAEMON_DIAGNOSE, diagnose())],
        reason,
        required_confirmations: Vec::new(),
        executable: false,
    }
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

    fn status(
        stale_reason_code: Option<super::super::DaemonStaleReasonCode>,
        repair_plan: Vec<DaemonRepairStep>,
        active_jobs: usize,
    ) -> DaemonStatus {
        DaemonStatus {
            running: false,
            fresh: stale_reason_code.is_none(),
            reachable: true,
            freshness: super::super::DaemonFreshnessReport {
                fresh: stale_reason_code.is_none(),
                stale_reason_code,
                restartable: true,
                lease_id: Some("661f731f-99c7-436a-aadc-24dee908fd8b".to_string()),
                pid: Some(3_572_046),
                recovery_evidence: None,
                ownership_evidence: Some("daemon lease evidence is inconclusive".to_string()),
                adoption_command: None,
                binary_hash: None,
                daemon_version: Some("0.326.1".to_string()),
                daemon_build_identity: Some("homeboy 0.326.1+8755ba48288f".to_string()),
                runtime_paths: None,
                active_jobs,
                termination_evidence: None,
                repair_plan,
            },
            stale_reason: None,
            state: None,
            state_path: "/root/.config/homeboy/daemon/state.json".to_string(),
            state_identity: "identity".to_string(),
            process_candidates: Vec::new(),
            active_job_recovery_evidence: Vec::new(),
            termination_evidence: None,
        }
    }

    /// The live reproduction this fix was written against: a reachable daemon
    /// whose build identity no longer matches the current binary, zero active
    /// jobs, restartable. The dispatcher must resolve the stop/start pair with
    /// every argument already filled in.
    #[test]
    fn a_version_mismatch_report_resolves_the_restart_plan() {
        let status = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![
                DaemonRepairStep::executable(DAEMON_STOP, stop()),
                DaemonRepairStep::executable(DAEMON_START, start()),
            ],
            0,
        );

        let plan = plan_recovery(&status);

        assert!(plan.executable);
        assert!(plan.required_confirmations.is_empty());
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| (step.code.as_str(), step.command.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (DAEMON_STOP, "homeboy daemon stop"),
                (DAEMON_START, "homeboy daemon start"),
            ]
        );
        assert!(plan.reason.contains("VersionMismatch"), "{}", plan.reason);
        for step in &plan.steps {
            assert!(step.action.is_some(), "{} carries no argv", step.code);
        }
    }

    #[test]
    fn a_fresh_report_plans_nothing_and_says_why() {
        let plan = plan_recovery(&status(None, Vec::new(), 0));

        assert!(plan.steps.is_empty());
        assert!(!plan.executable);
        assert!(plan.reason.contains("fresh"), "{}", plan.reason);
    }

    /// #11105: a report the freshness table authorizes nothing for used to
    /// leave an operator with no subcommand to run and no statement of why.
    #[test]
    fn a_report_matching_no_branch_still_yields_a_step_and_a_reason() {
        let plan = plan_recovery(&status(
            Some(super::super::DaemonStaleReasonCode::TransportUnreachable),
            Vec::new(),
            0,
        ));

        assert_eq!(plan.steps.len(), 1, "an unmatched report is never empty");
        assert_eq!(plan.steps[0].code, DAEMON_DIAGNOSE);
        assert_eq!(plan.steps[0].command, "homeboy daemon status");
        assert!(
            !plan.executable,
            "a read-only diagnosis is not a repair to apply"
        );
        assert!(
            plan.reason.contains("authorizes no automatic recovery"),
            "{}",
            plan.reason
        );
        assert!(
            plan.reason
                .contains("daemon lease evidence is inconclusive"),
            "the refusal must quote the evidence it refused on: {}",
            plan.reason
        );
    }

    /// Every `--job-id` is filled from the report; only the attestation that no
    /// report can contain is demanded from the operator.
    #[test]
    fn a_pidless_job_set_is_filled_from_the_report_but_still_demands_the_attestation() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::LeaseCorrupt),
            Vec::new(),
            2,
        );
        status.active_job_recovery_evidence = vec![
            recovery_evidence(Uuid::from_u128(1)),
            recovery_evidence(Uuid::from_u128(2)),
        ];

        let plan = plan_recovery(&status);

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].code, DAEMON_RECONCILE_DEAD_LEASE_ORPHANS);
        let args = &plan.steps[0].action.as_ref().expect("argv").args;
        assert_eq!(
            args.iter().filter(|arg| *arg == "--job-id").count(),
            2,
            "every durable job id is named: {args:?}"
        );
        assert!(args.contains(&Uuid::from_u128(1).to_string()));
        assert!(args.contains(&"661f731f-99c7-436a-aadc-24dee908fd8b".to_string()));
        assert_eq!(
            plan.required_confirmations,
            vec![CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string()],
            "workload absence is unverifiable and stays operator-supplied"
        );
    }

    fn recovery_evidence(job_id: Uuid) -> crate::api_jobs::DaemonActiveJobRecoveryEvidence {
        crate::api_jobs::DaemonActiveJobRecoveryEvidence {
            job_id,
            operation: "exec".to_string(),
            status: crate::api_jobs::JobStatus::Running,
            daemon_lease_id: Some("661f731f-99c7-436a-aadc-24dee908fd8b".to_string()),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            terminal_evidence: None,
            child_pid: None,
            child_started_at: None,
            linked_durable_run_id: None,
            linked_durable_run_state: None,
            linked_durable_run_terminal_status: None,
            disposition:
                crate::api_jobs::DaemonActiveJobRecoveryDisposition::MissingChildIdentityRecoverable,
        }
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
