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
//!
//! [`plan_recovery`] is the one daemon recovery planner. There is one daemon
//! state directory and one job store, so one authoritative status read decides
//! both the executable repair and the single next lifecycle action
//! ([`DaemonRecoveryPlan::lifecycle`]) that `daemon plan`, the mutation gate,
//! and `daemon recover` all follow.

use serde::Serialize;
use uuid::Uuid;

use super::lifecycle_plan::{Attestation, BlockCause, Plan, Step, WaitCause};
use super::{DaemonRepairStep, DaemonStaleReasonCode, DaemonStatus};
use crate::error::{ActionSafety, ExecutableAction};

/// Stop the daemon recorded in the local state file.
pub const DRAIN_THEN_REPLACE: &str = "drain_then_replace";
pub const DAEMON_STOP: &str = "daemon_stop";
/// Replace or reuse the daemon that owns the local store. Starting a daemon
/// runs its startup recovery of every dead generation in its scope.
pub const DAEMON_ENSURE_RUNNING: &str = "daemon_ensure_running";
/// Terminalize named dead-generation jobs on the operator's workload-absence
/// attestation.
pub const DAEMON_ATTEST_ABSENT: &str = "daemon_attest_absent";
/// Re-read daemon evidence. The step emitted when nothing else is authorized.
pub const DAEMON_DIAGNOSE: &str = "daemon_diagnose";

/// The operator attestation that cannot be synthesized from any report.
///
/// A dead generation's job with no recorded child identity has no PID to
/// inspect, so nothing in process can observe whether its workload still runs.
/// A dispatcher must surface this as a required confirmation, never supply it.
pub const ATTEST_ABSENT: &str = "attest-absent";

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

/// Stop the exact daemon that holds `lease_id`.
///
/// A bare `homeboy daemon stop` refuses to stop a daemon whose recorded lease
/// has gone stale, so a restartable report with a lease in hand must render the
/// bound spelling or an operator copy-pasting the plan lands worse than before
/// (#11220).
pub fn stop_for_lease(lease_id: &str) -> ExecutableAction {
    action(
        "daemon.stop",
        format!("stop the local daemon holding lease {lease_id}"),
        [
            "daemon".to_string(),
            "stop".to_string(),
            "--lease-id".to_string(),
            lease_id.to_string(),
        ],
        ActionSafety::Mutating,
    )
}

pub fn ensure_running() -> ExecutableAction {
    action(
        "daemon.ensure_running",
        "ensure a fresh daemon for the local store".to_string(),
        ["daemon".to_string(), "ensure-running".to_string()],
        ActionSafety::Mutating,
    )
}

/// Resolve and run the daemon recovery the current status report authorizes.
/// Used where the caller knows the resident daemon is stale but not which
/// exact repair applies; `recover` derives that from live status.
pub fn recover() -> ExecutableAction {
    action(
        "daemon.recover",
        "restart the resident daemon onto the installed Homeboy binary".to_string(),
        [
            "daemon".to_string(),
            "recover".to_string(),
            "--yes".to_string(),
        ],
        ActionSafety::Mutating,
    )
}

/// `homeboy daemon recover --attest-absent <job-id>...`. The store re-checks
/// every named job and refuses the whole set if any is not a dead generation's
/// in-scope job without child identity; a dispatcher may therefore fill the
/// ids from the report it read, but never the attestation itself.
pub fn attest_absent(job_ids: &[Uuid]) -> ExecutableAction {
    let mut args = vec![
        "daemon".to_string(),
        "recover".to_string(),
        format!("--{ATTEST_ABSENT}"),
    ];
    args.extend(job_ids.iter().map(Uuid::to_string));
    action(
        "daemon.attest_absent",
        format!(
            "terminalize {} dead-generation job(s) whose workload an operator attests is gone",
            job_ids.len()
        ),
        args,
        ActionSafety::Mutating,
    )
    .requiring_confirmation(ATTEST_ABSENT)
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
    /// The same decision as the single next lifecycle action. `daemon plan`
    /// and the mutating entry points' safety gate read this, so they can never
    /// disagree with what `daemon recover` executes.
    pub lifecycle: Plan,
}

impl DaemonRecoveryPlan {
    fn nothing(reason: impl Into<String>, lifecycle: impl FnOnce(String) -> Plan) -> Self {
        let reason = reason.into();
        Self {
            steps: Vec::new(),
            lifecycle: lifecycle(reason.clone()),
            reason,
            required_confirmations: Vec::new(),
            executable: false,
        }
    }

    /// A read-only plan: re-read the evidence, carry out nothing.
    fn diagnose(reason: String, lifecycle: impl FnOnce(String) -> Plan) -> Self {
        Self {
            steps: vec![DaemonRepairStep::executable(DAEMON_DIAGNOSE, diagnose())],
            lifecycle: lifecycle(reason.clone()),
            reason,
            required_confirmations: Vec::new(),
            executable: false,
        }
    }
}

/// Resolve the recovery for one daemon status report.
///
/// This is the one daemon recovery planner. A dead or missing daemon is
/// recovered by starting one: serve's startup recovery reconciles every dead
/// generation in its scope, protecting running children, terminalizing dead
/// ones, and resuming controller work. The freshness report already computes
/// that local-frame repair, so this dispatches to it rather than restating the
/// table. What startup cannot prove is a dead generation's job with no
/// recorded child identity: that needs `daemon recover --attest-absent`. A
/// live daemon process that holds no lease blocks everything, and the
/// unauthorized remainder gets an explicit reason and a read-only step instead
/// of nothing at all. Each outcome carries its lifecycle verdict.
pub fn plan_recovery(status: &DaemonStatus) -> DaemonRecoveryPlan {
    let mut plan = plan_recovery_steps(status);
    // A lease that cannot be decoded is not a lifecycle verdict anyone may
    // act on beyond the corrupt-lease repair the freshness report authorizes.
    if status.freshness.stale_reason_code == Some(DaemonStaleReasonCode::LeaseCorrupt)
        && !plan.executable
    {
        plan.lifecycle = Plan::Blocked {
            cause: BlockCause::Unreadable,
            reason: plan.reason.clone(),
        };
    }
    plan
}

/// Opt-in draining leaves the default recovery planner unchanged.
pub fn plan_recovery_with_drain(status: &DaemonStatus, drain: bool) -> DaemonRecoveryPlan {
    let freshness = &status.freshness;
    if drain
        && status.running
        && status.reachable
        && freshness.active_jobs > 0
        && matches!(
            freshness.stale_reason_code,
            Some(
                DaemonStaleReasonCode::VersionMismatch
                    | DaemonStaleReasonCode::BuildIdentityMismatch
                    | DaemonStaleReasonCode::ExecutableReplaced
            )
        )
    {
        return DaemonRecoveryPlan {
            steps: vec![DaemonRepairStep::executable(
                DRAIN_THEN_REPLACE,
                drain_action(),
            )],
            reason: format!(
                "drain {} protected job(s), then replace the stale daemon",
                freshness.active_jobs
            ),
            required_confirmations: Vec::new(),
            executable: true,
            lifecycle: Plan::Transition {
                step: Step::Repair {
                    lease_id: freshness.lease_id.clone(),
                    codes: vec![DRAIN_THEN_REPLACE.to_string()],
                },
            },
        };
    }
    plan_recovery(status)
}

fn drain_action() -> ExecutableAction {
    let mut action = recover();
    action.args.push("--drain".to_string());
    action
}

/// Dead-generation jobs startup recovery left untouched: no recorded child
/// identity proves their workload gone.
fn needs_attestation(status: &DaemonStatus) -> Vec<Uuid> {
    status
        .active_job_recovery_evidence
        .iter()
        .filter(|evidence| {
            evidence.disposition
                == crate::api_jobs::DaemonActiveJobRecoveryDisposition::NeedsAttestation
        })
        .map(|evidence| evidence.job_id)
        .collect()
}

fn attestation_plan(status: &DaemonStatus, job_ids: Vec<Uuid>) -> DaemonRecoveryPlan {
    let action = attest_absent(&job_ids);
    let required_confirmations = action.required_confirmations.clone();
    DaemonRecoveryPlan {
        lifecycle: Plan::NeedsAttestation(Attestation {
            state_dir: std::path::Path::new(&status.state_path)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default(),
            job_ids: job_ids.clone(),
        }),
        steps: vec![DaemonRepairStep::executable(DAEMON_ATTEST_ABSENT, action)],
        reason: format!(
            "{} dead-generation job(s) have no recorded child identity ({}); startup recovery cannot prove their workload gone, so an operator must attest it",
            job_ids.len(),
            job_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        required_confirmations,
        executable: true,
    }
}

fn plan_recovery_steps(status: &DaemonStatus) -> DaemonRecoveryPlan {
    let freshness = &status.freshness;
    // A second daemon process competing with the live lease for this store is
    // a conflict however healthy the lease looks (#17387): nothing is started
    // or replaced until one of them is gone.
    if let Some(contender) = &status.competing_serve {
        return DaemonRecoveryPlan::diagnose(
            format!(
                "{} while the live lease holds this store; stop the one its service manager does not own",
                contender.describe()
            ),
            |reason| Plan::Blocked {
                cause: BlockCause::UnleasedProcess,
                reason,
            },
        );
    }

    // The owner lock is held, yet no live lease names its holder: a daemon is
    // starting (and running its startup recovery), or a recovery operation
    // holds the store. Any start or stop could race it, so nothing proceeds
    // until the holder publishes a lease or releases the lock.
    if !status.running
        && status.owner_lock.is_held()
        && freshness.stale_reason_code != Some(DaemonStaleReasonCode::LeaseCorrupt)
    {
        let holder = match status.owner_lock.holder() {
            Some(holder) => format!("daemon pid {}", holder.pid),
            None => "a starting daemon or a recovery operation".to_string(),
        };
        return DaemonRecoveryPlan::diagnose(
            format!(
                "the daemon owner lock is held by {holder} without a live lease; no process is signaled and no daemon is started until it publishes a lease or exits"
            ),
            |reason| Plan::Blocked {
                cause: BlockCause::UnleasedProcess,
                reason,
            },
        );
    }

    // A drain waits on the protected jobs it still holds; name each one, and
    // the blocker with its recovery command, instead of a bare wait (#17707).
    if status.running && status.draining() {
        let holders = super::lifecycle_plan::drain_holders(status, crate::api_jobs::timestamp_ms());
        let reason = if holders.is_empty() {
            "daemon draining: no protected jobs remain; the replacement admits queued work on startup".to_string()
        } else {
            format!(
                "daemon draining: {} protected job(s) hold the drain: {}",
                holders.len(),
                super::lifecycle_plan::describe_drain_holders(&holders)
            )
        };
        return DaemonRecoveryPlan::diagnose(reason, |reason| Plan::Draining { reason, holders });
    }

    let attestation = needs_attestation(status);
    if freshness.fresh && freshness.stale_reason_code.is_none() {
        if !attestation.is_empty() {
            return attestation_plan(status, attestation);
        }
        return DaemonRecoveryPlan::nothing(
            "daemon lease is fresh; there is nothing to recover".to_string(),
            |_| Plan::Converged,
        );
    }

    // Starting (or restarting) the daemon runs startup recovery, so it comes
    // before any attestation: jobs it can prove are reconciled without one.
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
            lifecycle: Plan::Transition {
                step: Step::Repair {
                    lease_id: freshness.lease_id.clone(),
                    codes: freshness
                        .repair_plan
                        .iter()
                        .map(|step| step.code.clone())
                        .collect(),
                },
            },
            steps: freshness.repair_plan.clone(),
            reason,
            required_confirmations,
            executable: true,
        };
    }
    if !attestation.is_empty() {
        return attestation_plan(status, attestation);
    }

    // Nothing is authorized. Say so, and hand back the read-only step that
    // produces the evidence a later pass would need, rather than an empty plan.
    let reason = match freshness.stale_reason_code {
        Some(super::DaemonStaleReasonCode::VersionMismatch)
            if status.running && status.reachable =>
        {
            status.render_summary()
        }
        Some(code) => format!(
            "daemon is stale ({code:?}) but the evidence authorizes no automatic recovery: {}",
            freshness
                .ownership_evidence
                .as_deref()
                .unwrap_or("no ownership evidence was recorded")
        ),
        None => "daemon is not fresh and recorded no stale reason code".to_string(),
    };
    // A live stale daemon that still owns jobs is replaced once they finish.
    let busy = status.running && freshness.active_jobs > 0;
    DaemonRecoveryPlan::diagnose(reason, |reason| {
        if busy {
            Plan::Wait {
                cause: WaitCause::BusyStaleDaemon,
                reason,
            }
        } else {
            Plan::Blocked {
                cause: BlockCause::Unauthorized,
                reason,
            }
        }
    })
}

/// Whether an admission preflight may apply this daemon's canonical idle
/// restart without asking an operator to supply any additional evidence.
///
/// This policy is intentionally independent of the command requesting daemon
/// admission. It trusts only the authoritative status report and accepts only
/// the bounded in-place replacement plan: an explicitly restartable lease whose active
/// jobs are all restart-safe (no workload a replacement would lose), no
/// attestations, and no other mutation mixed into the plan.
pub fn authorizes_automatic_idle_restart(status: &DaemonStatus) -> bool {
    let plan = plan_recovery(status);
    !status.fresh
        && status.freshness.active_jobs == 0
        // Restart-safe jobs survive the replacement; see
        // `blocks_daemon_replacement`.
        && status
            .active_job_recovery_evidence
            .iter()
            .all(|evidence| !evidence.disposition.blocks_daemon_replacement())
        && status.freshness.restartable
        && plan.executable
        && plan.required_confirmations.is_empty()
        && plan.steps.len() == 1
        && plan.steps[0].code == DAEMON_ENSURE_RUNNING
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lease id the `status(...)` fixture carries. Assertions that depend on
    /// the report's repair rendering reference this so the fixture and the
    /// expectation cannot drift apart.
    const LEASE_ID: &str = "661f731f-99c7-436a-aadc-24dee908fd8b";

    #[test]
    fn rendered_text_is_derived_from_argv() {
        assert_eq!(stop().render_command(), "homeboy daemon stop");
        assert_eq!(
            stop_for_lease("lease-live").render_command(),
            "homeboy daemon stop --lease-id lease-live"
        );
        assert_eq!(
            ensure_running().render_command(),
            "homeboy daemon ensure-running"
        );
        assert_eq!(
            attest_absent(&[Uuid::nil(), Uuid::from_u128(2)]).render_command(),
            format!(
                "homeboy daemon recover --attest-absent {} {}",
                Uuid::nil(),
                Uuid::from_u128(2)
            )
        );
        assert_eq!(diagnose().render_command(), "homeboy daemon status");
    }

    /// The workload attestation is unverifiable by construction, so it must
    /// travel as a declared confirmation an operator has to supply rather than
    /// as an argument a dispatcher can fill in from a report.
    #[test]
    fn the_workload_attestation_is_declared_as_an_operator_confirmation() {
        assert_eq!(
            attest_absent(&[Uuid::nil()]).required_confirmations,
            vec![ATTEST_ABSENT.to_string()]
        );
        assert!(stop().required_confirmations.is_empty());
    }

    fn status(
        stale_reason_code: Option<super::super::DaemonStaleReasonCode>,
        repair_plan: Vec<DaemonRepairStep>,
        active_jobs: usize,
    ) -> DaemonStatus {
        let fresh = stale_reason_code.is_none();
        let replacement_blocked = !fresh && repair_plan.is_empty();
        let replacement_blocked_reason = replacement_blocked.then(|| {
            if active_jobs > 0 {
                format!(
                    "{active_jobs} active durable job(s) are protected from implicit replacement"
                )
            } else {
                "daemon lease evidence is inconclusive".to_string()
            }
        });
        let mut status = DaemonStatus {
            running: true,
            fresh,
            reachable: true,
            freshness: super::super::DaemonFreshnessReport {
                fresh,
                stale_reason_code,
                restartable: true,
                lease_id: Some(LEASE_ID.to_string()),
                pid: Some(3_572_046),
                recovery_evidence: None,
                ownership_evidence: Some("daemon lease evidence is inconclusive".to_string()),
                daemon_version: Some("0.326.1".to_string()),
                daemon_build_identity: Some("homeboy 0.326.1+8755ba48288f".to_string()),
                runtime_paths: None,
                active_jobs,
                termination_evidence: None,
                repair_plan,
            },
            stale_reason: None,
            replacement_blocked,
            replacement_blocked_reason,
            summary: String::new(),
            state: None,
            state_path: "/root/.config/homeboy/daemon/state.json".to_string(),
            state_identity: "identity".to_string(),
            owner_lock: super::super::DaemonOwnerLockProbe::Free,
            competing_serve: None,
            active_job_recovery_evidence: Vec::new(),
            termination_evidence: None,
        };
        status.summary = status.render_summary();
        status
    }

    /// The live reproduction this fix was written against: a reachable daemon
    /// whose build identity no longer matches the current binary, zero active
    /// jobs, restartable. The dispatcher must resolve the authority-bound
    /// replacement with
    /// every argument already filled in — including the lease the fixture
    /// carries. Recovery must keep the selected authority while replacing it.
    #[test]
    fn a_version_mismatch_report_resolves_the_restart_plan() {
        let status = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
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
            vec![(DAEMON_ENSURE_RUNNING, "homeboy daemon ensure-running")]
        );
        assert!(plan.reason.contains("VersionMismatch"), "{}", plan.reason);
        for step in &plan.steps {
            assert!(step.action.is_some(), "{} carries no argv", step.code);
        }
        assert!(authorizes_automatic_idle_restart(&status));
    }

    #[test]
    fn automatic_idle_restart_refuses_active_jobs_and_non_restart_plans() {
        let active = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            1,
        );
        assert!(!authorizes_automatic_idle_restart(&active));

        let mut inconsistent = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            0,
        );
        inconsistent
            .active_job_recovery_evidence
            .push(recovery_evidence(Uuid::nil()));
        assert!(!authorizes_automatic_idle_restart(&inconsistent));

        let diagnosis = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![DaemonRepairStep::executable(DAEMON_DIAGNOSE, diagnose())],
            0,
        );
        assert!(!authorizes_automatic_idle_restart(&diagnosis));
    }

    #[test]
    fn a_fresh_report_plans_nothing_and_says_why() {
        let plan = plan_recovery(&status(None, Vec::new(), 0));

        assert!(plan.steps.is_empty());
        assert!(!plan.executable);
        assert!(plan.reason.contains("fresh"), "{}", plan.reason);
        assert_eq!(plan.lifecycle, Plan::Converged);
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
        assert!(matches!(
            plan.lifecycle,
            Plan::Blocked {
                cause: BlockCause::Unauthorized,
                ..
            }
        ));
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

    /// A dead daemon is recovered by starting one: its startup recovery
    /// reconciles the predecessor's jobs, so no lease-specific verb exists.
    #[test]
    fn a_dead_daemon_with_jobs_is_recovered_by_starting_a_replacement() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::PidDead),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            1,
        );
        status.running = false;
        status.active_job_recovery_evidence = vec![recovery_evidence(Uuid::from_u128(1))];

        let plan = plan_recovery(&status);

        assert!(plan.executable);
        assert!(plan.required_confirmations.is_empty());
        assert_eq!(plan.steps[0].code, DAEMON_ENSURE_RUNNING);
        assert_eq!(
            plan.lifecycle,
            Plan::Transition {
                step: Step::Repair {
                    lease_id: Some(LEASE_ID.to_string()),
                    codes: vec![DAEMON_ENSURE_RUNNING.to_string()],
                },
            }
        );
    }

    /// Every job id is filled from the report; only the attestation that no
    /// report can contain is demanded from the operator (#15556 chaos case 2).
    #[test]
    fn pidless_dead_generation_jobs_demand_the_attestation() {
        let mut status = status(None, Vec::new(), 0);
        let live = recovery_evidence(Uuid::from_u128(3));
        status.active_job_recovery_evidence = vec![
            attestation_evidence(Uuid::from_u128(1)),
            attestation_evidence(Uuid::from_u128(2)),
            live,
        ];

        let plan = plan_recovery(&status);

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].code, DAEMON_ATTEST_ABSENT);
        let args = &plan.steps[0].action.as_ref().expect("argv").args;
        assert!(args.contains(&Uuid::from_u128(1).to_string()));
        assert!(args.contains(&Uuid::from_u128(2).to_string()));
        assert!(
            !args.contains(&Uuid::from_u128(3).to_string()),
            "only jobs awaiting attestation are named: {args:?}"
        );
        let Plan::NeedsAttestation(attestation) = &plan.lifecycle else {
            panic!("expected an attestation verdict: {:?}", plan.lifecycle);
        };
        assert_eq!(
            attestation.job_ids,
            vec![Uuid::from_u128(1), Uuid::from_u128(2)]
        );
        assert_eq!(
            attestation.state_dir,
            std::path::Path::new(&status.state_path).parent().unwrap()
        );
        assert_eq!(
            plan.required_confirmations,
            vec![ATTEST_ABSENT.to_string()],
            "workload absence is unverifiable and stays operator-supplied"
        );
    }

    /// Startup recovery may prove jobs an attestation would otherwise cover,
    /// so a dead daemon is started before anything is attested.
    #[test]
    fn a_dead_daemon_is_started_before_any_attestation() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::PidDead),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            1,
        );
        status.running = false;
        status.active_job_recovery_evidence = vec![attestation_evidence(Uuid::from_u128(1))];

        let plan = plan_recovery(&status);

        assert_eq!(plan.steps[0].code, DAEMON_ENSURE_RUNNING);
        assert!(plan.required_confirmations.is_empty());
    }

    #[test]
    fn admission_reporting_and_opt_in_drain_table() {
        for (reason, jobs, state) in [
            (None, 2, "serving"),
            (
                Some(DaemonStaleReasonCode::VersionMismatch),
                2,
                "stale_serving",
            ),
            (
                Some(DaemonStaleReasonCode::BuildIdentityMismatch),
                2,
                "stale_serving",
            ),
            (
                Some(DaemonStaleReasonCode::ExecutableReplaced),
                2,
                "stale_serving",
            ),
            (
                Some(DaemonStaleReasonCode::VersionMismatch),
                0,
                "unavailable",
            ),
        ] {
            let report = status(reason, Vec::new(), jobs);
            let admission = report.admission_report();
            assert_eq!(admission["state"], state);
            assert_eq!(admission["active_jobs"], jobs);
            assert_eq!(admission["daemon_version"], "0.326.1");
            if state == "stale_serving" {
                assert!(!report.admits_work());
                assert_eq!(admission["waiting_for"], "daemon_replacement");
                assert!(matches!(
                    plan_recovery(&report).lifecycle,
                    Plan::Wait { .. }
                ));
                let drain = plan_recovery_with_drain(&report, true);
                assert_eq!(drain.steps[0].code, DRAIN_THEN_REPLACE);
                assert!(drain.executable);
            }
        }
    }

    #[test]
    fn reachable_version_mismatch_with_active_jobs_never_offers_attestation() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            Vec::new(),
            2,
        );
        status.active_job_recovery_evidence = vec![
            recovery_evidence(Uuid::from_u128(1)),
            crate::api_jobs::DaemonActiveJobRecoveryEvidence {
                operation: "controller.lab.staging-dispatch".to_string(),
                linked_durable_run_id: Some("active-staging-dispatch".to_string()),
                linked_durable_run_state: Some(
                    crate::api_jobs::DaemonLinkedDurableRunState::Active,
                ),
                ..recovery_evidence(Uuid::from_u128(2))
            },
        ];

        let plan = plan_recovery(&status);

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].code, DAEMON_DIAGNOSE);
        assert!(!plan.executable);
        assert!(plan.required_confirmations.is_empty());
        assert!(matches!(
            plan.lifecycle,
            Plan::Wait {
                cause: WaitCause::BusyStaleDaemon,
                ..
            }
        ));
    }

    /// Replacement may proceed without any attestation when every active job
    /// carries durable terminal evidence: no workload remains.
    #[test]
    fn terminal_evidence_authorizes_automatic_idle_restart() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::VersionMismatch),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            0,
        );
        status.active_job_recovery_evidence = vec![terminal_recovery_evidence(Uuid::nil())];

        let plan = plan_recovery(&status);
        assert!(plan.executable);
        assert!(plan.required_confirmations.is_empty());
        assert!(authorizes_automatic_idle_restart(&status));
    }

    fn recovery_evidence(job_id: Uuid) -> crate::api_jobs::DaemonActiveJobRecoveryEvidence {
        crate::api_jobs::DaemonActiveJobRecoveryEvidence {
            job_id,
            operation: "exec".to_string(),
            status: crate::api_jobs::JobStatus::Running,
            daemon_lease_id: Some(LEASE_ID.to_string()),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            terminal_evidence: None,
            child_pid: None,
            child_started_at: None,
            controller_owned: false,
            linked_durable_run_id: None,
            linked_durable_run_state: None,
            linked_durable_run_terminal_status: None,
            disposition:
                crate::api_jobs::DaemonActiveJobRecoveryDisposition::MissingChildIdentityRecoverable,
        }
    }

    fn terminal_recovery_evidence(
        job_id: Uuid,
    ) -> crate::api_jobs::DaemonActiveJobRecoveryEvidence {
        crate::api_jobs::DaemonActiveJobRecoveryEvidence {
            linked_durable_run_id: Some("run-13619".to_string()),
            linked_durable_run_state: Some(crate::api_jobs::DaemonLinkedDurableRunState::Terminal),
            linked_durable_run_terminal_status: Some(crate::api_jobs::JobStatus::Cancelled),
            disposition: crate::api_jobs::DaemonActiveJobRecoveryDisposition::TerminalEvidence,
            ..recovery_evidence(job_id)
        }
    }

    fn attestation_evidence(job_id: Uuid) -> crate::api_jobs::DaemonActiveJobRecoveryEvidence {
        crate::api_jobs::DaemonActiveJobRecoveryEvidence {
            daemon_lease_id: Some("lease-dead".to_string()),
            disposition: crate::api_jobs::DaemonActiveJobRecoveryDisposition::NeedsAttestation,
            ..recovery_evidence(job_id)
        }
    }

    /// The owner lock is held but no live lease names its holder (a daemon is
    /// starting): the start the freshness report offers would race that
    /// holder, so the one planner blocks it and the mutation gate refuses.
    #[test]
    fn a_held_owner_lock_blocks_a_dead_lease_without_jobs() {
        let mut status = status(
            Some(super::super::DaemonStaleReasonCode::PidDead),
            vec![DaemonRepairStep::executable(
                DAEMON_ENSURE_RUNNING,
                ensure_running(),
            )],
            0,
        );
        status.running = false;
        status.owner_lock = super::super::DaemonOwnerLockProbe::Held { holder: None };

        let plan = plan_recovery(&status);

        assert!(!plan.executable);
        assert_eq!(plan.steps[0].code, DAEMON_DIAGNOSE);
        assert!(matches!(
            plan.lifecycle,
            Plan::Blocked {
                cause: BlockCause::UnleasedProcess,
                ..
            }
        ));
        assert!(super::super::lifecycle_plan::mutation_refusal(&plan.lifecycle).is_some());
    }

    #[test]
    fn diagnosis_is_the_only_read_only_action() {
        assert_eq!(diagnose().safety, ActionSafety::ReadOnly);
        for action in [
            stop(),
            stop_for_lease("lease"),
            ensure_running(),
            attest_absent(&[Uuid::nil()]),
        ] {
            assert_eq!(action.safety, ActionSafety::Mutating, "{}", action.id);
        }
    }
}
