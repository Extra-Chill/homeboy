use clap::{Args, CommandFactory, Subcommand};
use serde::Serialize;
use std::path::PathBuf;
use uuid::Uuid;

use homeboy::core::daemon::{
    self, BrokerConfig, BrokerConfigOptions, DaemonExactOrphanRecoveryResult,
    DaemonLeaselessRecoveryResult, DaemonOrphanAdoptionResult, DaemonStartResult,
    DaemonStateLossRecoveryResult, DaemonStatus, DaemonStopResult, ServiceIdentity,
};
use homeboy::core::http_api::{AnalysisJobRunOutput, AnalysisJobRunner};
use homeboy::core::Error;

use super::CmdResult;

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the local daemon in the background
    Start {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Return the current live daemon or start one when no live daemon exists
    EnsureRunning {
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Controller-generated idempotency key for a replacement operation.
        #[arg(long)]
        replacement_operation_id: Option<String>,
    },
    /// Resolve and run the right daemon recovery from the current status report
    ///
    /// Reads `homeboy daemon status` once, matches its stale reason code, and
    /// fills every argument the resolved recovery needs from that report. The
    /// explicit subcommands below stay available as escape hatches for the
    /// cases this cannot resolve.
    Recover {
        /// Print the resolved plan without running it. This is the default.
        #[arg(long)]
        dry_run: bool,
        /// Run the resolved plan.
        #[arg(long, conflicts_with = "dry_run")]
        yes: bool,
        /// Required only when the resolved plan reconciles PID-less durable
        /// jobs. Unverifiable by design, so it is never supplied automatically:
        /// the daemon died before persisting any child identity, leaving no PID
        /// for anything in process to check.
        #[arg(long)]
        confirm_workload_processes_absent: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Explicitly replace one proven-dead daemon lease and reconcile its durable jobs
    AdoptOrphan {
        /// Exact lease ID reported by `homeboy daemon status`
        #[arg(long)]
        lease_id: String,
        /// Deprecated no-op retained for one release; adoption already proves the
        /// recorded PID dead under the daemon lifecycle lock.
        #[arg(long)]
        confirm_pid_dead: bool,
        /// Accepted migration alias for legacy child recovery. It never mutates jobs.
        #[arg(long)]
        recover_missing_child_identity: bool,
        /// Confirm the one expired PID-less reservation to terminalize before replacement.
        #[arg(long = "confirm-untracked-child-dead")]
        confirm_untracked_child_dead: Vec<Uuid>,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Reconcile an exact PID-less job set after one proven unexpected daemon exit
    ReconcileDeadLeaseOrphans {
        #[arg(long)]
        lease_id: String,
        /// Exact, complete active durable-job set to terminalize. The store
        /// recomputes the active set and refuses any mismatch, so this is a
        /// compare-and-swap over the destructive scope, not a fact assertion.
        #[arg(long = "job-id", required = true)]
        job_ids: Vec<Uuid>,
        /// Deprecated no-op retained for one release; recovery already requires
        /// persisted unexpected-termination evidence and re-proves the PID dead.
        #[arg(long)]
        confirm_pid_dead: bool,
        /// Required. Attests that the workload processes for --job-id were
        /// inspected and are absent. Unverifiable by design: this command exists
        /// because the daemon died before persisting any child identity, so the
        /// store holds no PID to check. Persisted as durable job provenance.
        #[arg(long)]
        confirm_workload_processes_absent: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Recover one legacy job with exact PID and Linux start-tick evidence.
    RecoverMissingChildIdentity {
        #[arg(long)]
        lease_id: String,
        #[arg(long)]
        recorded_daemon_pid: u32,
        #[arg(long)]
        recorded_daemon_endpoint: String,
        #[arg(long)]
        job_id: Uuid,
        #[arg(long)]
        child_pid: u32,
        #[arg(long)]
        child_starttime_ticks: u64,
    },
    /// Explicitly reconcile active jobs after proving a missing-lease store has no daemon owner
    ReconcileLeaselessOrphans {
        // Deprecated no-op retained for one release: recovery already fails
        // closed on the daemon owner lock, daemon process candidates, and a
        // reachable listener at --addr.
        //
        // This deliberately carries NO doc comment. Controllers negotiate the
        // lease-less recovery contract by running this subcommand's `--help` on
        // the remote and matching bare long options
        // (`connection::declared_long_options` only accepts an option whose
        // trailing tokens are value placeholders). Rendering help text after
        // `--confirm-no-daemon-owner` removes it from the advertised contract
        // and makes every controller refuse remote lease-less recovery. The
        // deprecation is documented in `docs/commands/daemon.md` instead.
        #[arg(long)]
        confirm_no_daemon_owner: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Controller-generated idempotency key for a replacement operation.
        #[arg(long)]
        replacement_operation_id: Option<String>,
    },
    /// Recover one exact lease after its daemon state record was lost
    RecoverMissingLeaseState {
        /// Exact lease ID captured before the daemon state record was lost
        #[arg(long)]
        lease_id: String,
        /// Recorded daemon PID captured with the lease ID
        #[arg(long)]
        recorded_pid: u32,
        /// Recorded concrete loopback endpoint captured with the lease ID
        #[arg(long)]
        recorded_endpoint: String,
        /// Deprecated no-op retained for one release; recovery already refuses a
        /// running recorded PID.
        #[arg(long)]
        confirm_pid_dead: bool,
        /// Deprecated no-op retained for one release; recovery already requires an
        /// absent state record, a `lease_missing` freshness code, an unreachable
        /// daemon, and a failed probe of the recorded endpoint.
        #[arg(long)]
        confirm_control_plane_lost: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Controller-generated idempotency key for a replacement operation.
        #[arg(long)]
        replacement_operation_id: Option<String>,
    },
    /// Run the local daemon in the foreground
    Serve {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Exact startup admission identity, forwarded by the supervisor.
        #[arg(long, hide = true)]
        startup_token: Option<String>,
    },
    /// Supervise one daemon child and persist its termination evidence.
    #[command(hide = true)]
    Supervise {
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        #[arg(long)]
        startup_token: String,
    },
    /// Stop the background daemon recorded in the state file
    Stop {
        /// Require this exact live daemon lease before stopping
        #[arg(long)]
        lease_id: Option<String>,
        /// Directly SIGTERM a matching stale or unreachable daemon lease. Requires --lease-id.
        #[arg(long, requires = "lease_id")]
        force: bool,
    },
    /// Show daemon state and selected local address
    Status,
    /// Render deployable reverse-runner broker service configuration
    BrokerConfig {
        /// Stable loopback address for the VPS service.
        #[arg(long, default_value = "127.0.0.1:7421")]
        listen_addr: String,
        /// Homeboy binary path used by the service unit.
        #[arg(long, default_value = "/usr/local/bin/homeboy")]
        binary_path: String,
        /// System user that runs the broker service.
        #[arg(long, default_value = "homeboy")]
        user: String,
        /// System group that runs the broker service.
        #[arg(long, default_value = "homeboy")]
        group: String,
        /// Optional public hostname to render disabled Nginx/Caddy examples.
        #[arg(long)]
        domain: Option<String>,
    },
    /// Fetch artifact bytes through the local daemon byte endpoint
    ArtifactGet(DaemonArtifactGetArgs),
}

#[derive(Args, Clone)]
pub struct DaemonArtifactGetArgs {
    /// Observation run id that owns the artifact
    pub run_id: String,
    /// Artifact id/path token from daemon artifact metadata
    pub artifact_id: String,
    /// Destination file path. Defaults to the artifact id basename.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// Daemon base URL. Defaults to the address from `homeboy daemon status`.
    #[arg(long = "daemon-url")]
    pub daemon_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DaemonOutput {
    Start(DaemonStartResult),
    EnsureRunning(DaemonStartResult),
    Recover(DaemonRecoverOutput),
    AdoptOrphan(DaemonOrphanAdoptionResult),
    ReconcileDeadLeaseOrphans(DaemonExactOrphanRecoveryResult),
    RecoverMissingChildIdentity(homeboy::core::api_jobs::Job),
    ReconcileLeaselessOrphans(DaemonLeaselessRecoveryResult),
    RecoverMissingLeaseState(DaemonStateLossRecoveryResult),
    Serve(DaemonStartResult),
    Stop(DaemonStopResult),
    Status(DaemonStatus),
    BrokerConfig(BrokerConfig),
    ArtifactGet(DaemonArtifactGetOutput),
}

/// The resolved recovery, and what was done about it.
///
/// `plan` is the whole point: every argument in it was filled from the status
/// read in this same invocation, so an operator never transcribes a lease id, a
/// PID, an endpoint, or a `/proc` start-tick value between two commands.
#[derive(Debug, Serialize)]
pub struct DaemonRecoverOutput {
    pub command: &'static str,
    /// `false` for a dry run, which is the default.
    pub executed: bool,
    pub fresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason_code: Option<daemon::DaemonStaleReasonCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    pub active_jobs: usize,
    pub plan: daemon::recovery_actions::DaemonRecoveryPlan,
    /// The steps actually run, in order. Empty on a dry run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applied_steps: Vec<String>,
    /// Why execution stopped short, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    pub next_command: String,
}

#[derive(Debug, Serialize)]
pub struct DaemonArtifactGetOutput {
    pub command: &'static str,
    pub run_id: String,
    pub artifact_id: String,
    pub daemon_url: String,
    pub content_url: String,
    pub output_path: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

pub fn run(args: DaemonArgs) -> CmdResult<DaemonOutput> {
    match args.command {
        DaemonCommand::Start { addr } => {
            Ok((DaemonOutput::Start(daemon::start_background(&addr)?), 0))
        }
        DaemonCommand::EnsureRunning {
            addr,
            replacement_operation_id,
        } => Ok((
            DaemonOutput::EnsureRunning(daemon::ensure_running_with_replacement_operation(
                &addr,
                replacement_operation_id.as_deref(),
            )?),
            0,
        )),
        DaemonCommand::Recover {
            dry_run,
            yes,
            confirm_workload_processes_absent,
            addr,
        } => recover(
            // Dry run is the default: an unasked-for recovery is a mutation of
            // the daemon that owns the caller's durable jobs.
            !yes || dry_run,
            confirm_workload_processes_absent,
            &addr,
        ),
        DaemonCommand::AdoptOrphan {
            lease_id,
            // Deprecated no-op: adoption proves PID death itself, under the lock.
            confirm_pid_dead: _deprecated_confirm_pid_dead,
            recover_missing_child_identity,
            confirm_untracked_child_dead,
            addr,
        } => {
            if recover_missing_child_identity {
                return Err(legacy_child_recovery_migration_error());
            }
            Ok((
                DaemonOutput::AdoptOrphan(daemon::adopt_orphaned_lease(
                    &lease_id,
                    &confirm_untracked_child_dead,
                    &addr,
                )?),
                0,
            ))
        }
        // `confirm_workload_processes_absent` stays load-bearing: no PID exists
        // for these jobs, so only the operator can attest workload absence.
        DaemonCommand::ReconcileDeadLeaseOrphans {
            lease_id,
            job_ids,
            confirm_pid_dead: _deprecated_confirm_pid_dead,
            confirm_workload_processes_absent,
            addr,
        } => Ok((
            DaemonOutput::ReconcileDeadLeaseOrphans(daemon::reconcile_dead_lease_orphans(
                &lease_id,
                &job_ids,
                confirm_workload_processes_absent,
                &addr,
            )?),
            0,
        )),
        DaemonCommand::RecoverMissingChildIdentity {
            lease_id,
            recorded_daemon_pid,
            recorded_daemon_endpoint,
            job_id,
            child_pid,
            child_starttime_ticks,
        } => Ok((
            DaemonOutput::RecoverMissingChildIdentity(daemon::recover_missing_child_identity(
                &lease_id,
                recorded_daemon_pid,
                &recorded_daemon_endpoint,
                job_id,
                child_pid,
                child_starttime_ticks,
            )?),
            0,
        )),
        DaemonCommand::ReconcileLeaselessOrphans {
            confirm_no_daemon_owner: _deprecated_confirm_no_daemon_owner,
            addr,
            replacement_operation_id,
        } => Ok((
            DaemonOutput::ReconcileLeaselessOrphans(daemon::reconcile_leaseless_orphans(
                &addr,
                replacement_operation_id.as_deref(),
            )?),
            0,
        )),
        DaemonCommand::RecoverMissingLeaseState {
            lease_id,
            recorded_pid,
            recorded_endpoint,
            confirm_pid_dead: _deprecated_confirm_pid_dead,
            confirm_control_plane_lost: _deprecated_confirm_control_plane_lost,
            addr,
            replacement_operation_id,
        } => Ok((
            DaemonOutput::RecoverMissingLeaseState(daemon::recover_missing_lease_state(
                &lease_id,
                recorded_pid,
                &recorded_endpoint,
                &addr,
                replacement_operation_id.as_deref(),
            )?),
            0,
        )),
        DaemonCommand::Serve {
            addr,
            startup_token,
        } => {
            // Supervision supplies the token through the environment; the
            // hidden argument is retained as the portable ownership proof.
            let _ = startup_token;
            serve(&addr)
        }
        DaemonCommand::Supervise {
            addr,
            startup_token,
        } => {
            daemon::supervise(&addr, &startup_token)?;
            Ok((
                DaemonOutput::Serve(DaemonStartResult {
                    pid: std::process::id(),
                    address: addr,
                    state_path: String::new(),
                    lease_id: String::new(),
                }),
                0,
            ))
        }
        DaemonCommand::Stop { lease_id, force } => {
            let lease_bound = lease_id.is_some();
            let result = match (force, lease_id) {
                (true, Some(lease_id)) => daemon::force_stop_for_lease(&lease_id)?,
                (true, None) => unreachable!("clap requires --lease-id with --force"),
                (false, Some(lease_id)) => daemon::stop_for_lease(&lease_id)?,
                (false, None) => daemon::stop()?,
            };
            if lease_bound && !result.stopped && !result.already_absent {
                return Err(Error::validation_invalid_argument(
                    "daemon_stop",
                    "lease-bound daemon stop did not stop the exact owner or prove it already absent",
                    None,
                    None,
                ));
            }
            Ok((DaemonOutput::Stop(result), 0))
        }
        DaemonCommand::Status => Ok((DaemonOutput::Status(daemon::read_status()?), 0)),
        DaemonCommand::BrokerConfig {
            listen_addr,
            binary_path,
            user,
            group,
            domain,
        } => Ok((
            DaemonOutput::BrokerConfig(daemon::render_broker_config(BrokerConfigOptions {
                listen_addr,
                binary_path,
                identity: ServiceIdentity {
                    service_user: user,
                    service_group: group,
                },
                domain,
            })?),
            0,
        )),
        DaemonCommand::ArtifactGet(args) => artifact_get(args),
    }
}

/// Resolve the recovery for the local daemon and either print it or run it.
///
/// One `read_status()` read supplies every argument. Dispatch is on the typed
/// step code, never on the rendered command string (#11105).
fn recover(
    dry_run: bool,
    confirm_workload_processes_absent: bool,
    addr: &str,
) -> CmdResult<DaemonOutput> {
    use daemon::recovery_actions as actions;

    let status = daemon::read_status()?;
    let plan = actions::plan_recovery(&status);
    let lease_id = status.freshness.lease_id.clone();
    let job_ids: Vec<Uuid> = status
        .active_job_recovery_evidence
        .iter()
        .map(|evidence| evidence.job_id)
        .collect();

    let mut output = DaemonRecoverOutput {
        command: "daemon.recover",
        executed: false,
        fresh: status.fresh,
        stale_reason_code: status.freshness.stale_reason_code,
        lease_id: lease_id.clone(),
        active_jobs: status.freshness.active_jobs,
        applied_steps: Vec::new(),
        blocked_on: None,
        next_command: String::new(),
        plan,
    };

    if output.plan.steps.is_empty() {
        output.next_command = "homeboy daemon status".to_string();
        return Ok((DaemonOutput::Recover(output), 0));
    }
    if !output.plan.executable {
        // A read-only diagnosis, or evidence that authorizes nothing. The plan
        // and its reason are the answer; running it would be a guess.
        output.blocked_on = Some(output.plan.reason.clone());
        output.next_command = rendered_plan(&output.plan);
        return Ok((DaemonOutput::Recover(output), 0));
    }

    // Confirmations an operator has to make are surfaced, never synthesized.
    let missing = unmet_confirmations(&output.plan, confirm_workload_processes_absent);
    if !missing.is_empty() {
        output.blocked_on = Some(format!(
            "this recovery requires operator attestation that no report can supply: {}",
            missing
                .iter()
                .map(|confirmation| format!("--{confirmation}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        output.next_command = format!(
            "{} --{}",
            rendered_plan(&output.plan),
            actions::CONFIRM_WORKLOAD_PROCESSES_ABSENT
        );
        return Ok((DaemonOutput::Recover(output), 0));
    }

    if dry_run {
        output.next_command = "homeboy daemon recover --yes".to_string();
        return Ok((DaemonOutput::Recover(output), 0));
    }

    for step in &output.plan.steps {
        match step.code.as_str() {
            // A lease-bound stop refuses to kill a daemon other than the exact
            // one the report described, which is why the lease is carried here
            // rather than left to a bare `homeboy daemon stop`.
            code if code == actions::DAEMON_STOP => match lease_id.as_deref() {
                Some(lease_id) => {
                    daemon::stop_for_lease(lease_id)?;
                }
                None => {
                    daemon::stop()?;
                }
            },
            code if code == actions::DAEMON_START => {
                daemon::start_background(addr)?;
            }
            code if code == actions::DAEMON_ADOPT_ORPHAN => {
                let lease_id = lease_id.as_deref().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "lease_id",
                        "the status report named an orphan adoption but carried no lease id",
                        None,
                        None,
                    )
                })?;
                daemon::adopt_orphaned_lease(lease_id, &[], addr)?;
            }
            code if code == actions::DAEMON_RECONCILE_LEASELESS_ORPHANS => {
                daemon::reconcile_leaseless_orphans(addr, None)?;
            }
            code if code == actions::DAEMON_RECONCILE_DEAD_LEASE_ORPHANS => {
                let lease_id = lease_id.as_deref().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "lease_id",
                        "the status report named a dead-lease reconciliation but carried no lease id",
                        None,
                        None,
                    )
                })?;
                daemon::reconcile_dead_lease_orphans(
                    lease_id,
                    &job_ids,
                    confirm_workload_processes_absent,
                    addr,
                )?;
            }
            code => {
                return Err(Error::validation_invalid_argument(
                    "recovery_step",
                    format!("resolved recovery step `{code}` has no dispatcher"),
                    Some(code.to_string()),
                    Some(vec![
                        "Run `homeboy daemon status` and use the explicit recovery subcommand for this case.".to_string(),
                    ]),
                ));
            }
        }
        output.applied_steps.push(step.code.clone());
    }

    output.executed = true;
    output.next_command = "homeboy daemon status".to_string();
    Ok((DaemonOutput::Recover(output), 0))
}

/// Attestations the resolved plan needs and the operator has not made.
///
/// The dispatcher fills arguments from the report; it does not fill in claims
/// about the world. `--confirm-workload-processes-absent` is the one input on
/// this surface that no report can contain — `core/daemon/control.rs` documents
/// why — so it is reported back as a blocker rather than assumed.
fn unmet_confirmations(
    plan: &daemon::recovery_actions::DaemonRecoveryPlan,
    confirm_workload_processes_absent: bool,
) -> Vec<String> {
    plan.required_confirmations
        .iter()
        .filter(|confirmation| {
            confirmation.as_str() != daemon::recovery_actions::CONFIRM_WORKLOAD_PROCESSES_ABSENT
                || !confirm_workload_processes_absent
        })
        .cloned()
        .collect()
}

fn rendered_plan(plan: &daemon::recovery_actions::DaemonRecoveryPlan) -> String {
    plan.steps
        .iter()
        .map(|step| step.command.as_str())
        .collect::<Vec<_>>()
        .join(" && ")
}

fn legacy_child_recovery_migration_error() -> Error {
    Error::validation_invalid_argument(
        "recover_missing_child_identity",
        format!(
            "--recover-missing-child-identity is migration-only. Recover each job with exact persisted evidence instead"
        ),
        None,
        Some(vec![
            "Use `homeboy daemon recover-missing-child-identity --lease-id <expected-lease> --recorded-daemon-pid <recorded-daemon-pid> --recorded-daemon-endpoint <recorded-daemon-endpoint> --job-id <job-id> --child-pid <child-pid> --child-starttime-ticks <child-starttime-ticks>`.".to_string(),
            "Required exact evidence: expected lease, recorded daemon PID, recorded daemon endpoint, job ID, child PID, and child starttime ticks.".to_string(),
        ]),
    )
}

fn artifact_get(args: DaemonArtifactGetArgs) -> CmdResult<DaemonOutput> {
    let outcome = daemon::fetch_artifact_to_path(
        &args.run_id,
        &args.artifact_id,
        args.daemon_url,
        args.output,
    )?;

    Ok((
        DaemonOutput::ArtifactGet(DaemonArtifactGetOutput {
            command: "daemon.artifact.get",
            run_id: args.run_id,
            artifact_id: args.artifact_id,
            daemon_url: outcome.daemon_url,
            content_url: outcome.content_url,
            output_path: outcome.output_path.display().to_string(),
            content_type: outcome.content_type,
            size_bytes: Some(outcome.size_bytes),
            sha256: outcome.sha256,
        }),
        0,
    ))
}

fn serve(addr: &str) -> CmdResult<DaemonOutput> {
    let parsed = daemon::parse_bind_addr(addr)?;
    let state = daemon::serve_with_analysis_runner(parsed, CommandAnalysisJobRunner)?;
    Ok((
        DaemonOutput::Serve(DaemonStartResult {
            pid: state.pid,
            address: state.address,
            state_path: state.state_path,
            lease_id: state.lease_id,
        }),
        0,
    ))
}

#[derive(Debug, Clone, Copy)]
struct CommandAnalysisJobRunner;

impl AnalysisJobRunner for CommandAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        let matches = crate::cli_surface::Cli::command()
            .try_get_matches_from(argv)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "body",
                    error.to_string(),
                    None,
                    Some(vec![
                        "Use the documented JSON request body contract for this endpoint"
                            .to_string(),
                    ]),
                )
            })?;
        let (cli, spec) = crate::cli_surface::Cli::from_registered_arg_matches(&matches)
            .expect("validated arguments should produce a typed CLI");
        let (result, exit_code) = crate::commands::json_output::run(cli.command, spec);
        Ok(AnalysisJobRunOutput {
            exit_code,
            output: result?,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use homeboy::test_support::with_isolated_home;

    use super::*;
    use crate::cli_surface::{Cli, Commands};

    #[test]
    fn legacy_child_recovery_parser_requires_exact_evidence() {
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "recover-missing-child-identity"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover-missing-child-identity",
            "--lease-id",
            "lease",
            "--recorded-daemon-pid",
            "nope",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover-missing-child-identity",
            "--lease-id",
            "lease",
            "--recorded-daemon-pid",
            "42",
            "--recorded-daemon-endpoint",
            "127.0.0.1:1",
            "--job-id",
            "not-a-uuid",
            "--child-pid",
            "43",
            "--child-starttime-ticks",
            "1",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover-missing-child-identity",
            "--lease-id",
            "lease",
            "--recorded-daemon-pid",
            "42",
            "--recorded-daemon-endpoint",
            "127.0.0.1:1",
            "--job-id",
            "00000000-0000-0000-0000-000000000001",
            "--child-pid",
            "43",
            "--child-starttime-ticks",
            "1",
        ])
        .is_ok());
    }

    /// `--confirm-workload-processes-absent` is the one confirmation on this
    /// surface that is not ceremony. This command exists because the daemon died
    /// before persisting any child identity, so the store holds no PID for the
    /// named jobs and nothing in-process can observe whether their workloads are
    /// still running. The attestation is the sole evidence, and it is persisted
    /// as durable job provenance, so it must stay required.
    #[test]
    fn exact_dead_lease_recovery_still_requires_the_workload_absence_attestation() {
        for args in [
            vec![
                "homeboy",
                "daemon",
                "reconcile-dead-lease-orphans",
                "--lease-id",
                "lease-dead",
                "--job-id",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "homeboy",
                "daemon",
                "reconcile-dead-lease-orphans",
                "--lease-id",
                "lease-dead",
                "--job-id",
                "00000000-0000-0000-0000-000000000001",
                "--confirm-pid-dead",
            ],
        ] {
            let cli = Cli::try_parse_from(args).expect("recovery command parses");
            let Commands::Daemon(args) = cli.command else {
                panic!("expected daemon command");
            };
            let error = run(args)
                .expect_err("missing operator attestation is rejected before state access");
            assert!(
                error.message.contains("confirm-workload-processes-absent"),
                "expected the workload attestation refusal, got {}",
                error.message
            );
            assert!(
                !error.message.contains("--confirm-pid-dead"),
                "PID death is proven by the lifecycle controller, not asserted: {}",
                error.message
            );
        }
    }

    /// PID death, control-plane loss, and daemon-owner absence are all proven by
    /// the lifecycle controller before it mutates anything, so none of them may
    /// be demanded as an operator assertion at the argument layer. Each command
    /// below must get past argument validation without its former confirmation
    /// and fail on real state instead.
    #[test]
    fn deprecated_confirmations_are_no_longer_demanded() {
        with_isolated_home(|_| {
            for (args, refused_flag) in [
                (
                    vec![
                        "homeboy",
                        "daemon",
                        "adopt-orphan",
                        "--lease-id",
                        "lease-dead",
                    ],
                    "--confirm-pid-dead",
                ),
                (
                    vec!["homeboy", "daemon", "reconcile-leaseless-orphans"],
                    "--confirm-no-daemon-owner",
                ),
                (
                    vec![
                        "homeboy",
                        "daemon",
                        "recover-missing-lease-state",
                        "--lease-id",
                        "lease-dead",
                        "--recorded-pid",
                        "4242",
                        "--recorded-endpoint",
                        "127.0.0.1:4242",
                    ],
                    "--confirm-control-plane-lost",
                ),
            ] {
                let cli = Cli::try_parse_from(args).expect("recovery command parses");
                let Commands::Daemon(args) = cli.command else {
                    panic!("expected daemon command");
                };
                let error =
                    run(args).expect_err("an isolated home has no recoverable daemon state");
                assert!(
                    !error.message.contains(refused_flag),
                    "{refused_flag} must no longer gate recovery, got {}",
                    error.message
                );
            }
        });
    }

    /// The released spellings still appear in composed remote commands and in
    /// operator runbooks. They must keep parsing for one release, and — because
    /// controllers negotiate the lease-less recovery contract by reading
    /// `--confirm-no-daemon-owner` out of the remote's `--help` output — they
    /// must also stay visible in help rather than being hidden.
    #[test]
    fn deprecated_confirmations_remain_accepted_and_advertised() {
        for args in [
            vec![
                "homeboy",
                "daemon",
                "adopt-orphan",
                "--lease-id",
                "lease-dead",
                "--confirm-pid-dead",
            ],
            vec![
                "homeboy",
                "daemon",
                "reconcile-leaseless-orphans",
                "--confirm-no-daemon-owner",
            ],
            vec![
                "homeboy",
                "daemon",
                "recover-missing-lease-state",
                "--lease-id",
                "lease-dead",
                "--recorded-pid",
                "4242",
                "--recorded-endpoint",
                "127.0.0.1:4242",
                "--confirm-pid-dead",
                "--confirm-control-plane-lost",
            ],
        ] {
            assert!(
                Cli::try_parse_from(args.clone()).is_ok(),
                "released spelling must stay accepted: {args:?}"
            );
        }

        let mut command = Cli::command();
        let daemon = command
            .find_subcommand_mut("daemon")
            .expect("daemon subcommand");
        let leaseless = daemon
            .find_subcommand_mut("reconcile-leaseless-orphans")
            .expect("reconcile-leaseless-orphans subcommand");
        let help = leaseless.render_help().to_string();
        assert!(
            help.contains("--confirm-no-daemon-owner"),
            "controllers negotiate this contract from remote help output: {help}"
        );
    }

    /// Controllers negotiate the lease-less recovery contract by running this
    /// subcommand's `--help` on the remote and matching *bare* long options:
    /// `homeboy_lab_runner::connection::declared_long_options` only accepts an
    /// option whose trailing tokens are value placeholders. Adding a doc comment
    /// to `--confirm-no-daemon-owner` makes clap render help text after the flag
    /// name, silently removing it from the advertised contract and making every
    /// controller refuse remote lease-less recovery. This reproduces that
    /// predicate against the real rendered help so the coupling cannot rot.
    #[test]
    fn leaseless_recovery_help_advertises_a_bare_confirmation_option() {
        fn is_option_value_placeholder(token: &str) -> bool {
            (token.starts_with('<') && token.ends_with('>'))
                || (token.starts_with("[<") && token.ends_with(">]"))
        }

        let mut command = Cli::command();
        let daemon = command
            .find_subcommand_mut("daemon")
            .expect("daemon subcommand");
        let leaseless = daemon
            .find_subcommand_mut("reconcile-leaseless-orphans")
            .expect("reconcile-leaseless-orphans subcommand");
        let help = leaseless.render_help().to_string();

        let mut declared = Vec::new();
        let mut in_options = false;
        for line in help.lines() {
            let trimmed = line.trim();
            if line == trimmed && trimmed.ends_with(':') {
                in_options = trimmed.eq_ignore_ascii_case("options:");
                continue;
            }
            if !in_options || line == trimmed {
                continue;
            }
            let mut tokens = trimmed.split_whitespace();
            let Some(option) = tokens.next() else {
                continue;
            };
            let option = option.trim_end_matches(',');
            if option.starts_with("--") && tokens.all(is_option_value_placeholder) {
                declared.push(option.to_string());
            }
        }

        assert!(
            declared.iter().any(|option| option == "--confirm-no-daemon-owner"),
            "--confirm-no-daemon-owner must render as a bare long option or controllers refuse remote lease-less recovery; declared={declared:?} help={help}"
        );
    }

    /// #11105: five recovery subcommands existed and nothing chose between
    /// them. The dispatcher parses with no arguments at all — every value it
    /// needs comes from the status report it reads.
    #[test]
    fn the_recovery_dispatcher_needs_no_transcribed_arguments() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover"]).is_ok());
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover", "--dry-run"]).is_ok());
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover", "--yes"]).is_ok());
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "recover", "--yes", "--dry-run"]).is_err(),
            "a dry run and an execution are not the same request"
        );
    }

    /// Dry run is the default. Recovery mutates the daemon that owns the
    /// caller's durable jobs, so it may not happen because someone ran the
    /// obvious command to find out what it would do.
    #[test]
    fn recovery_is_a_dry_run_unless_execution_is_asked_for() {
        with_isolated_home(|_| {
            let cli = Cli::try_parse_from(["homeboy", "daemon", "recover"])
                .expect("the dispatcher parses bare");
            let Commands::Daemon(args) = cli.command else {
                panic!("expected daemon command");
            };

            let (output, exit_code) = run(args).expect("an isolated home still resolves a plan");

            assert_eq!(exit_code, 0);
            let DaemonOutput::Recover(output) = output else {
                panic!("expected recovery output");
            };
            assert!(!output.executed, "a bare recover must not mutate anything");
            assert_eq!(output.command, "daemon.recover");
        });
    }

    /// The one confirmation on this surface that is not ceremony must stay
    /// operator-supplied. `core/daemon/control.rs` documents why: the daemon
    /// died before persisting any child identity, so nothing in process can
    /// observe whether the workloads are still running. A dispatcher that
    /// filled it in from a report would be fabricating evidence.
    #[test]
    fn the_dispatcher_never_synthesizes_the_workload_attestation() {
        use daemon::recovery_actions as actions;

        let job_ids = [Uuid::nil()];
        let plan = actions::DaemonRecoveryPlan {
            steps: vec![daemon::DaemonRepairStep::executable(
                actions::DAEMON_RECONCILE_DEAD_LEASE_ORPHANS,
                actions::reconcile_dead_lease_orphans("lease-dead", &job_ids),
            )],
            reason: "dead lease with PID-less durable jobs".to_string(),
            required_confirmations: vec![actions::CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string()],
            executable: true,
        };

        assert_eq!(
            unmet_confirmations(&plan, false),
            vec![actions::CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string()],
            "a plan needing the attestation must block until the operator makes it"
        );
        assert!(
            unmet_confirmations(&plan, true).is_empty(),
            "the operator must still be able to supply it"
        );
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover",
            "--yes",
            "--confirm-workload-processes-absent",
        ])
        .is_ok());
    }

    /// Every value the dispatcher fills comes from the report. A restart plan
    /// needs no attestation, so it must not be gated behind one.
    #[test]
    fn a_plan_needing_no_attestation_is_not_gated_on_one() {
        use daemon::recovery_actions as actions;

        let plan = actions::DaemonRecoveryPlan {
            steps: vec![
                daemon::DaemonRepairStep::executable(actions::DAEMON_STOP, actions::stop()),
                daemon::DaemonRepairStep::executable(actions::DAEMON_START, actions::start()),
            ],
            reason: "version mismatch".to_string(),
            required_confirmations: Vec::new(),
            executable: true,
        };

        assert!(unmet_confirmations(&plan, false).is_empty());
        assert_eq!(
            rendered_plan(&plan),
            "homeboy daemon stop && homeboy daemon start"
        );
    }

    #[test]
    fn stop_accepts_an_exact_lease_selector() {
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "stop", "--lease-id", "lease-live",]).is_ok()
        );
    }

    #[test]
    fn force_stop_requires_lease_id() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "stop", "--force"]).is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "stop",
            "--force",
            "--lease-id",
            "lease-live",
        ])
        .is_ok());
    }

    #[test]
    fn legacy_child_recovery_alias_remains_migration_only() {
        with_isolated_home(|_| {
            let cli = Cli::try_parse_from([
                "homeboy",
                "daemon",
                "adopt-orphan",
                "--lease-id",
                "lease-dead",
                "--confirm-pid-dead",
                "--recover-missing-child-identity",
            ])
            .expect("legacy alias still parses");
            let Commands::Daemon(args) = cli.command else {
                panic!("expected daemon command");
            };

            let error = run(args).expect_err("legacy alias must not mutate");
            assert!(error.message.contains("migration-only"));
            let rendered = format!("{error:?}");
            for field in [
                "recover-missing-child-identity",
                "expected-lease",
                "recorded-daemon-pid",
            ] {
                assert!(
                    rendered.contains(field),
                    "missing remediation field {field}"
                );
            }
        });
    }

    #[test]
    fn pidless_confirmation_is_not_a_migration_alias() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "daemon",
            "adopt-orphan",
            "--lease-id",
            "lease-dead",
            "--confirm-untracked-child-dead",
            "00000000-0000-0000-0000-000000000001",
        ])
        .expect("one released alias still parses");
        let Commands::Daemon(args) = cli.command else {
            panic!("expected daemon command");
        };

        let error = run(args).expect_err("absent daemon lease fails before recovery");
        assert!(!error.message.contains("migration-only"));
    }

    #[test]
    fn leaseless_recovery_subcommand_reaches_address_validation() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "daemon",
            "reconcile-leaseless-orphans",
            "--confirm-no-daemon-owner",
            "--addr",
            "not-an-address",
        ])
        .expect("the recovery subcommand and its required confirmation should parse");
        let Commands::Daemon(args) = cli.command else {
            panic!("expected daemon command");
        };

        let error = run(args).expect_err("invalid daemon address should reach handler validation");

        assert!(error.message.contains("Invalid daemon bind address"));
    }

    #[test]
    fn artifact_get_downloads_daemon_byte_alias() {
        with_isolated_home(|home| {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let addr = listener.local_addr().expect("addr");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0; 1024];
                let bytes = stream.read(&mut request).expect("request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                assert!(request.starts_with(
                    "GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"
                ));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                    )
                    .expect("response");
            });
            let output = home.path().join("summary.json");

            let (result, exit_code) = artifact_get(DaemonArtifactGetArgs {
                run_id: "run-1".to_string(),
                artifact_id: "report/summary.json".to_string(),
                output: Some(output.clone()),
                daemon_url: Some(format!("http://{addr}")),
            })
            .expect("artifact get");

            server.join().expect("server");
            assert_eq!(exit_code, 0);
            let DaemonOutput::ArtifactGet(result) = result else {
                panic!("expected artifact get output");
            };
            assert_eq!(result.command, "daemon.artifact.get");
            assert_eq!(result.content_type.as_deref(), Some("application/json"));
            assert_eq!(result.size_bytes, Some(11));
            assert_eq!(result.sha256.as_deref(), Some("abc123"));
            assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
        });
    }
}
