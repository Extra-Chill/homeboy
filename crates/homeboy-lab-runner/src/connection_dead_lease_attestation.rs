//! Operator-attested remote dead-lease recovery (#15556).
//!
//! `runner reconcile <runner> --confirm-workload-processes-absent` runs the
//! existing attested `daemon reconcile-dead-lease-orphans` on the runner, bound
//! to the exact dead lease the runner's own lifecycle plan names: its lease,
//! state directory, and complete active job set. Every refusal — a live daemon
//! PID, a live or unverifiable recorded child, or a changed job set — is
//! proven and enforced by the remote store itself; this module only runs the
//! planned attestation and surfaces refusals verbatim.

use serde_json::Value;
use uuid::Uuid;

use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::server::SshClient;

use homeboy_core::daemon::lifecycle_plan::Plan;

use super::remote_daemon::{
    execute_attested_reconcile_command, remote_daemon_attested_reconcile_command,
    remote_daemon_lifecycle_plan, resolve_ssh_runner,
};
use crate::session::RunnerStatusReport;
use crate::Runner;

/// How the controller reaches the runner's daemon frames. The real
/// implementation runs over the runner's trusted SSH transport; tests supply a
/// fake that records the executed command.
pub(crate) trait RemoteDeadLeaseAttestationTransport {
    /// The runner daemon's own lifecycle plan (`daemon plan` in its frame).
    /// `Ok(None)` when the runner's Homeboy predates `daemon plan`.
    fn lifecycle_plan(&self, runner_id: &str) -> std::result::Result<Option<Plan>, String>;

    /// The remote homeboy executable the state-dir-bound commands run.
    fn homeboy(&self) -> &str;

    /// Execute one constructed attested reconcile command.
    fn execute_command(&self, command: &str) -> std::result::Result<Value, String>;
}

/// The SSH transport: the same bootstrap boundary `daemon stop` and
/// `ensure-running` already use for remote daemon recovery.
pub(crate) struct SshRemoteDeadLeaseAttestationTransport {
    client: SshClient,
    homeboy_path: String,
}

impl SshRemoteDeadLeaseAttestationTransport {
    pub(crate) fn for_runner(runner: &Runner) -> Result<Self> {
        let homeboy_path =
            crate::remote_runner_homeboy_path(runner, "runner attested dead-lease reconcile")?
                .to_string();
        let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
            return Err(Error::validation_invalid_argument(
                "runner",
                "attested dead-lease recovery requires an SSH-backed runner",
                Some(runner.id.clone()),
                None,
            ));
        };
        Ok(Self {
            client,
            homeboy_path,
        })
    }
}

impl RemoteDeadLeaseAttestationTransport for SshRemoteDeadLeaseAttestationTransport {
    fn lifecycle_plan(&self, runner_id: &str) -> std::result::Result<Option<Plan>, String> {
        remote_daemon_lifecycle_plan(&self.client, runner_id, &self.homeboy_path)
    }

    fn homeboy(&self) -> &str {
        &self.homeboy_path
    }

    fn execute_command(&self, command: &str) -> std::result::Result<Value, String> {
        execute_attested_reconcile_command(&self.client, command)
    }
}

/// The attested recovery this module applied, with the exact coordinates it
/// was bound to and the verbatim remote result.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteAttestedReconcileApplied {
    pub lease_id: String,
    pub state_dir: String,
    pub job_ids: Vec<Uuid>,
    pub command: String,
    pub result: Value,
}

/// Run the attested reconcile the runner's own lifecycle plan calls for.
fn run_attested_reconcile_with_transport(
    runner_id: &str,
    transport: &dyn RemoteDeadLeaseAttestationTransport,
) -> Result<RemoteAttestedReconcileApplied> {
    // The runner's own lifecycle plan is authoritative (#15557 C5): it names
    // the exact dead lease, its state directory, and its complete job set, or
    // says why nothing may be attested.
    let planned = transport
        .lifecycle_plan(runner_id)
        .map_err(|error| resolution_refused(runner_id, &error))?;
    let attestation = match planned {
        Some(Plan::NeedsAttestation(attestation)) => attestation,
        Some(other) => {
            return Err(resolution_refused(
                runner_id,
                &format!(
                    "runner `{runner_id}`: the runner's lifecycle plan does not call for a workload-absence attestation: {}",
                    describe_plan(&other)
                ),
            ))
        }
        None => {
            return Err(resolution_refused(
                runner_id,
                &format!(
                    "runner `{runner_id}`: the runner's Homeboy predates `daemon plan`, so it cannot name the exact dead lease to attest; refresh the runner's Homeboy and retry"
                ),
            ))
        }
    };
    let state_dir = attestation.state_dir.display().to_string();
    let command = remote_daemon_attested_reconcile_command(
        transport.homeboy(),
        &state_dir,
        &attestation.lease_id,
        &attestation.job_ids,
    );
    let result = transport
        .execute_command(&command)
        .map_err(|error| attestation_refused(runner_id, &error, &command))?;
    Ok(RemoteAttestedReconcileApplied {
        lease_id: attestation.lease_id,
        state_dir,
        job_ids: attestation.job_ids,
        command,
        result,
    })
}

fn describe_plan(plan: &Plan) -> String {
    match plan {
        Plan::Wait { reason, .. } => format!("wait: {reason}"),
        Plan::Blocked { reason, .. } => format!("blocked: {reason}"),
        Plan::Converged => "converged: nothing to recover".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// Run the attested remote dead-lease recovery for one runner over its SSH
/// transport. Called only after a reconcile was blocked by daemon ownership
/// evidence and the operator supplied the workload-absence attestation.
pub(crate) fn run_attested_dead_lease_reconcile(
    runner_id: &str,
) -> Result<RemoteAttestedReconcileApplied> {
    let runner = crate::load(runner_id)?;
    let transport = SshRemoteDeadLeaseAttestationTransport::for_runner(&runner)?;
    run_attested_reconcile_with_transport(runner_id, &transport)
}

/// The runner daemon's own lifecycle plan, read-only (#15557 C5).
/// `None` when the runner's Homeboy predates `daemon plan`.
pub fn remote_lifecycle_plan(runner_id: &str) -> Result<Option<Plan>> {
    let runner = crate::load(runner_id)?;
    let transport = SshRemoteDeadLeaseAttestationTransport::for_runner(&runner)?;
    transport
        .lifecycle_plan(runner_id)
        .map_err(|error| resolution_refused(runner_id, &error))
}

fn resolution_refused(runner_id: &str, problem: &str) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        problem.to_string(),
        serde_json::json!({
            "runner_id": runner_id,
            "recovery": "attested_remote_dead_lease_reconcile",
        }),
    )
}

fn attestation_refused(runner_id: &str, refusal: &str, command: &str) -> Error {
    Error::new(
        ErrorCode::RemoteCommandFailed,
        refusal.to_string(),
        serde_json::json!({
            "runner_id": runner_id,
            "recovery": "attested_remote_dead_lease_reconcile",
            "command": command,
        }),
    )
}

/// Whether a reconcile result is blocked by the terminal daemon-ownership
/// evidence blocker this attestation recovers from.
pub(crate) fn blocked_by_daemon_ownership_evidence(status: &RunnerStatusReport) -> bool {
    status
        .daemon_freshness
        .as_ref()
        .is_some_and(|freshness| freshness.has_terminal_recovery_ownership_blocker())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use homeboy_core::daemon::lifecycle_plan::Attestation;
    use std::cell::RefCell;

    const STATE_DIR: &str = "/home/lab/.config/homeboy/runner-daemons/lab/ctrl";

    struct FakeTransport {
        homeboy_path: String,
        lifecycle: std::result::Result<Option<Plan>, String>,
        executed: RefCell<Vec<String>>,
        execute_result: RefCell<std::result::Result<Value, String>>,
    }

    impl FakeTransport {
        /// A runner whose own plan calls for attesting `job_ids` of a dead lease.
        fn dead_lease(lease_id: &str, job_ids: &[u128]) -> Self {
            let job_ids: Vec<Uuid> = job_ids.iter().map(|id| Uuid::from_u128(*id)).collect();
            Self {
                homeboy_path: "/opt/homeboy".to_string(),
                lifecycle: Ok(Some(Plan::NeedsAttestation(Attestation {
                    lease_id: lease_id.to_string(),
                    state_dir: std::path::PathBuf::from(STATE_DIR),
                    job_ids: job_ids.clone(),
                    confirmation: "confirm-workload-processes-absent".to_string(),
                }))),
                executed: RefCell::new(Vec::new()),
                execute_result: RefCell::new(Ok(serde_json::json!({
                    "recovered_lease_id": lease_id,
                    "reconciled_job_ids": job_ids,
                }))),
            }
        }

        fn executed_commands(&self) -> Vec<String> {
            self.executed.borrow().clone()
        }
    }

    impl RemoteDeadLeaseAttestationTransport for FakeTransport {
        fn lifecycle_plan(&self, _runner_id: &str) -> std::result::Result<Option<Plan>, String> {
            self.lifecycle.clone()
        }

        fn homeboy(&self) -> &str {
            &self.homeboy_path
        }

        fn execute_command(&self, command: &str) -> std::result::Result<Value, String> {
            self.executed.borrow_mut().push(command.to_string());
            self.execute_result.borrow().clone()
        }
    }

    #[test]
    fn the_constructed_command_targets_the_planned_state_dir_lease_and_jobs() {
        let transport = FakeTransport::dead_lease("lease-lab", &[1, 2]);
        let applied =
            run_attested_reconcile_with_transport("homeboy-lab", &transport).expect("applied");

        assert_eq!(applied.lease_id, "lease-lab");
        assert_eq!(applied.state_dir, STATE_DIR);
        assert_eq!(
            applied.job_ids,
            vec![Uuid::from_u128(1), Uuid::from_u128(2)]
        );
        let commands = transport.executed_commands();
        assert_eq!(commands.len(), 1, "exactly one remote command runs");
        assert_eq!(
            commands[0],
            format!(
                "HOMEBOY_DAEMON_STATE_DIR={STATE_DIR} /opt/homeboy daemon reconcile-dead-lease-orphans --lease-id lease-lab --job-id {} --job-id {} --confirm-workload-processes-absent --no-replacement",
                Uuid::from_u128(1),
                Uuid::from_u128(2)
            )
        );
        assert_eq!(applied.command, commands[0]);
    }

    #[test]
    fn a_refusal_from_the_runner_side_is_surfaced_not_swallowed() {
        let transport = FakeTransport::dead_lease("lease-lab", &[1]);
        *transport.execute_result.borrow_mut() = Err(
            "remote attested dead-lease reconcile refused: Invalid argument 'job_id': dead-daemon \
             recovery job IDs must name the exact active durable-job set; command: \
             HOMEBOY_DAEMON_STATE_DIR=... daemon reconcile-dead-lease-orphans"
                .to_string(),
        );

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("the remote refusal must fail the operation");

        assert!(
            error.message.contains(
                "dead-daemon recovery job IDs must name the exact active durable-job set"
            ),
            "the runner-side refusal must survive verbatim: {}",
            error.message
        );
        assert_eq!(error.code.as_str(), "remote.command_failed");
    }

    #[test]
    fn a_runner_without_daemon_plan_is_refused_without_running_anything() {
        let mut transport = FakeTransport::dead_lease("lease-lab", &[1]);
        transport.lifecycle = Ok(None);

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("no plan, no recovery");

        assert!(
            error.message.contains("refresh the runner's Homeboy"),
            "{}",
            error.message
        );
        assert!(transport.executed_commands().is_empty());
    }

    #[test]
    fn a_runner_plan_that_waits_or_blocks_is_refused_with_its_reason() {
        for plan in [
            Plan::Wait {
                cause: homeboy_core::daemon::lifecycle_plan::WaitCause::LiveWorkload,
                reason: "job 9 still has a live workload process".to_string(),
            },
            Plan::Blocked {
                cause: homeboy_core::daemon::lifecycle_plan::BlockCause::UnleasedProcess,
                reason: "pid 7 holds no lease".to_string(),
            },
            Plan::Converged,
        ] {
            let mut transport = FakeTransport::dead_lease("lease-lab", &[1]);
            transport.lifecycle = Ok(Some(plan.clone()));
            let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
                .expect_err("refused");
            assert!(
                error
                    .message
                    .contains("does not call for a workload-absence attestation"),
                "{}",
                error.message
            );
            assert!(transport.executed_commands().is_empty(), "{plan:?}");
        }
    }
}
