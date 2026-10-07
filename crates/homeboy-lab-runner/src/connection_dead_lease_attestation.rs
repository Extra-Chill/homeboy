//! Operator-attested remote dead-lease recovery (#15556).
//!
//! `runner reconcile <runner> --confirm-workload-processes-absent` runs the
//! existing attested `daemon reconcile-dead-lease-orphans` on the runner, bound
//! to the exact dead generation: the lease/state-directory pair is resolved
//! from the remote daemon's own router registry (`endpoint.state_dir`, matched
//! by lease id — never a derived path), and the exact active job set is read
//! from that generation's own daemon status. Every refusal — a live daemon
//! PID, a live or unverifiable recorded child, or a changed job set — is
//! proven and enforced by the remote store itself; this module only resolves
//! the exact target and surfaces refusals verbatim.

use serde_json::Value;
use uuid::Uuid;

use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::server::SshClient;

use homeboy_core::daemon::lifecycle_plan::Plan;

use super::remote_daemon::{
    execute_attested_reconcile_command, read_remote_daemon_generation_registry,
    remote_daemon_attested_reconcile_command, remote_daemon_lifecycle_plan,
    remote_generation_status, resolve_ssh_runner, RemoteDaemonGenerationEndpoint,
};
use crate::session::RunnerStatusReport;
use crate::Runner;

/// How the controller reaches the runner's daemon frames. The real
/// implementation runs over the runner's trusted SSH transport; tests supply a
/// fake that records the executed command.
pub(crate) trait RemoteDeadLeaseAttestationTransport {
    /// The exact generation endpoints (lease id + state directory) recorded in
    /// the remote daemon's router registry.
    fn generation_registry(
        &self,
        runner_id: &str,
    ) -> std::result::Result<Vec<RemoteDaemonGenerationEndpoint>, String>;

    /// The runner daemon's own lifecycle plan (`daemon plan` in its router
    /// frame). `Ok(None)` when the runner's Homeboy predates `daemon plan`.
    fn lifecycle_plan(&self, runner_id: &str) -> std::result::Result<Option<Plan>, String>;

    /// The remote homeboy executable the generation-bound commands run.
    fn homeboy(&self) -> &str;

    /// One read-only generation-bound `daemon status`.
    fn generation_status(&self, state_dir: &str) -> std::result::Result<Value, String>;

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
    fn generation_registry(
        &self,
        runner_id: &str,
    ) -> std::result::Result<Vec<RemoteDaemonGenerationEndpoint>, String> {
        read_remote_daemon_generation_registry(&self.client, runner_id)
    }

    fn lifecycle_plan(&self, runner_id: &str) -> std::result::Result<Option<Plan>, String> {
        remote_daemon_lifecycle_plan(&self.client, runner_id, &self.homeboy_path)
    }

    fn homeboy(&self) -> &str {
        &self.homeboy_path
    }

    fn generation_status(&self, state_dir: &str) -> std::result::Result<Value, String> {
        remote_generation_status(&self.client, &self.homeboy_path, state_dir)
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

/// One probed generation: what the remote status proved about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationObservation {
    lease_id: Option<String>,
    state_dir: String,
    stale_reason_code: Option<String>,
    ownership_evidence: Option<String>,
    daemon_pid: Option<u32>,
    job_ids: std::result::Result<Vec<Uuid>, String>,
}

fn observe_generation(
    state_dir: &str,
    transport: &dyn RemoteDeadLeaseAttestationTransport,
) -> std::result::Result<GenerationObservation, String> {
    let status = transport.generation_status(state_dir)?;
    let lease_id = status
        .pointer("/state/lease_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let daemon_pid = status
        .pointer("/state/pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    let stale_reason_code = status
        .pointer("/freshness/stale_reason_code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ownership_evidence = status
        .pointer("/freshness/ownership_evidence")
        .and_then(Value::as_str)
        .map(str::to_string);
    let job_ids = status
        .get("active_job_recovery_evidence")
        .and_then(Value::as_array)
        .map(|evidence| {
            evidence
                .iter()
                .map(|entry| {
                    entry
                        .get("job_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            "remote generation status has an active job entry without a job id"
                                .to_string()
                        })
                        .and_then(|job_id| {
                            Uuid::parse_str(job_id).map_err(|error| {
                                format!("remote generation status has an unreadable job id `{job_id}`: {error}")
                            })
                        })
                })
                .collect::<std::result::Result<Vec<_>, String>>()
        })
        .unwrap_or_else(|| Ok(Vec::new()));
    Ok(GenerationObservation {
        lease_id,
        state_dir: state_dir.to_string(),
        stale_reason_code,
        ownership_evidence,
        daemon_pid,
        job_ids,
    })
}

/// Resolve the one dead generation the operator's attestation may recover.
///
/// Fail-closed: a live daemon PID, an unreadable job set, an ambiguous match,
/// or an unresolvable lease/state-dir pair is refused with the remote evidence
/// rather than guessed at.
fn resolve_attested_dead_lease(
    registry: &[RemoteDaemonGenerationEndpoint],
    transport: &dyn RemoteDeadLeaseAttestationTransport,
) -> std::result::Result<(RemoteDaemonGenerationEndpoint, Vec<Uuid>), String> {
    if registry.is_empty() {
        return Err(
            "the remote daemon generation registry is absent or empty; no lease/state-directory \
             pair can be resolved exactly, so attested dead-lease recovery is refused"
                .to_string(),
        );
    }
    let mut observations = Vec::new();
    let mut probe_errors = Vec::new();
    for endpoint in registry {
        match observe_generation(&endpoint.state_dir, transport) {
            Ok(observation) => observations.push((endpoint.clone(), observation)),
            Err(error) => {
                probe_errors.push(format!("state directory `{}`: {error}", endpoint.state_dir))
            }
        }
    }
    if observations.is_empty() {
        return Err(format!(
            "no remote generation status could be read for the registered generations: {}",
            probe_errors.join("; ")
        ));
    }
    let mut qualifying = Vec::new();
    let mut evidence = Vec::new();
    for (endpoint, observation) in &observations {
        let lease_matches = observation.lease_id.as_deref() == Some(endpoint.lease_id.as_str());
        let job_ids = match &observation.job_ids {
            Ok(job_ids) => job_ids.clone(),
            Err(error) => {
                evidence.push(format!(
                    "generation `{}` in `{}`: {error}",
                    endpoint.lease_id, endpoint.state_dir
                ));
                continue;
            }
        };
        if observation.stale_reason_code.as_deref() == Some("pid_dead")
            && lease_matches
            && !job_ids.is_empty()
        {
            qualifying.push((endpoint.clone(), job_ids));
            continue;
        }
        evidence.push(format!(
            "generation `{}` in `{}`: lease recorded {}, daemon pid {:?} reported stale reason {:?} with {} active job(s); {}",
            endpoint.lease_id,
            endpoint.state_dir,
            if lease_matches {
                "matches".to_string()
            } else {
                format!("mismatches the directory ({:?})", observation.lease_id)
            },
            observation.daemon_pid,
            observation.stale_reason_code,
            job_ids.len(),
            observation
                .ownership_evidence
                .as_deref()
                .unwrap_or("no ownership evidence was recorded"),
        ));
    }
    match qualifying.len() {
        1 => {
            let (endpoint, job_ids) = qualifying.remove(0);
            Ok((endpoint, job_ids))
        }
        0 => Err(format!(
            "no registered remote generation is a proven-dead daemon with an active job set to attest: {}",
            evidence.join("; ")
        )),
        _ => Err(format!(
            "multiple registered remote generations are proven-dead with active job sets; refusing \
             an ambiguous attestation: {}",
            qualifying
                .iter()
                .map(|(endpoint, job_ids)| format!(
                    "lease `{}` in `{}` with {} job(s)",
                    endpoint.lease_id,
                    endpoint.state_dir,
                    job_ids.len()
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// Resolve the exact dead generation and run the attested reconcile against it.
fn run_attested_reconcile_with_transport(
    runner_id: &str,
    transport: &dyn RemoteDeadLeaseAttestationTransport,
) -> Result<RemoteAttestedReconcileApplied> {
    // The runner's own lifecycle plan is authoritative (#15557 C5): it names
    // the exact generation and its complete job set, or says why nothing may
    // be attested. Older runners without `daemon plan` use the direct probes.
    let planned = transport
        .lifecycle_plan(runner_id)
        .map_err(|error| resolution_refused(runner_id, &error))?;
    let (endpoint, job_ids) = match planned {
        Some(Plan::NeedsAttestation(attestation)) => (
            RemoteDaemonGenerationEndpoint {
                lease_id: attestation.lease_id,
                state_dir: attestation.state_dir.display().to_string(),
            },
            attestation.job_ids,
        ),
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
            let registry = transport
                .generation_registry(runner_id)
                .map_err(|error| resolution_refused(runner_id, &error))?;
            resolve_attested_dead_lease(&registry, transport).map_err(|error| {
                resolution_refused(runner_id, &format!("runner `{runner_id}`: {error}"))
            })?
        }
    };
    let command = remote_daemon_attested_reconcile_command(
        transport.homeboy(),
        &endpoint.state_dir,
        &endpoint.lease_id,
        &job_ids,
    );
    let result = transport
        .execute_command(&command)
        .map_err(|error| attestation_refused(runner_id, &error, &command))?;
    Ok(RemoteAttestedReconcileApplied {
        lease_id: endpoint.lease_id,
        state_dir: endpoint.state_dir,
        job_ids,
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const STATE_DIR: &str = "/home/lab/.config/homeboy/daemon-generations/lab/controllers/ctrl/primary/generations/gen-1";

    struct FakeTransport {
        registry: std::result::Result<Vec<RemoteDaemonGenerationEndpoint>, String>,
        homeboy_path: String,
        statuses: RefCell<BTreeMap<String, Value>>,
        lifecycle: std::result::Result<Option<Plan>, String>,
        executed: RefCell<Vec<String>>,
        execute_result: RefCell<std::result::Result<Value, String>>,
    }

    impl FakeTransport {
        fn dead_generation(lease_id: &str, job_ids: &[u128]) -> Self {
            let status = serde_json::json!({
                "running": false,
                "fresh": false,
                "reachable": false,
                "state": { "lease_id": lease_id, "pid": 4242, "address": "127.0.0.1:9001" },
                "freshness": {
                    "stale_reason_code": "pid_dead",
                    "ownership_evidence":
                        "remote daemon status over SSH proved PID 4242 is dead for lease `lease-lab`",
                },
                "active_job_recovery_evidence": job_ids
                    .iter()
                    .map(|id| serde_json::json!({
                        "job_id": Uuid::from_u128(*id).to_string(),
                        "status": "running",
                        "disposition": "blocking_ambiguous",
                    }))
                    .collect::<Vec<_>>(),
            });
            let mut statuses = BTreeMap::new();
            statuses.insert(STATE_DIR.to_string(), status);
            Self {
                registry: Ok(vec![RemoteDaemonGenerationEndpoint {
                    lease_id: "lease-lab".to_string(),
                    state_dir: STATE_DIR.to_string(),
                }]),
                homeboy_path: "/opt/homeboy".to_string(),
                statuses: RefCell::new(statuses),
                // An older runner: no `daemon plan`, so the probes resolve.
                lifecycle: Ok(None),
                executed: RefCell::new(Vec::new()),
                execute_result: RefCell::new(Ok(serde_json::json!({
                    "recovered_lease_id": "lease-lab",
                    "reconciled_job_ids": job_ids
                        .iter()
                        .map(|id| Uuid::from_u128(*id).to_string())
                        .collect::<Vec<_>>(),
                }))),
            }
        }

        fn executed_commands(&self) -> Vec<String> {
            self.executed.borrow().clone()
        }
    }

    impl RemoteDeadLeaseAttestationTransport for FakeTransport {
        fn generation_registry(
            &self,
            _runner_id: &str,
        ) -> std::result::Result<Vec<RemoteDaemonGenerationEndpoint>, String> {
            self.registry.clone()
        }

        fn lifecycle_plan(&self, _runner_id: &str) -> std::result::Result<Option<Plan>, String> {
            self.lifecycle.clone()
        }

        fn homeboy(&self) -> &str {
            &self.homeboy_path
        }

        fn generation_status(&self, state_dir: &str) -> std::result::Result<Value, String> {
            self.statuses
                .borrow()
                .get(state_dir)
                .cloned()
                .ok_or_else(|| format!("no status fixture for {state_dir}"))
        }

        fn execute_command(&self, command: &str) -> std::result::Result<Value, String> {
            self.executed.borrow_mut().push(command.to_string());
            self.execute_result.borrow().clone()
        }
    }

    fn status_with_jobs(
        lease_id: &str,
        stale_reason_code: &str,
        job_ids: &[u128],
        ownership_evidence: &str,
    ) -> Value {
        serde_json::json!({
            "state": { "lease_id": lease_id, "pid": 4242 },
            "freshness": {
                "stale_reason_code": stale_reason_code,
                "ownership_evidence": ownership_evidence,
            },
            "active_job_recovery_evidence": job_ids
                .iter()
                .map(|id| serde_json::json!({ "job_id": Uuid::from_u128(*id).to_string() }))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_constructed_command_targets_the_exact_generation_state_dir_lease_and_jobs() {
        let transport = FakeTransport::dead_generation("lease-lab", &[1, 2]);
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
                "HOMEBOY_DAEMON_ROUTER_BYPASS=1 HOMEBOY_DAEMON_STATE_DIR={STATE_DIR} /opt/homeboy daemon reconcile-dead-lease-orphans --lease-id lease-lab --job-id {} --job-id {} --confirm-workload-processes-absent --no-replacement",
                Uuid::from_u128(1),
                Uuid::from_u128(2)
            )
        );
        assert_eq!(applied.command, commands[0]);
    }

    /// The dead generation is selected from the registry by its own evidence,
    /// not by admission: a live admitting generation is never attested.
    #[test]
    fn a_live_remote_daemon_pid_is_refused_with_the_remote_evidence() {
        let transport = FakeTransport::dead_generation("lease-lab", &[7]);
        transport.statuses.borrow_mut().insert(
            STATE_DIR.to_string(),
            status_with_jobs(
                "lease-lab",
                "null",
                &[7],
                "remote daemon status over SSH proved a live daemon at PID 4242 for lease `lease-lab`",
            ),
        );

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("live daemon must be refused");

        assert!(
            error
                .message
                .contains("remote daemon status over SSH proved a live daemon at PID 4242"),
            "the remote evidence must be surfaced verbatim: {}",
            error.message
        );
        assert!(
            transport.executed_commands().is_empty(),
            "no attested command may run against a live daemon"
        );
    }

    #[test]
    fn a_refusal_from_the_runner_side_is_surfaced_not_swallowed() {
        let transport = FakeTransport::dead_generation("lease-lab", &[1]);
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

    /// A generation directory that now holds a different lease is not the pair
    /// the registry described; it must never be attested through stale data.
    #[test]
    fn a_generation_directory_with_a_changed_lease_is_refused() {
        let transport = FakeTransport::dead_generation("lease-lab", &[1]);
        transport.statuses.borrow_mut().insert(
            STATE_DIR.to_string(),
            status_with_jobs(
                "lease-newer",
                "pid_dead",
                &[1],
                "the directory was restarted with a different lease",
            ),
        );

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("stale registry pair must be refused");

        assert!(
            error.message.contains("mismatches the directory"),
            "{}",
            error.message
        );
        assert!(transport.executed_commands().is_empty());
    }

    #[test]
    fn an_ambiguous_dead_job_set_is_refused_instead_of_guessed() {
        let mut transport = FakeTransport::dead_generation("lease-lab", &[1]);
        transport.registry = Ok(vec![
            RemoteDaemonGenerationEndpoint {
                lease_id: "lease-a".to_string(),
                state_dir: "/gen/a".to_string(),
            },
            RemoteDaemonGenerationEndpoint {
                lease_id: "lease-b".to_string(),
                state_dir: "/gen/b".to_string(),
            },
        ]);
        transport.statuses.borrow_mut().insert(
            "/gen/a".to_string(),
            status_with_jobs("lease-a", "pid_dead", &[1], "dead"),
        );
        transport.statuses.borrow_mut().insert(
            "/gen/b".to_string(),
            status_with_jobs("lease-b", "pid_dead", &[2], "dead"),
        );

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("ambiguous targets must be refused");

        assert!(
            error.message.contains("refusing an ambiguous attestation"),
            "{}",
            error.message
        );
        assert!(transport.executed_commands().is_empty());
    }

    #[test]
    fn an_unresolvable_registry_is_refused_without_running_anything() {
        let mut transport = FakeTransport::dead_generation("lease-lab", &[1]);
        transport.registry = Ok(Vec::new());

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("no pair, no recovery");

        assert!(
            error
                .message
                .contains("no lease/state-directory pair can be resolved exactly"),
            "{}",
            error.message
        );
        assert!(transport.executed_commands().is_empty());
    }

    #[test]
    fn a_generation_without_active_jobs_never_demands_the_attestation() {
        let transport = FakeTransport::dead_generation("lease-lab", &[]);
        transport.statuses.borrow_mut().insert(
            STATE_DIR.to_string(),
            status_with_jobs("lease-lab", "pid_dead", &[], "dead with no work"),
        );

        let error = run_attested_reconcile_with_transport("homeboy-lab", &transport)
            .expect_err("nothing to attest");

        assert!(
            error
                .message
                .contains("proven-dead daemon with an active job set"),
            "{}",
            error.message
        );
        assert!(transport.executed_commands().is_empty());
    }

    /// #15557 C5: the runner's own plan names the target; no probes run.
    #[test]
    fn the_runners_lifecycle_plan_selects_the_exact_attestation() {
        let mut transport = FakeTransport::dead_generation("lease-lab", &[1]);
        transport.registry = Err("the registry must not be read".to_string());
        transport.lifecycle = Ok(Some(Plan::NeedsAttestation(
            homeboy_core::daemon::lifecycle_plan::Attestation {
                lease_id: "lease-planned".to_string(),
                state_dir: std::path::PathBuf::from("/lab/gen-planned"),
                job_ids: vec![Uuid::from_u128(5), Uuid::from_u128(6)],
                confirmation: "confirm-workload-processes-absent".to_string(),
            },
        )));

        let applied =
            run_attested_reconcile_with_transport("homeboy-lab", &transport).expect("applied");

        assert_eq!(applied.lease_id, "lease-planned");
        assert_eq!(applied.state_dir, "/lab/gen-planned");
        assert_eq!(
            applied.job_ids,
            vec![Uuid::from_u128(5), Uuid::from_u128(6)]
        );
        assert_eq!(transport.executed_commands().len(), 1);
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
            let mut transport = FakeTransport::dead_generation("lease-lab", &[1]);
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
