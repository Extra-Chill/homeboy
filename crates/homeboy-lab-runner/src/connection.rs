use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use homeboy_core::api_jobs::{
    ActiveRunnerJobSummary, Job, JobClaimMetadata, JobEventKind, JobStatus, RemoteRunnerJobRequest,
    RemoteRunnerJobResult, RunnerJobSource,
};
use homeboy_core::daemon::{
    DaemonFreshnessReport, DaemonLeaselessRecoveryResult, DaemonStaleReasonCode,
    DaemonStateLossRecoveryResult,
};
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::http_api::RunSummary;
use homeboy_core::paths;
use homeboy_core::server::{self, Server, SshClient};

use super::broker_http;
use super::session::{
    ReverseRunnerConnectOptions, RunnerActiveJobError, RunnerActiveJobRecoveryEvidence,
    RunnerActiveJobSource, RunnerActiveJobState, RunnerChangedRuntimePath, RunnerConnectReport,
    RunnerDisconnectReport, RunnerFailureKind, RunnerLeaselessRecoveryContract,
    RunnerLeaselessRecoveryEvidence, RunnerSession, RunnerSessionRole, RunnerSessionState,
    RunnerStaleDaemonWarning, RunnerStatusReport, RunnerTunnelMode,
};
use super::{load, remote_runner_homeboy_path, Runner, RunnerKind};
use homeboy_core::broker_auth;

const REVERSE_RUNNER_HEARTBEAT_TTL: Duration = Duration::from_secs(90);
const REMOTE_LEASELESS_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_LEASELESS_RECOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
#[path = "connection_daemon.rs"]
mod connection_daemon;
use connection_daemon::{
    connect_remote_daemon, daemon_http_freshness, daemon_http_version, versions_match,
};
use connection_daemon::{daemon_http_identity, normalize_homeboy_version_owned};
use connection_daemon::{daemon_http_runtime_loaded_paths, daemon_http_runtime_stale_paths};

#[path = "connection_stop_transport_recovery.rs"]
mod stop_transport_recovery;
pub(crate) use stop_transport_recovery::{
    disconnect_with_force, disconnect_with_session, recorded_session,
};

use super::daemon_http_get::daemon_get;

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption(runner_id, None, &[], false, None, None, None)
}

/// Connect using an explicit dead-lease or missing-lease selector. A
/// lease-less store is handled by its dedicated operator-confirmed path.
pub fn connect_with_recovery(
    runner_id: &str,
    orphan_lease_id: Option<&str>,
    missing_lease_state_identity: Option<&str>,
) -> Result<(RunnerConnectReport, i32)> {
    if missing_lease_state_identity.is_some() {
        return Err(Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "missing-lease recovery requires the explicit remote recovery command",
            missing_lease_state_identity.map(str::to_string),
            None,
        ));
    }
    connect_with_orphan_adoption(runner_id, orphan_lease_id, &[], false, None, None, None)
}

/// Reconnect after the explicit lease-less recovery transaction has terminalized
/// the unowned store and started its replacement daemon.
pub fn connect_with_leaseless_orphan_reconciliation(
    runner_id: &str,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption(runner_id, None, &[], true, None, None, None)
}

/// Connect while explicitly adopting one recorded dead remote lease. This is an
/// operator recovery path; ordinary reconnects never infer orphan ownership.
pub fn connect_with_orphan_adoption(
    runner_id: &str,
    orphan_lease_id: Option<&str>,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
    reconcile_leaseless_orphans: bool,
    missing_lease_id: Option<&str>,
    recorded_pid: Option<u32>,
    recorded_endpoint: Option<&str>,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        orphan_lease_id,
        confirmed_no_pid_job_ids,
        reconcile_leaseless_orphans,
        missing_lease_id,
        recorded_pid,
        recorded_endpoint,
        None,
    )
}

/// Explicitly adopt a currently healthy remote lease. The operator supplies
/// both coordinates, which are rechecked after the tunnel health probe and
/// immediately before atomic session persistence.
pub fn connect_with_live_lease_adoption(
    runner_id: &str,
    lease_id: &str,
    pid: u32,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption_and_live_lease(
        runner_id,
        None,
        &[],
        false,
        None,
        None,
        None,
        Some((lease_id, pid)),
    )
}

fn connect_with_orphan_adoption_and_live_lease(
    runner_id: &str,
    orphan_lease_id: Option<&str>,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
    reconcile_leaseless_orphans: bool,
    missing_lease_id: Option<&str>,
    recorded_pid: Option<u32>,
    recorded_endpoint: Option<&str>,
    live_lease_expectation: Option<(&str, u32)>,
) -> Result<(RunnerConnectReport, i32)> {
    // Reconnect replaces daemon runtime state. It shares the promotion lease
    // with binary selection so a second session cannot reconnect against a
    // different configured executable halfway through the transaction.
    let promotion_lease =
        homeboy_core::runtime_promotion::acquire("runner daemon reconnect", runner_id.to_string())?;
    promotion_lease.assert_generation()?;
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&runner, "runner connect")?;

    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "only SSH runners are supported by direct runner connect in this wave".to_string(),
        ));
    };

    let ssh_probe = client.execute("true");
    if !ssh_probe.success {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            command_failure_message("SSH connectivity check failed", &ssh_probe),
        ));
    }

    let identity = remote_homeboy_identity(&client, homeboy);
    let Ok(identity) = identity else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            identity.err().unwrap(),
        ));
    };
    let version = identity.version.clone();
    // A malformed local record is not ownership evidence. It remains fail
    // closed unless an operator supplies an exact live-lease expectation.
    let previous_session = match read_session_or_live_peer(runner_id) {
        Ok(session) => session,
        Err(error) if error.code == homeboy_core::error::ErrorCode::ConfigInvalidJson => None,
        Err(error) => return Err(error),
    };

    let mut leaseless_recovery = None;
    let mut state_loss_recovery = None;
    if let Some(lease_id) = missing_lease_id {
        let recorded_pid = recorded_pid.ok_or_else(|| {
            Error::validation_invalid_argument(
                "recorded_pid",
                "state-loss recovery requires a recorded PID",
                None,
                None,
            )
        })?;
        let recorded_endpoint = recorded_endpoint.ok_or_else(|| {
            Error::validation_invalid_argument(
                "recorded_endpoint",
                "state-loss recovery requires a recorded endpoint",
                None,
                None,
            )
        })?;
        let capability = format!(
            "{} daemon recover-missing-lease-state --help",
            shell::quote_arg(homeboy),
        );
        let capability =
            client.execute_with_timeout(&capability, REMOTE_LEASELESS_RECOVERY_TIMEOUT);
        if !capability.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                "remote Homeboy does not support `daemon recover-missing-lease-state`; update the runner to a build with the canonical state-loss recovery contract before retrying".to_string(),
            ));
        }
        let command =
            remote_state_loss_recovery_command(homeboy, lease_id, recorded_pid, recorded_endpoint);
        let recovery = client.execute_with_timeout(&command, REMOTE_LEASELESS_RECOVERY_TIMEOUT);
        if !recovery.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                state_loss_recovery_failure_message(&recovery),
            ));
        }
        let envelope = parse_envelope(&recovery.stdout).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse state-loss daemon recovery".to_string()),
            )
        })?;
        if !envelope.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                "remote exact state-loss recovery returned an error envelope".to_string(),
            ));
        }
        state_loss_recovery = Some(decode_state_loss_recovery(envelope.data)?);
    }
    let mut recovery_evidence = None;
    if reconcile_leaseless_orphans {
        let recovery_addr = previous_session
            .as_ref()
            .and_then(|session| session.remote_daemon_address.as_deref())
            .filter(|address| parse_loopback_daemon_addr(address).is_ok())
            .unwrap_or("127.0.0.1:0");
        let recovery = execute_remote_leaseless_recovery(
            || {
                client.execute_with_timeout(
                    &remote_leaseless_recovery_help_command(homeboy),
                    REMOTE_LEASELESS_RECOVERY_PROBE_TIMEOUT,
                )
            },
            |contract| {
                client.execute_with_timeout(
                    &remote_leaseless_recovery_command(homeboy, recovery_addr, contract),
                    REMOTE_LEASELESS_RECOVERY_TIMEOUT,
                )
            },
        );
        let (contract, recovery) = match recovery {
            Ok(recovery) => recovery,
            Err(message) => {
                return Ok(failed_connect(
                    runner_id,
                    session_path,
                    RunnerFailureKind::DaemonStartupFailure,
                    format!("remote Homeboy {}: {message}", identity.display),
                ));
            }
        };
        if !recovery.success {
            let message = leaseless_recovery_failure_message(&recovery);
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                message,
            ));
        }
        let envelope = parse_envelope(&recovery.stdout).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse lease-less daemon recovery".to_string()),
            )
        })?;
        if !envelope.success {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                "remote lease-less daemon recovery returned an error envelope".to_string(),
            ));
        }
        let recovery = decode_leaseless_recovery(envelope.data)?;
        recovery_evidence = Some(leaseless_recovery_evidence(
            contract,
            &identity.display,
            recovery.clone(),
        ));
        leaseless_recovery = Some(recovery);
    }

    if let (Some(recovery), Some(evidence)) = (&leaseless_recovery, &recovery_evidence) {
        let replacement = &recovery.replacement;
        write_session(&RunnerSession {
            runner_id: runner.id.clone(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some(server_id.clone()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some(replacement.address.clone()),
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: Some(replacement.pid),
            remote_daemon_lease_id: Some(replacement.lease_id.clone()),
            homeboy_version: version.clone(),
            homeboy_build_identity: Some(identity.display.clone()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: serde_json::to_value(&evidence).ok(),
        })?;
    }

    let daemon = ensure_remote_daemon(
        &client,
        homeboy,
        runner_id,
        previous_session.as_ref(),
        &identity.display,
        orphan_lease_id,
        confirmed_no_pid_job_ids,
        live_lease_expectation,
    );
    let Ok(daemon) = daemon else {
        let (mut report, exit_code) = failed_connect_after_recovery(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            daemon.err().unwrap(),
            leaseless_recovery,
            recovery_evidence,
        );
        attach_state_loss_recovery(&mut report, state_loss_recovery);
        return Ok((report, exit_code));
    };

    let expected_version = daemon.version.clone().unwrap_or(version.clone());
    let expected_identity = daemon
        .build_identity
        .clone()
        .unwrap_or(identity.display.clone());
    let (local_port, tunnel_pid, local_url, daemon) = match connect_remote_daemon(
        &server,
        &client,
        homeboy,
        daemon,
        &expected_version,
        &expected_identity,
        runner_id,
        &session_path,
    ) {
        Ok(connection) => connection,
        Err((mut report, exit_code)) => {
            attach_state_loss_recovery(&mut report, state_loss_recovery);
            attach_leaseless_recovery(&mut report, leaseless_recovery, recovery_evidence);
            return Ok((report, exit_code));
        }
    };

    let remote_daemon_lease_id = daemon.lease_id.clone();
    if let Err(error) = verify_live_lease_adoption(
        &client,
        homeboy,
        live_lease_expectation,
        &daemon,
        &identity.display,
    ) {
        return Ok(session_write_failure_report(
            runner_id,
            session_path,
            error,
            state_loss_recovery,
            tunnel_pid,
            session_store::terminate_pid,
        ));
    }
    let connection_warning = daemon
        .inspected_freshness
        .as_ref()
        .and_then(|report| stale_reattach_warning_for_report(&runner.id, report));
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: Some(controller_id()),
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id,
        homeboy_version: version,
        homeboy_build_identity: Some(identity.display),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: recovery_evidence
            .as_ref()
            .and_then(|e| serde_json::to_value(e).ok()),
    };
    // The tunnel health check above re-proved the exact remote lease and PID.
    // Refuse a runtime generation change before atomically publishing it.
    if let Err(error) = promotion_lease
        .assert_generation()
        .and_then(|_| write_session(&session))
    {
        return Ok(session_write_failure_report(
            runner_id,
            session_path,
            error,
            state_loss_recovery,
            session.tunnel_pid,
            session_store::terminate_pid,
        ));
    }

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(session.mode.clone()),
            role: Some(session.role.clone()),
            connected: true,
            recorded: None,
            local_url: session.local_url.clone(),
            broker_url: None,
            controller_id: None,
            remote_daemon_address: session.remote_daemon_address.clone(),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            connection_warning,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            leaseless_recovery,
            state_loss_recovery,
            leaseless_recovery_evidence: recovery_evidence,
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

fn verify_live_lease_adoption(
    client: &SshClient,
    homeboy: &str,
    expectation: Option<(&str, u32)>,
    connected_daemon: &RemoteDaemon,
    expected_identity: &str,
) -> std::result::Result<(), Error> {
    let Some((expected_lease, expected_pid)) = expectation else {
        return Ok(());
    };
    let mut status = remote_daemon_status(client, homeboy).map_err(Error::internal_unexpected)?;
    probe_remote_daemon_endpoint(client, &mut status);
    let daemon = status.daemon.ok_or_else(|| {
        Error::validation_invalid_argument(
            "adopt_live_lease",
            "live lease changed before adoption could be persisted",
            None,
            None,
        )
    })?;
    if !status.fresh
        || !status.reachable
        || status.endpoint_probe_error.is_some()
        || daemon.lease_id.as_deref() != Some(expected_lease)
        || daemon.pid != Some(expected_pid)
        || daemon.lease_id != connected_daemon.lease_id
        || daemon.pid != connected_daemon.pid
        || daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim())
    {
        return Err(Error::validation_invalid_argument(
            "adopt_live_lease",
            format!(
                "explicit live lease adoption snapshot no longer matches: expected lease `{expected_lease}` PID {expected_pid} build `{expected_identity}`; current lease `{}` PID `{}` build `{}`; no session state was changed",
                daemon.lease_id.as_deref().unwrap_or("unavailable"),
                daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
                daemon.build_identity.as_deref().unwrap_or("unavailable"),
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn remote_leaseless_recovery_help_command(homeboy: &str) -> String {
    format!(
        "{} daemon reconcile-leaseless-orphans --help",
        shell::quote_arg(homeboy),
    )
}

fn remote_leaseless_recovery_command(
    homeboy: &str,
    addr: &str,
    contract: RunnerLeaselessRecoveryContract,
) -> String {
    let confirmations = match contract {
        RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner
        | RunnerLeaselessRecoveryContract::ReconcileLeaselessOrphansAndConfirmNoDaemonOwner
        | RunnerLeaselessRecoveryContract::ConfirmControlPlaneLost => "--confirm-no-daemon-owner",
    };
    format!(
        "{} daemon reconcile-leaseless-orphans {confirmations} --addr {}",
        shell::quote_arg(homeboy),
        shell::quote_arg(addr),
    )
}

fn execute_remote_leaseless_recovery<Probe, Recover>(
    probe: Probe,
    recover: Recover,
) -> std::result::Result<
    (
        RunnerLeaselessRecoveryContract,
        homeboy_core::server::CommandOutput,
    ),
    String,
>
where
    Probe: FnOnce() -> homeboy_core::server::CommandOutput,
    Recover: FnOnce(RunnerLeaselessRecoveryContract) -> homeboy_core::server::CommandOutput,
{
    let probe = probe();
    let contract = negotiate_leaseless_recovery_contract(&probe)?;
    Ok((contract.clone(), recover(contract)))
}

fn negotiate_leaseless_recovery_contract(
    output: &homeboy_core::server::CommandOutput,
) -> std::result::Result<RunnerLeaselessRecoveryContract, String> {
    if !output.success {
        if output.timed_out {
            return Err(format!(
                "lease-less recovery capability probe timed out after {}s; refusing recovery before contract negotiation",
                REMOTE_LEASELESS_RECOVERY_PROBE_TIMEOUT.as_secs()
            ));
        }
        return Err(command_failure_message(
            "lease-less recovery capability probe failed; refusing recovery before contract negotiation",
            output,
        ));
    }

    let options = declared_long_options(&output.stdout);
    let confirm_no_daemon_owner = options.contains("--confirm-no-daemon-owner");
    if confirm_no_daemon_owner
        && !options.contains("--reconcile-leaseless-orphans")
        && !options.contains("--confirm-control-plane-lost")
    {
        Ok(RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner)
    } else {
        Err(
            "lease-less recovery capability probe did not advertise the canonical --confirm-no-daemon-owner contract; update the runner before retrying"
                .to_string(),
        )
    }
}

fn declared_long_options(help: &str) -> std::collections::BTreeSet<&str> {
    let mut options = std::collections::BTreeSet::new();
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
            options.insert(option);
        }
    }
    options
}

fn is_option_value_placeholder(token: &str) -> bool {
    (token.starts_with('<') && token.ends_with('>'))
        || (token.starts_with("[<") && token.ends_with(">]"))
}

fn leaseless_recovery_evidence(
    contract: RunnerLeaselessRecoveryContract,
    remote_command_identity: &str,
    recovery: DaemonLeaselessRecoveryResult,
) -> RunnerLeaselessRecoveryEvidence {
    RunnerLeaselessRecoveryEvidence {
        contract,
        remote_command_identity: remote_command_identity.to_string(),
        recovery: Some(recovery),
    }
}

fn decode_leaseless_recovery(data: Option<Value>) -> Result<DaemonLeaselessRecoveryResult> {
    serde_json::from_value(data.ok_or_else(|| {
        Error::internal_unexpected("remote lease-less daemon recovery returned no data")
    })?)
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("decode lease-less daemon recovery".to_string()),
        )
    })
}

fn decode_state_loss_recovery(data: Option<Value>) -> Result<DaemonStateLossRecoveryResult> {
    serde_json::from_value(data.ok_or_else(|| {
        Error::internal_unexpected("remote exact state-loss recovery returned no data")
    })?)
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("decode exact state-loss daemon recovery".to_string()),
        )
    })
}

fn attach_state_loss_recovery(
    report: &mut RunnerConnectReport,
    recovery: Option<DaemonStateLossRecoveryResult>,
) {
    report.state_loss_recovery = recovery;
}

fn session_write_failure_report<Terminate>(
    runner_id: &str,
    session_path: PathBuf,
    error: Error,
    recovery: Option<DaemonStateLossRecoveryResult>,
    tunnel_pid: Option<u32>,
    terminate: Terminate,
) -> (RunnerConnectReport, i32)
where
    Terminate: FnOnce(u32),
{
    if let Some(pid) = tunnel_pid {
        terminate(pid);
    }
    let (mut report, exit_code) = failed_connect(
        runner_id,
        session_path,
        RunnerFailureKind::DaemonStartupFailure,
        format!(
            "persist runner session after daemon recovery: {}",
            error.message
        ),
    );
    attach_state_loss_recovery(&mut report, recovery);
    (report, exit_code)
}

fn leaseless_recovery_failure_message(output: &homeboy_core::server::CommandOutput) -> String {
    if output.timed_out {
        format!(
            "remote lease-less daemon recovery timed out after {}s; inspect `homeboy daemon status` on the runner before retrying",
            REMOTE_LEASELESS_RECOVERY_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message("remote lease-less daemon recovery failed", output)
    }
}

fn state_loss_recovery_failure_message(output: &homeboy_core::server::CommandOutput) -> String {
    if output.timed_out {
        format!(
            "remote exact state-loss recovery timed out after {}s; inspect `homeboy daemon status` on the runner before retrying",
            REMOTE_LEASELESS_RECOVERY_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message("remote exact state-loss recovery failed", output)
    }
}

fn remote_state_loss_recovery_command(
    homeboy: &str,
    lease_id: &str,
    recorded_pid: u32,
    recorded_endpoint: &str,
) -> String {
    format!(
        "{} daemon recover-missing-lease-state --lease-id {} --recorded-pid {} --recorded-endpoint {} --confirm-pid-dead --confirm-control-plane-lost --addr 127.0.0.1:0",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
        recorded_pid,
        shell::quote_arg(recorded_endpoint),
    )
}

pub fn connect_reverse(options: ReverseRunnerConnectOptions) -> Result<(RunnerConnectReport, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Reverse runner connect requires --reverse-runner <runner-id>",
            None,
            None,
        ));
    }
    if options.controller_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller",
            "Reverse runner connect requires a controller or broker ID",
            None,
            None,
        ));
    }

    let runner = load(&options.runner_id)?;
    let session_path = session_path(&runner.id)?;
    let homeboy_identity = homeboy_product_identity::build_identity();
    let homeboy_version = homeboy_identity.version.clone();
    let now = Utc::now().to_rfc3339();
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::Reverse,
        role: RunnerSessionRole::Runner,
        server_id: runner.server_id.clone(),
        controller_id: Some(options.controller_id.clone()),
        broker_url: options.broker_url.clone(),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        remote_daemon_lease_id: None,
        homeboy_version,
        homeboy_build_identity: Some(homeboy_identity.display),
        connected_at: now.clone(),
        worker_identity: Some(format!("{}@{}", std::process::id(), hostname_fallback())),
        worker_pid: Some(std::process::id()),
        last_seen_at: Some(now),
        leaseless_recovery_evidence: None,
    };
    write_session(&session)?;
    let broker_registered = match session.broker_url.as_deref() {
        Some(broker_url) => register_reverse_session_with_broker(broker_url, &session)?,
        None => false,
    };

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(RunnerTunnelMode::Reverse),
            role: Some(RunnerSessionRole::Runner),
            connected: broker_registered,
            recorded: Some(true),
            local_url: None,
            broker_url: options.broker_url,
            controller_id: Some(options.controller_id),
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            connection_warning: None,
            homeboy_version: Some(session.homeboy_version),
            homeboy_build_identity: session.homeboy_build_identity,
            session_path: Some(session_path.display().to_string()),
            leaseless_recovery: None,
            state_loss_recovery: None,
            leaseless_recovery_evidence: None,
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

fn register_reverse_session_with_broker(broker_url: &str, session: &RunnerSession) -> Result<bool> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let response = client
        .post(format!(
            "{}/runner/sessions",
            broker_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "runner_id": session.runner_id,
            "controller_id": session.controller_id,
            "broker_url": session.broker_url,
            "homeboy_version": session.homeboy_version,
            "homeboy_build_identity": session.homeboy_build_identity,
            "worker_identity": session.worker_identity,
            "worker_pid": session.worker_pid,
            "last_seen_at": session.last_seen_at,
        }))
        .send()
        .map_err(|err| {
            Error::internal_unexpected(format!("register reverse runner session: {err}"))
        })?;
    let status_code = response.status().as_u16();
    let envelope: CliEnvelope = response.json().map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner session registration response".to_string()),
        )
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "reverse runner session registration failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    Ok(true)
}

pub fn status(runner_id: &str) -> Result<RunnerStatusReport> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    // A Cook handoff can cross controller identities after readiness accepted a
    // direct SSH tunnel. Reuse that still-live tunnel rather than treating the
    // controller-local record lookup as a daemon disconnect.
    let session = read_session_or_live_peer(runner_id)?;
    let state = session_state(session.as_ref());
    let connected = state == RunnerSessionState::Connected;
    let stale_daemon = stale_daemon_warning(&runner, session.as_ref(), connected)?;
    let mut daemon_freshness = runner_daemon_freshness(&runner, session.as_ref(), connected)?
        .or_else(|| remote_daemon_recovery_freshness(runner_id, &runner));
    let active_job_source = session.as_ref().and_then(active_runner_job_source);
    let direct_daemon_active_jobs =
        matches!(active_job_source, Some(RunnerActiveJobSource::DirectDaemon))
            .then(|| {
                daemon_freshness
                    .as_ref()
                    .map(|freshness| freshness.active_jobs)
            })
            .flatten();
    let (active_jobs, stale_jobs, active_job_state, active_job_error) = if connected {
        match session.as_ref() {
            Some(session) => match runner_jobs(runner_id, session) {
                Ok((active_jobs, mut stale_jobs)) => {
                    // `/jobs` has a typed remote-runner subset while daemon
                    // freshness counts every live child. Never manufacture an
                    // orphan when that subset is incomplete.
                    if should_infer_child_run_orphans(active_jobs.len(), direct_daemon_active_jobs)
                    {
                        stale_jobs.extend(orphaned_child_run_jobs(
                            runner_id,
                            session,
                            &active_jobs,
                        ));
                    }
                    (
                        active_jobs,
                        stale_jobs,
                        RunnerActiveJobState::Available,
                        None,
                    )
                }
                Err(err) => (
                    Vec::new(),
                    Vec::new(),
                    RunnerActiveJobState::Unavailable,
                    Some(RunnerActiveJobError {
                        code: err.code.as_str().to_string(),
                        message: err.message,
                    }),
                ),
            },
            None => (
                Vec::new(),
                Vec::new(),
                RunnerActiveJobState::NotQueried,
                None,
            ),
        }
    } else {
        (
            Vec::new(),
            Vec::new(),
            RunnerActiveJobState::NotQueried,
            None,
        )
    };
    let (active_jobs, stale_jobs, direct_daemon_active_jobs, active_job_recovery_evidence) =
        reconcile_terminal_phantom_activity(
            runner_id,
            session.as_ref(),
            active_jobs,
            stale_jobs,
            direct_daemon_active_jobs,
        );
    if let (Some(freshness), Some(active_jobs)) =
        (daemon_freshness.as_mut(), direct_daemon_active_jobs)
    {
        freshness.active_jobs = active_jobs;
    }
    let active_job_count = direct_daemon_active_jobs.unwrap_or(active_jobs.len());
    let active_job_error = match (active_job_error, direct_daemon_active_jobs) {
        (Some(error), _) => Some(error),
        (None, Some(authoritative_count)) if authoritative_count != active_jobs.len() => {
            Some(RunnerActiveJobError {
                code: "active_job_view_inconsistent".to_string(),
                message: format!(
                    "direct daemon freshness reports {authoritative_count} active job(s), but /jobs exposed {} typed runner job(s); freshness is authoritative and terminal-only reconciliation found no durable terminal handoffs",
                    active_jobs.len()
                ),
            })
        }
        (None, _) => None,
    };
    let stale_runner_job_count = stale_jobs.len();
    let active_runner_jobs = active_jobs.iter().map(Into::into).collect();
    let stale_runner_jobs = stale_jobs.iter().map(Into::into).collect();
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        state,
        session,
        stale_daemon,
        daemon_freshness,
        active_jobs,
        active_runner_jobs,
        stale_runner_jobs,
        active_job_count,
        stale_runner_job_count,
        active_job_state,
        active_job_source,
        active_job_error,
        active_job_recovery_evidence,
        session_path: session_path.display().to_string(),
    })
}

/// Resolve a direct-SSH session for work admission. A readiness observation can
/// become stale when its controller-owned tunnel exits before submission; the
/// reconnect transaction proves the remote daemon lease before replacing the
/// local tunnel record.
pub(crate) fn status_for_admission(runner_id: &str) -> Result<RunnerStatusReport> {
    status_for_admission_with(runner_id, status, |runner_id| {
        let (report, exit_code) = connect(runner_id)?;
        if report.connected && exit_code == 0 {
            return Ok(());
        }

        Err(Error::validation_invalid_argument(
            "runner",
            report
                .failure_message
                .unwrap_or_else(|| "runner reconnect did not become ready".to_string()),
            Some(runner_id.to_string()),
            None,
        ))
    })
}

fn status_for_admission_with<Status, Reconnect>(
    runner_id: &str,
    mut status_fn: Status,
    mut reconnect: Reconnect,
) -> Result<RunnerStatusReport>
where
    Status: FnMut(&str) -> Result<RunnerStatusReport>,
    Reconnect: FnMut(&str) -> Result<()>,
{
    let status = status_fn(runner_id)?;
    if status.connected
        || status
            .session
            .as_ref()
            .is_none_or(|session| session.mode != RunnerTunnelMode::DirectSsh)
    {
        return Ok(status);
    }

    reconnect(runner_id)?;
    status_fn(runner_id)
}

fn reconcile_terminal_phantom_activity(
    runner_id: &str,
    session: Option<&RunnerSession>,
    active_jobs: Vec<ActiveRunnerJobSummary>,
    stale_jobs: Vec<ActiveRunnerJobSummary>,
    direct_daemon_active_jobs: Option<usize>,
) -> (
    Vec<ActiveRunnerJobSummary>,
    Vec<ActiveRunnerJobSummary>,
    Option<usize>,
    Option<RunnerActiveJobRecoveryEvidence>,
) {
    let Some(authoritative_count) = direct_daemon_active_jobs else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    if authoritative_count == active_jobs.len() {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    }
    let Some(local_url) = session.and_then(|session| session.local_url.as_deref()) else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    let Ok(client) = Client::builder().timeout(Duration::from_secs(10)).build() else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    let Ok(response) = client
        .post(format!(
            "{}/jobs/reconcile-terminal",
            local_url.trim_end_matches('/')
        ))
        .send()
    else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    if !response.status().is_success() {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    }
    let Ok(body) = response.json::<Value>() else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    let reconciled = body
        .pointer("/body/reconciled_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let reconciled_job_ids = body
        .pointer("/body/reconciled_job_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .take(100)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if reconciled == 0
        || reconciled_job_ids.is_empty()
        || usize::try_from(reconciled).ok() != Some(reconciled_job_ids.len())
    {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    }
    let Ok((refreshed_active_jobs, refreshed_stale_jobs)) =
        runner_jobs(runner_id, session.expect("local URL requires session"))
    else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    let Some(refreshed_count) = daemon_http_freshness(
        local_url,
        &session.expect("local URL requires session").homeboy_version,
        session
            .expect("local URL requires session")
            .homeboy_build_identity
            .as_deref()
            .unwrap_or(""),
    )
    .ok()
    .map(|freshness| freshness.active_jobs) else {
        return (active_jobs, stale_jobs, direct_daemon_active_jobs, None);
    };
    (
        refreshed_active_jobs,
        refreshed_stale_jobs,
        Some(refreshed_count),
        Some(RunnerActiveJobRecoveryEvidence {
            reconciled_job_ids,
            prior_active_job_count: authoritative_count,
            active_job_count: refreshed_count,
        }),
    )
}

fn should_infer_child_run_orphans(
    typed_active_jobs: usize,
    direct_daemon_active_jobs: Option<usize>,
) -> bool {
    direct_daemon_active_jobs.is_none_or(|count| typed_active_jobs >= count)
}

/// Query the daemon job store before an operation replaces its process.
/// Controller observation may classify child runs as recoverable orphans, but
/// those inferred records are not authoritative enough to interrupt a daemon.
pub(super) fn active_jobs_before_daemon_replacement(
    runner_id: &str,
) -> Result<Vec<ActiveRunnerJobSummary>> {
    let report = status(runner_id)?;
    if !report.connected {
        return Ok(Vec::new());
    }
    if authoritative_zero_active_jobs(&report) {
        return Ok(Vec::new());
    }
    if report.active_job_state != RunnerActiveJobState::Available {
        let mut error = Error::validation_invalid_argument(
            "reconnect",
            format!(
                "runner `{runner_id}` is connected but its active daemon jobs could not be listed; refusing to replace the daemon"
            ),
            Some(runner_id.to_string()),
            Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
        );
        error.details["active_job_state"] = serde_json::json!(report.active_job_state);
        return Err(error);
    }
    if let Some(error) = &report.active_job_error {
        return Err(Error::validation_invalid_argument(
            "reconnect",
            format!(
                "runner `{runner_id}` has an inconsistent active daemon job view ({}); refusing to replace the daemon",
                error.message
            ),
            Some(runner_id.to_string()),
            Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
        ));
    }
    Ok(report.active_jobs)
}

/// A daemon freshness report is the daemon's own job count. Only a restartable
/// daemon which reports zero jobs can replace an unavailable typed `/jobs` view.
pub(crate) fn authoritative_zero_active_jobs(report: &RunnerStatusReport) -> bool {
    report
        .daemon_freshness
        .as_ref()
        .is_some_and(|freshness| freshness.restartable && freshness.active_jobs == 0)
}

fn runner_daemon_freshness(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Result<Option<homeboy_core::daemon::DaemonFreshnessReport>> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let Some(session) = session else {
        return Ok(None);
    };
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(None);
    };
    Ok(daemon_http_freshness(
        local_url,
        &session.homeboy_version,
        session.homeboy_build_identity.as_deref().unwrap_or(""),
    )
    .ok())
}

/// Preserve remote lease evidence when the controller-side daemon HTTP tunnel is unavailable.
fn remote_daemon_recovery_freshness(
    runner_id: &str,
    runner: &Runner,
) -> Option<DaemonFreshnessReport> {
    if runner.kind != RunnerKind::Ssh {
        return None;
    }
    let homeboy = match remote_runner_homeboy_path(runner, "runner status") {
        Ok(homeboy) => homeboy,
        Err(error) => return Some(unavailable_recovery_freshness(error.message)),
    };
    let (_, _, client) = match resolve_ssh_runner(runner) {
        Ok(Some(connection)) => connection,
        Ok(None) => return None,
        Err(error) => return Some(unavailable_recovery_freshness(error.message)),
    };
    match remote_daemon_status(&client, homeboy) {
        Ok(mut status) => {
            remote_daemon::probe_remote_daemon_endpoint(&client, &mut status);
            Some(remote_daemon_recovery_freshness_from_status(
                runner_id, &status,
            ))
        }
        Err(error) => Some(unavailable_recovery_freshness(error)),
    }
}

fn active_runner_job_source(session: &RunnerSession) -> Option<RunnerActiveJobSource> {
    if session.local_url.is_some() {
        Some(RunnerActiveJobSource::DirectDaemon)
    } else if session.mode == RunnerTunnelMode::Reverse && session.broker_url.is_some() {
        Some(RunnerActiveJobSource::ReverseBroker)
    } else {
        None
    }
}

fn runner_jobs(
    runner_id: &str,
    session: &RunnerSession,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build active job client: {err}")))?;
    let (body, source) = if let Some(local_url) = session.local_url.as_deref() {
        let data = daemon_get(&client, local_url, "/jobs")?;
        (
            data.get("body").cloned().ok_or_else(|| {
                Error::internal_unexpected("daemon jobs response missing data.body")
            })?,
            RunnerJobSource::Daemon,
        )
    } else if session.mode == RunnerTunnelMode::Reverse {
        let Some(broker_url) = session.broker_url.as_deref() else {
            return Err(Error::internal_unexpected(format!(
                "reverse runner `{runner_id}` is connected but has no broker URL for active-job status"
            )));
        };
        (
            broker_http::get_json(
                &client,
                broker_url,
                "/jobs",
                "list reverse runner broker jobs",
                None,
            )?,
            RunnerJobSource::Broker,
        )
    } else {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner_id}` is connected but has no active-job status endpoint"
        )));
    };
    let active_jobs = parse_runner_jobs(
        &body,
        "active_runner_jobs",
        "parse active runner jobs",
        runner_id,
        source,
    )?;
    let stale_jobs = parse_runner_jobs(
        &body,
        "stale_runner_jobs",
        "parse stale runner jobs",
        runner_id,
        source,
    )?;
    Ok((active_jobs, stale_jobs))
}

fn orphaned_child_run_jobs(
    runner_id: &str,
    session: &RunnerSession,
    active_jobs: &[ActiveRunnerJobSummary],
) -> Vec<ActiveRunnerJobSummary> {
    let Ok(runs) = runner_running_runs(session) else {
        return Vec::new();
    };
    runs.into_iter()
        .filter(|run| !child_run_has_active_job(run, active_jobs))
        .map(|run| orphaned_child_run_job(runner_id, run))
        .collect()
}

fn child_run_has_active_job(run: &RunSummary, active_jobs: &[ActiveRunnerJobSummary]) -> bool {
    active_jobs.iter().any(|job| {
        job.durable_run_id.as_deref() == Some(run.id.as_str())
            || job.command.contains(run.id.as_str())
    })
}

fn runner_running_runs(session: &RunnerSession) -> Result<Vec<RunSummary>> {
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(Vec::new());
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build runner runs client: {err}")))?;
    let data = daemon_get(&client, local_url, "/runs?status=running&limit=1000")?;
    let runs: Vec<RunSummary> =
        serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse runner daemon running runs".to_string()),
            )
        })?;
    Ok(runs
        .into_iter()
        .filter(|run| !is_synthetic_active_job_run_summary(run))
        .collect())
}

fn is_synthetic_active_job_run_summary(run: &RunSummary) -> bool {
    run.status_note
        .as_deref()
        .is_some_and(|note| note.starts_with("active runner job:"))
}

fn orphaned_child_run_job(runner_id: &str, run: RunSummary) -> ActiveRunnerJobSummary {
    ActiveRunnerJobSummary {
        runner_id: runner_id.to_string(),
        job_id: format!("orphaned-child-run-{}", run.id),
        operation: "child-run".to_string(),
        source: "runner-observation".to_string(),
        kind: run.kind,
        status: JobStatus::Failed,
        command: run
            .command
            .unwrap_or_else(|| format!("homeboy runs show {}", run.id)),
        cwd: run.cwd,
        started_at_ms: rfc3339_to_ms(&run.started_at).unwrap_or(0),
        updated_at_ms: 0,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: JobClaimMetadata {
            claim_id: None,
            claimed_by_runner_id: Some(runner_id.to_string()),
            claimed_at_ms: None,
            claim_expires_at_ms: None,
        },
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: Some(run.id),
        stale_reason: Some("child_run_running_without_active_runner_job".to_string()),
        lifecycle_state: Some("recoverable_orphan".to_string()),
        retryable: Some(true),
        active_child_count: None,
        active_cell_count: None,
    }
}

fn rfc3339_to_ms(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
}

/// Parse a runner-jobs array (`key`) out of `body`, keep only jobs for
/// `runner_id`, and tag each with its originating `source`.
fn parse_runner_jobs(
    body: &Value,
    key: &str,
    error_context: &str,
    runner_id: &str,
    source: RunnerJobSource,
) -> Result<Vec<ActiveRunnerJobSummary>> {
    let jobs: Vec<ActiveRunnerJobSummary> = serde_json::from_value(
        body.get(key)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|err| Error::internal_json(err.to_string(), Some(error_context.to_string())))?;
    Ok(jobs
        .into_iter()
        .filter(|job| job.runner_id == runner_id)
        .map(|mut job| {
            job.source = source.to_string();
            job
        })
        .collect())
}

pub fn reverse_broker_reconcile(runner_id: &str) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let client = broker_client("build broker reconcile client")?;
    broker_http::post_json(
        &client,
        &broker_url,
        "/runner/jobs/reconcile",
        Value::Null,
        "reconcile reverse runner broker jobs",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
}

/// Submit a redacted, replayable request to a connected reverse broker. Secret
/// values are intentionally absent: the worker resolves named references when
/// it prepares the claimed process.
pub fn submit_reverse_broker_job(runner_id: &str, request: RemoteRunnerJobRequest) -> Result<Job> {
    if request.runner_id != runner_id {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "reverse broker submission runner does not match request runner",
            Some(runner_id.to_string()),
            None,
        ));
    }
    let broker_url = reverse_broker_url(runner_id)?;
    let client = broker_client("build reverse broker submission client")?;
    let body = serde_json::to_value(&request).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize replayable reverse runner job".to_string()),
        )
    })?;
    let response = broker_http::post_json(
        &client,
        &broker_url,
        "/runner/jobs",
        body,
        "replay reverse runner job submission",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )?;
    serde_json::from_value(
        response.get("job").cloned().ok_or_else(|| {
            Error::internal_unexpected("reverse broker submission returned no job")
        })?,
    )
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse replayed reverse runner job".to_string()),
        )
    })
}

pub fn reverse_broker_artifact(runner_id: &str, job_id: &str, artifact_id: &str) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let artifact_id = homeboy_core::execution_contract::encode_uri_component(artifact_id);
    let client = broker_client("build broker artifact client")?;
    broker_http::get_json(
        &client,
        &broker_url,
        &format!("/runner/jobs/{job_id}/artifacts/{artifact_id}"),
        "lookup reverse runner broker artifact",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
}

/// Fetch bytes that a reverse runner mirrored into its terminal job result.
/// Callers must still validate the returned content against durable metadata.
pub fn reverse_broker_artifact_content(
    runner_id: &str,
    job_id: &str,
    artifact_id: &str,
) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let artifact_id = homeboy_core::execution_contract::encode_uri_component(artifact_id);
    let client = broker_client("build broker artifact content client")?;
    broker_http::get_json(
        &client,
        &broker_url,
        &format!("/runner/jobs/{job_id}/artifacts/{artifact_id}/content"),
        "fetch reverse runner broker artifact content",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
}

/// Retrieve terminal job artifact bytes through the runner's active managed
/// transport. Direct sessions read the daemon's persisted job result; reverse
/// sessions use the broker content endpoint.
pub fn runner_artifact_content(runner_id: &str, job_id: &str, artifact_id: &str) -> Result<Value> {
    let report = status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not connected"),
            Some(runner_id.to_string()),
            None,
        ));
    };
    match artifact_content_transport(&session)? {
        RunnerArtifactContentTransport::DirectDaemon => {
            direct_daemon_job_artifact_content(runner_id, job_id, artifact_id)
        }
        RunnerArtifactContentTransport::ReverseBroker => {
            reverse_broker_artifact_content(runner_id, job_id, artifact_id)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerArtifactContentTransport {
    DirectDaemon,
    ReverseBroker,
}

fn artifact_content_transport(session: &RunnerSession) -> Result<RunnerArtifactContentTransport> {
    match session.mode {
        RunnerTunnelMode::DirectSsh if session.local_url.is_some() => {
            Ok(RunnerArtifactContentTransport::DirectDaemon)
        }
        RunnerTunnelMode::Reverse if session.broker_url.is_some() => {
            Ok(RunnerArtifactContentTransport::ReverseBroker)
        }
        RunnerTunnelMode::DirectSsh => Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "direct runner `{}` has no managed daemon endpoint for artifact retrieval",
                session.runner_id
            ),
            Some(session.runner_id.clone()),
            Some(vec![
                "Reconnect the direct runner so its local daemon URL is available, then retry the review or promotion."
                    .to_string(),
            ]),
        )),
        RunnerTunnelMode::Reverse => Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "reverse runner `{}` has no broker endpoint for artifact retrieval",
                session.runner_id
            ),
            Some(session.runner_id.clone()),
            Some(vec![
                "Reconnect the reverse runner with its broker URL, then retry the review or promotion."
                    .to_string(),
            ]),
        )),
    }
}

fn direct_daemon_job_artifact_content(
    runner_id: &str,
    job_id: &str,
    artifact_id: &str,
) -> Result<Value> {
    let snapshot = super::runner_job_log_snapshot(runner_id, job_id)?;
    let result = snapshot
        .events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.clone())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("runner job `{job_id}` has no terminal result containing artifact bytes"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let result: RemoteRunnerJobResult = serde_json::from_value(result).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse direct runner terminal job result for artifact retrieval".to_string()),
        )
    })?;
    let artifact = result
        .artifacts
        .iter()
        .chain(result.artifact_refs.iter())
        .find(|artifact| artifact.id == artifact_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!("direct runner job `{job_id}` has no artifact `{artifact_id}`"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let content_base64 = artifact.content_base64.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "direct runner job `{job_id}` did not retain bytes for artifact `{artifact_id}`"
            ),
            Some(artifact_id.to_string()),
            Some(vec![
                "Only artifacts mirrored in the direct runner terminal result can be materialized."
                    .to_string(),
            ]),
        )
    })?;
    Ok(serde_json::json!({
        "command": "runner.jobs.artifacts.direct_daemon_content",
        "job_id": job_id,
        "artifact_id": artifact_id,
        "content_base64": content_base64,
        "filename": artifact.name,
        "mime": artifact.mime,
        "size_bytes": artifact.size_bytes,
        "sha256": artifact.sha256,
    }))
}

fn reverse_broker_url(runner_id: &str) -> Result<String> {
    let report = status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not connected"),
            Some(runner_id.to_string()),
            None,
        ));
    };
    if session.mode != RunnerTunnelMode::Reverse {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not reverse-connected"),
            Some(runner_id.to_string()),
            Some(vec![
                "Broker job wrappers require a reverse runner session.".to_string(),
            ]),
        ));
    }
    session.broker_url.ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner_id",
            format!("reverse runner `{runner_id}` has no broker URL"),
            Some(runner_id.to_string()),
            Some(vec![
                "Reconnect with `homeboy runner connect <controller-id> --reverse --reverse-runner <runner-id> --broker-url <url>`.".to_string(),
            ]),
        )
    })
}

fn broker_client(action: &str) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("{action}: {err}")))
}

pub(crate) fn daemon_endpoint_identity(local_url: &str) -> std::result::Result<String, String> {
    daemon_http_identity(local_url)
}

pub(crate) fn local_live_session(
    runner_id: &str,
    timeout: Duration,
) -> Result<Option<RunnerSession>> {
    let Some(session) = read_session_or_live_peer(runner_id)? else {
        return Ok(None);
    };
    Ok(session_is_live_with_timeout(&session, timeout).then_some(session))
}

fn stale_daemon_warning(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Result<Option<RunnerStaleDaemonWarning>> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let Some(session) = session else {
        return Ok(None);
    };
    if session.mode != RunnerTunnelMode::DirectSsh {
        return Ok(None);
    }
    let homeboy = remote_runner_homeboy_path(runner, "runner status stale-daemon diagnostics")?;
    let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
        return Ok(None);
    };
    let current_identity = match remote_homeboy_identity(&client, homeboy) {
        Ok(identity) => identity,
        Err(_) => return Ok(None),
    };
    let current_version = current_identity.version.clone();
    let observed_session_version = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_version(local_url).ok())
        .unwrap_or_else(|| session.homeboy_version.clone());
    let daemon_identity = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_identity(local_url).ok())
        .filter(|identity| !identity.trim().is_empty());
    let session_identity = daemon_identity.or_else(|| session.homeboy_build_identity.clone());
    let stale_runtime_paths = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_runtime_stale_paths(local_url).ok())
        .unwrap_or_default();
    let changed_runtime_paths = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_runtime_loaded_paths(local_url).ok())
        .map(|loaded| changed_runtime_paths(&runner.env, &loaded))
        .unwrap_or_default();
    if versions_match(&observed_session_version, &current_version)
        && versions_match(&session.homeboy_version, &current_version)
        && identities_match(session_identity.as_deref(), Some(&current_identity.display))
        && stale_runtime_paths.is_empty()
        && changed_runtime_paths.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(
        RunnerStaleDaemonWarning::new(
            &runner.id,
            observed_session_version,
            current_version,
            session_identity,
            Some(current_identity.display),
        )
        .with_runtime_paths(&runner.id, stale_runtime_paths, changed_runtime_paths),
    ))
}

fn stale_reattach_warning_for_report(
    runner_id: &str,
    report: &DaemonFreshnessReport,
) -> Option<String> {
    (!report.fresh).then(|| format!(
        "Reattached the live remote daemon lease after drift ({:?}; {}); active jobs were preserved. Continue with `homeboy runner refresh-homeboy {runner_id} --reconnect` when its work is complete.",
        report.stale_reason_code,
        report.ownership_evidence.as_deref().unwrap_or("remote status freshness drift")
    ))
}

fn changed_runtime_paths(
    runner_env: &std::collections::HashMap<String, String>,
    loaded_paths: &BTreeMap<String, String>,
) -> Vec<RunnerChangedRuntimePath> {
    let current_paths: BTreeMap<String, String> = runner_env
        .iter()
        .filter(|(name, value)| is_runtime_path_env(name) && !value.trim().is_empty())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut names: std::collections::BTreeSet<String> = loaded_paths.keys().cloned().collect();
    names.extend(current_paths.keys().cloned());
    names
        .into_iter()
        .filter_map(|env| {
            let loaded_path = loaded_paths.get(&env).cloned();
            let configured_path = current_paths.get(&env).cloned();
            if loaded_path == configured_path {
                None
            } else {
                Some(RunnerChangedRuntimePath {
                    env,
                    loaded_path,
                    configured_path,
                })
            }
        })
        .collect()
}

fn is_runtime_path_env(name: &str) -> bool {
    name.starts_with("HOMEBOY_")
        && [
            "_COMPONENT_PATH",
            "_PLUGIN_PATH",
            "_PROVIDER_PATH",
            "_RUNTIME_PATH",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub fn statuses() -> Result<Vec<RunnerStatusReport>> {
    let mut reports = Vec::new();
    for runner in super::list()? {
        reports.push(status(&runner.id)?);
    }
    Ok(reports)
}

pub fn disconnect(runner_id: &str) -> Result<RunnerDisconnectReport> {
    stop_transport_recovery::disconnect_with_force(runner_id, false)
}

mod remote_daemon;
mod session_store;

use remote_daemon::*;
use session_store::*;

#[cfg(test)]
mod tests;
