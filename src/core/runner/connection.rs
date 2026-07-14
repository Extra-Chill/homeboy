use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::api_jobs::{ActiveRunnerJobSummary, JobClaimMetadata, JobStatus, RunnerJobSource};
use crate::core::daemon::{
    DaemonFreshnessReport, DaemonLeaselessRecoveryResult, DaemonStaleReasonCode,
    DaemonStateLossRecoveryResult,
};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::http_api::RunSummary;
use crate::core::paths;
use crate::core::server::{self, Server, SshClient};

use super::session::{
    ReverseRunnerConnectOptions, RunnerActiveJobError, RunnerActiveJobSource, RunnerActiveJobState,
    RunnerChangedRuntimePath, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerLeaselessRecoveryContract, RunnerLeaselessRecoveryEvidence, RunnerSession,
    RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
    RunnerTunnelMode,
};
use super::{broker_auth, broker_http};
use super::{load, remote_runner_homeboy_path, Runner, RunnerKind};

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

use super::daemon_http_get::daemon_get;

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption(runner_id, None, false, None, None, None)
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
    connect_with_orphan_adoption(runner_id, orphan_lease_id, false, None, None, None)
}

/// Reconnect after the explicit lease-less recovery transaction has terminalized
/// the unowned store and started its replacement daemon.
pub fn connect_with_leaseless_orphan_reconciliation(
    runner_id: &str,
) -> Result<(RunnerConnectReport, i32)> {
    connect_with_orphan_adoption(runner_id, None, true, None, None, None)
}

/// Connect while explicitly adopting one recorded dead remote lease. This is an
/// operator recovery path; ordinary reconnects never infer orphan ownership.
pub fn connect_with_orphan_adoption(
    runner_id: &str,
    orphan_lease_id: Option<&str>,
    reconcile_leaseless_orphans: bool,
    missing_lease_id: Option<&str>,
    recorded_pid: Option<u32>,
    recorded_endpoint: Option<&str>,
) -> Result<(RunnerConnectReport, i32)> {
    // Reconnect replaces daemon runtime state. It shares the promotion lease
    // with binary selection so a second session cannot reconnect against a
    // different configured executable halfway through the transaction.
    let promotion_lease =
        crate::core::runtime_promotion::acquire("runner daemon reconnect", runner_id.to_string())?;
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
    let previous_session = read_session(runner_id)?;

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
            leaseless_recovery_evidence: Some(evidence.clone()),
        })?;
    }

    let daemon = ensure_remote_daemon(&client, homeboy, previous_session.as_ref(), orphan_lease_id);
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
    let connection_warning = daemon
        .inspected_freshness
        .as_ref()
        .and_then(|report| stale_reattach_warning_for_report(&runner.id, report));
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: None,
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
        leaseless_recovery_evidence: recovery_evidence.clone(),
    };
    write_session(&session)?;

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
        RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner => "--confirm-no-daemon-owner",
        RunnerLeaselessRecoveryContract::ReconcileLeaselessOrphansAndConfirmNoDaemonOwner => {
            "--reconcile-leaseless-orphans --confirm-no-daemon-owner"
        }
        RunnerLeaselessRecoveryContract::ConfirmControlPlaneLost => "--confirm-control-plane-lost",
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
        crate::core::server::CommandOutput,
    ),
    String,
>
where
    Probe: FnOnce() -> crate::core::server::CommandOutput,
    Recover: FnOnce(RunnerLeaselessRecoveryContract) -> crate::core::server::CommandOutput,
{
    let probe = probe();
    let contract = negotiate_leaseless_recovery_contract(&probe)?;
    Ok((contract.clone(), recover(contract)))
}

fn negotiate_leaseless_recovery_contract(
    output: &crate::core::server::CommandOutput,
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
    let reconcile_leaseless_orphans = options.contains("--reconcile-leaseless-orphans");
    let confirm_control_plane_lost = options.contains("--confirm-control-plane-lost");
    match (
        confirm_no_daemon_owner,
        reconcile_leaseless_orphans,
        confirm_control_plane_lost,
    ) {
        (true, false, false) => Ok(RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner),
        (true, true, false) => Ok(
            RunnerLeaselessRecoveryContract::ReconcileLeaselessOrphansAndConfirmNoDaemonOwner,
        ),
        (false, false, true) => Ok(RunnerLeaselessRecoveryContract::ConfirmControlPlaneLost),
        (false, false, false) => Err(
            "lease-less recovery capability probe did not advertise a supported confirmation contract; refusing recovery"
                .to_string(),
        ),
        _ => Err(
            "lease-less recovery capability probe advertised ambiguous confirmation contracts; refusing recovery"
                .to_string(),
        ),
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

fn leaseless_recovery_failure_message(output: &crate::core::server::CommandOutput) -> String {
    if output.timed_out {
        format!(
            "remote lease-less daemon recovery timed out after {}s; inspect `homeboy daemon status` on the runner before retrying",
            REMOTE_LEASELESS_RECOVERY_TIMEOUT.as_secs()
        )
    } else {
        command_failure_message("remote lease-less daemon recovery failed", output)
    }
}

fn state_loss_recovery_failure_message(output: &crate::core::server::CommandOutput) -> String {
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
    let homeboy_identity = crate::core::build_identity::current();
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
    let session = read_session(runner_id)?;
    let state = session_state(session.as_ref());
    let connected = state == RunnerSessionState::Connected;
    let stale_daemon = stale_daemon_warning(&runner, session.as_ref(), connected)?;
    let daemon_freshness = runner_daemon_freshness(&runner, session.as_ref(), connected)?
        .or_else(|| remote_daemon_recovery_freshness(runner_id, &runner));
    let active_job_source = session.as_ref().and_then(active_runner_job_source);
    let (active_jobs, stale_jobs, active_job_state, active_job_error) = if connected {
        match session.as_ref() {
            Some(session) => match runner_jobs(runner_id, session) {
                Ok((active_jobs, mut stale_jobs)) => {
                    stale_jobs.extend(orphaned_child_run_jobs(runner_id, session, &active_jobs));
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
    let active_job_count = active_jobs.len();
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
        session_path: session_path.display().to_string(),
    })
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
    Ok(report.active_jobs)
}

fn runner_daemon_freshness(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Result<Option<crate::core::daemon::DaemonFreshnessReport>> {
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

pub fn reverse_broker_artifact(runner_id: &str, job_id: &str, artifact_id: &str) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let artifact_id = crate::core::execution_contract::encode_uri_component(artifact_id);
    let client = broker_client("build broker artifact client")?;
    broker_http::get_json(
        &client,
        &broker_url,
        &format!("/runner/jobs/{job_id}/artifacts/{artifact_id}"),
        "lookup reverse runner broker artifact",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
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
        "Reattached the live remote daemon lease after drift ({:?}; {}); active jobs were preserved. Refresh it explicitly with `homeboy runner refresh-homeboy {runner_id} --reconnect` when its work is complete.",
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
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    if let Some(session) = &session {
        if session.mode == RunnerTunnelMode::DirectSsh {
            if let Err(err) = disconnect_remote_daemon(&runner, session) {
                log_status!(
                    "runner",
                    "Could not stop remote daemon for runner `{runner_id}` during disconnect: {err}"
                );
            }
        }
        if let Some(pid) = session.tunnel_pid {
            terminate_pid(pid);
        }
    }
    if session_path.exists() {
        std::fs::remove_file(&session_path).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("delete {}", session_path.display())),
            )
        })?;
    }
    Ok(RunnerDisconnectReport {
        runner_id: runner_id.to_string(),
        disconnected: session.is_some(),
        session,
        session_path: session_path.display().to_string(),
    })
}

fn disconnect_remote_daemon(
    runner: &Runner,
    session: &RunnerSession,
) -> std::result::Result<(), String> {
    let Some((_, _, client)) =
        remote_daemon::resolve_ssh_runner(runner).map_err(|err| err.message)?
    else {
        return Ok(());
    };
    let homeboy =
        remote_runner_homeboy_path(runner, "runner disconnect").map_err(|err| err.message)?;
    let output = client.execute(&remote_daemon_disconnect_command(
        homeboy,
        session.remote_daemon_pid,
    ));
    if !output.success {
        return Err(command_failure_message(
            "remote daemon stop failed while disconnecting runner",
            &output,
        ));
    }
    Ok(())
}

fn remote_daemon_disconnect_command(homeboy: &str, remote_daemon_pid: Option<u32>) -> String {
    let mut command = format!(
        "{} daemon stop >/dev/null 2>&1 || true",
        shell::quote_arg(homeboy)
    );
    if let Some(pid) = remote_daemon_pid {
        command.push_str("\n");
        command.push_str(&format!("pid={pid}\n"));
        command.push_str("if [ -r \"/proc/$pid/cmdline\" ]; then\n");
        command
            .push_str("  cmdline=$(tr '\\0' ' ' < \"/proc/$pid/cmdline\" 2>/dev/null || true)\n");
        command.push_str("  case \"$cmdline\" in\n");
        command.push_str("    *\" daemon serve \"*)\n");
        command.push_str("      kill -TERM \"$pid\" 2>/dev/null || true\n");
        command.push_str("      i=0\n");
        command.push_str("      while kill -0 \"$pid\" 2>/dev/null && [ \"$i\" -lt 20 ]; do\n");
        command.push_str("        i=$((i + 1))\n");
        command.push_str("        sleep 0.1\n");
        command.push_str("      done\n");
        command.push_str("      if kill -0 \"$pid\" 2>/dev/null; then\n");
        command.push_str("        kill -KILL \"$pid\" 2>/dev/null || true\n");
        command.push_str("      fi\n");
        command.push_str("      ;;\n");
        command.push_str("  esac\n");
        command.push_str("fi");
    }
    command
}

mod remote_daemon {
    use super::*;
    use crate::core::daemon::{DaemonFreshnessReport, DaemonRecoveryEvidence};
    use std::time::Duration;

    pub(super) const REMOTE_DAEMON_STATUS_TIMEOUT: Duration = Duration::from_secs(15);

    pub(super) fn resolve_ssh_runner(
        runner: &Runner,
    ) -> Result<Option<(String, Server, SshClient)>> {
        if runner.kind != RunnerKind::Ssh {
            return Ok(None);
        }
        let server_id = runner.server_id.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "server_id",
                "SSH runner requires server_id",
                Some(runner.id.clone()),
                None,
            )
        })?;
        let server = server::load(&server_id)?;
        let mut client = SshClient::from_server(&server, &server_id)?;
        client.env.extend(runner.env.clone());
        Ok(Some((server_id, server, client)))
    }

    pub(super) fn remote_homeboy_version(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<String, String> {
        let command = format!("{} --version", shell::quote_arg(homeboy));
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote Homeboy version check failed",
                &output,
            ));
        }
        let version = output.stdout.trim().to_string();
        if version.is_empty() {
            return Err("remote Homeboy version check returned empty output".to_string());
        }
        Ok(version)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct RemoteHomeboyIdentity {
        pub(super) version: String,
        pub(super) display: String,
    }

    pub(super) fn remote_homeboy_identity(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteHomeboyIdentity, String> {
        let command = format!("{} self identity", shell::quote_arg(homeboy));
        let output = client.execute(&command);
        if output.success {
            if let Some(identity) = parse_self_identity_output(&output.stdout) {
                return Ok(identity);
            }
        }

        let version = remote_homeboy_version(client, homeboy)?;
        Ok(RemoteHomeboyIdentity {
            version: normalize_homeboy_version_owned(&version),
            display: version,
        })
    }

    pub(super) fn parse_self_identity_output(output: &str) -> Option<RemoteHomeboyIdentity> {
        let body: Value = serde_json::from_str(output.trim()).ok()?;
        let data = body.get("data").unwrap_or(&body);
        let version = data.get("version")?.as_str()?.trim();
        let display = data
            .get("display")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(version);
        if version.is_empty() {
            return None;
        }
        Some(RemoteHomeboyIdentity {
            version: version.to_string(),
            display: display.to_string(),
        })
    }

    pub(super) fn identities_match(left: Option<&str>, right: Option<&str>) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => versions_match(left, right),
            _ => false,
        }
    }

    pub(super) struct SshTunnelOutput {
        pub(super) pid: Option<u32>,
        pub(super) stderr: String,
        pub(super) success: bool,
    }

    pub(super) fn open_loopback_tunnel(
        server: &Server,
        local_port: u16,
        remote_host: &str,
        remote_port: u16,
    ) -> SshTunnelOutput {
        if is_loopback_host(&server.host) {
            return SshTunnelOutput {
                pid: None,
                stderr: String::new(),
                success: true,
            };
        }

        let mut args = crate::core::server::ssh_args::server_option_args(
            server,
            crate::core::server::ssh_args::SshArgOptions {
                batch_mode: true,
                connect_timeout: true,
                exit_on_forward_failure: true,
                port_flag: Some(crate::core::server::ssh_args::SshPortFlag::Lowercase),
                ..crate::core::server::ssh_args::SshArgOptions::default()
            },
        );
        args.extend([
            "-N".to_string(),
            "-L".to_string(),
            format!("127.0.0.1:{}:{}:{}", local_port, remote_host, remote_port),
            format!("{}@{}", server.user, server.host),
        ]);

        let child = std::process::Command::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(child) => SshTunnelOutput {
                pid: Some(child.id()),
                stderr: String::new(),
                success: true,
            },
            Err(err) => SshTunnelOutput {
                pid: None,
                stderr: format!("SSH tunnel error: {}", err),
                success: false,
            },
        }
    }

    #[derive(Debug, Clone)]
    pub(super) struct RemoteDaemon {
        pub(super) address: String,
        pub(super) pid: Option<u32>,
        pub(super) lease_id: Option<String>,
        pub(super) version: Option<String>,
        pub(super) build_identity: Option<String>,
        pub(super) inspected_freshness: Option<DaemonFreshnessReport>,
    }

    #[derive(Debug, Clone)]
    pub(super) struct RemoteDaemonStatus {
        pub(super) daemon: Option<RemoteDaemon>,
        pub(super) stale_reason: Option<String>,
        pub(super) stale_reason_code: Option<DaemonStaleReasonCode>,
        pub(super) fresh: bool,
        pub(super) reachable: bool,
        pub(super) active_jobs: usize,
        pub(super) endpoint_probe_error: Option<String>,
    }

    pub(super) fn remote_daemon_recovery_freshness_from_status(
        runner_id: &str,
        status: &RemoteDaemonStatus,
    ) -> DaemonFreshnessReport {
        let daemon = status.daemon.as_ref();
        let lease_id = daemon.and_then(|daemon| daemon.lease_id.clone());
        let pid = daemon.and_then(|daemon| daemon.pid);
        let proven_dead = status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
            && lease_id.is_some()
            && pid.is_some();
        let mut ownership_evidence = if proven_dead {
            Some(format!(
                "remote daemon status over SSH proved PID {} is dead for lease `{}`",
                pid.expect("proven dead PID"),
                lease_id.as_deref().expect("proven dead lease")
            ))
        } else {
            Some("remote daemon lease evidence is unavailable; active jobs are protected from implicit replacement".to_string())
        };
        if let Some(error) = &status.endpoint_probe_error {
            ownership_evidence = Some(format!(
                "{}; reachable endpoint identity probe failed: {error}",
                ownership_evidence.unwrap_or_default()
            ));
        }
        if let Some(stale_reason) = &status.stale_reason {
            ownership_evidence = Some(format!(
                "{}; inspected stale reason: {stale_reason}",
                ownership_evidence.unwrap_or_default()
            ));
        }
        let adoption_command = proven_dead.then(|| {
            format!(
                "homeboy runner connect {} --adopt-orphan-lease {} --confirm-pid-dead",
                shell::quote_arg(runner_id),
                shell::quote_arg(lease_id.as_deref().expect("proven dead lease"))
            )
        });
        DaemonFreshnessReport {
            fresh: status.fresh,
            stale_reason_code: status.stale_reason_code,
            restartable: false,
            lease_id,
            pid,
            recovery_evidence: Some(if proven_dead {
                DaemonRecoveryEvidence::ProvenDead
            } else {
                DaemonRecoveryEvidence::Unavailable
            }),
            ownership_evidence,
            adoption_command,
            binary_hash: None,
            daemon_version: daemon.and_then(|daemon| daemon.version.clone()),
            daemon_build_identity: daemon.and_then(|daemon| daemon.build_identity.clone()),
            runtime_paths: None,
            active_jobs: status.active_jobs,
            repair_plan: Vec::new(),
        }
    }

    pub(super) fn unavailable_recovery_freshness(
        error: impl Into<String>,
    ) -> DaemonFreshnessReport {
        DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(DaemonStaleReasonCode::TransportUnreachable),
            restartable: false,
            lease_id: None,
            pid: None,
            recovery_evidence: Some(DaemonRecoveryEvidence::Unavailable),
            ownership_evidence: Some(format!(
                "remote daemon recovery evidence unavailable: {}",
                error.into()
            )),
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            repair_plan: Vec::new(),
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum RemoteDaemonConnectAction {
        Reattach,
        Start,
    }

    pub(super) fn ensure_remote_daemon(
        client: &SshClient,
        homeboy: &str,
        previous_session: Option<&RunnerSession>,
        orphan_lease_id: Option<&str>,
    ) -> std::result::Result<RemoteDaemon, String> {
        let mut status = remote_daemon_status(client, homeboy)?;
        probe_remote_daemon_endpoint(client, &mut status);
        if let Some(lease_id) = orphan_lease_id {
            if status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
                && status
                    .daemon
                    .as_ref()
                    .and_then(|daemon| daemon.lease_id.as_deref())
                    == Some(lease_id)
            {
                return remote_daemon_adopt_orphan(client, homeboy, lease_id);
            }
        }
        let inspected_freshness =
            remote_daemon_recovery_freshness_from_status("<runner-id>", &status);
        match remote_daemon_connect_action_with_controller_identity(
            previous_session,
            &status,
            &crate::core::build_identity::current().display,
        )? {
            RemoteDaemonConnectAction::Reattach => {
                let mut daemon = status.daemon.ok_or_else(|| {
                    "remote daemon reattach selected without a daemon lease".to_string()
                })?;
                daemon.inspected_freshness = Some(inspected_freshness);
                return Ok(daemon);
            }
            RemoteDaemonConnectAction::Start => {
                return remote_daemon_ensure_running(client, homeboy)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn remote_daemon_connect_action(
        previous_session: Option<&RunnerSession>,
        status: &RemoteDaemonStatus,
    ) -> std::result::Result<RemoteDaemonConnectAction, String> {
        remote_daemon_connect_action_with_controller_identity(
            previous_session,
            status,
            &crate::core::build_identity::current().display,
        )
    }

    pub(super) fn remote_daemon_connect_action_with_controller_identity(
        previous_session: Option<&RunnerSession>,
        status: &RemoteDaemonStatus,
        controller_identity: &str,
    ) -> std::result::Result<RemoteDaemonConnectAction, String> {
        let Some(daemon) = status.daemon.as_ref() else {
            if status.active_jobs > 0 {
                return Err(format!(
                    "remote daemon status has {} active job(s) but no daemon state; refusing ensure-running or implicit replacement. Inspect `homeboy daemon status` and use the explicit active-job recovery guidance before retrying.",
                    status.active_jobs
                ));
            }
            return Ok(RemoteDaemonConnectAction::Start);
        };

        if !status.reachable {
            return Err(format!(
                "remote daemon is unreachable; refusing to replace or persist a session{}",
                active_job_recovery_guidance(status.active_jobs)
            ));
        }
        if daemon.lease_id.as_deref().is_none_or(str::is_empty) || daemon.pid.is_none() {
            return Err(format!(
                "reachable remote daemon did not report both lease and PID; refusing to replace or persist a session{}",
                active_job_recovery_guidance(status.active_jobs)
            ));
        }
        if parse_loopback_daemon_addr(&daemon.address).is_err() {
            return Err(format!(
                "reachable remote daemon did not report a loopback address; refusing to replace or persist a session{}",
                active_job_recovery_guidance(status.active_jobs)
            ));
        }
        if let Some(session) = previous_session.filter(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
        }) {
            if let Some(expected_lease) = session.remote_daemon_lease_id.as_deref() {
                let actual_lease = daemon.lease_id.as_deref().expect("checked above");
                if expected_lease != actual_lease {
                    return Err(format!(
                        "live remote daemon lease `{actual_lease}` does not match persisted session lease `{expected_lease}`; refusing to replace the live daemon"
                    ));
                }
            } else if session.remote_daemon_pid != daemon.pid
                || session.remote_daemon_address.as_deref() != Some(daemon.address.as_str())
            {
                return Err("persisted direct-SSH runner session has no daemon lease and does not match the live daemon PID/address; refusing replacement".to_string());
            }
        }

        // Retain a live daemon across version/build/runtime drift. The tunnel's
        // health endpoint must still prove this exact lease and PID before a
        // session is persisted.
        if !status.fresh {
            return Ok(RemoteDaemonConnectAction::Reattach);
        }

        let healthy = status.fresh && status.reachable;

        if healthy {
            if previous_session.is_none() && status.active_jobs > 0 {
                let daemon = status.daemon.as_ref().expect("healthy daemon exists");
                let daemon_identity = daemon.build_identity.as_deref().ok_or_else(|| format!(
                    "remote daemon has {} active job(s) but its reachable endpoint did not provide a build identity; refusing reattachment or replacement",
                    status.active_jobs
                ))?;
                let daemon_version = daemon.version.as_deref().ok_or_else(|| format!(
                    "remote daemon has {} active job(s) but its reachable endpoint did not provide a version; refusing reattachment or replacement",
                    status.active_jobs
                ))?;
                if daemon_identity.trim() != controller_identity.trim() {
                    return Err(format!(
                        "remote daemon has {} active job(s) under reachable lease `{}` (PID {}) but build identity `{daemon_identity}` / version `{daemon_version}` does not match this controller `{controller_identity}`; refusing replacement. Run a controller pinned to `{daemon_identity}` and retry `homeboy runner connect <runner-id>` to reattach this exact lease.",
                        status.active_jobs,
                        daemon.lease_id.as_deref().unwrap_or("unavailable"),
                        daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
                    ));
                }
            }
            if let Some(session) = previous_session.filter(|session| {
                session.mode == RunnerTunnelMode::DirectSsh
                    && session.role == RunnerSessionRole::Controller
            }) {
                if session.remote_daemon_lease_id.is_none() {
                    if session.remote_daemon_pid == daemon.pid
                        && session.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
                    {
                        return Ok(RemoteDaemonConnectAction::Reattach);
                    }
                    return Err("persisted direct-SSH runner session has no daemon lease and does not match the live daemon PID/address; refusing replacement".to_string());
                }
                let expected_lease = session.remote_daemon_lease_id.as_deref().expect("checked");
                let actual_lease = daemon.lease_id.as_deref().ok_or_else(|| {
                    "live remote daemon did not report a lease; refusing to replace it".to_string()
                })?;
                if expected_lease != actual_lease {
                    return Err(format!(
                        "live remote daemon lease `{actual_lease}` does not match persisted session lease `{expected_lease}`; refusing to replace the live daemon"
                    ));
                }
            }
            return Ok(RemoteDaemonConnectAction::Reattach);
        }

        Ok(RemoteDaemonConnectAction::Start)
    }

    fn active_job_recovery_guidance(active_jobs: usize) -> String {
        (active_jobs > 0)
            .then(|| format!(
                "; {active_jobs} active job(s) were not replaced. Inspect `homeboy daemon status` and use explicit active-job recovery guidance before retrying"
            ))
            .unwrap_or_default()
    }

    pub(super) fn remote_daemon_status(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteDaemonStatus, String> {
        let command = format!("{} daemon status", shell::quote_arg(homeboy));
        let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
        if !output.success {
            return Err(command_failure_message(
                "remote daemon status failed",
                &output,
            ));
        }
        let envelope = parse_envelope(&output.stdout)
            .map_err(|err| format!("remote daemon status returned invalid JSON: {}", err))?;
        if !envelope.success {
            return Err(format!(
                "remote daemon status returned an error: {}",
                envelope.error.unwrap_or(Value::Null)
            ));
        }
        let data = envelope
            .data
            .ok_or_else(|| "remote daemon status returned no data".to_string())?;
        let stale_reason = data
            .get("stale_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let stale_reason_code = data
            .pointer("/freshness/stale_reason_code")
            .cloned()
            .and_then(|code| serde_json::from_value(code).ok());
        if !data
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(RemoteDaemonStatus {
                daemon: data.get("state").map(remote_daemon_from_state),
                stale_reason,
                stale_reason_code,
                fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
                reachable: data
                    .get("reachable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                active_jobs: remote_daemon_active_jobs(&data),
                endpoint_probe_error: None,
            });
        }
        let Some(state) = data.get("state") else {
            return Ok(RemoteDaemonStatus {
                daemon: None,
                stale_reason: Some(
                    stale_reason
                        .unwrap_or_else(|| "remote daemon status has no lease state".to_string()),
                ),
                stale_reason_code,
                fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
                reachable: data
                    .get("reachable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                active_jobs: remote_daemon_active_jobs(&data),
                endpoint_probe_error: None,
            });
        };
        Ok(RemoteDaemonStatus {
            daemon: Some(remote_daemon_from_state(state)),
            stale_reason,
            stale_reason_code,
            fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reachable: data
                .get("reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active_jobs: remote_daemon_active_jobs(&data),
            endpoint_probe_error: None,
        })
    }

    pub(super) fn probe_remote_daemon_endpoint(
        client: &SshClient,
        status: &mut RemoteDaemonStatus,
    ) {
        if !status.reachable {
            return;
        }
        let Some(daemon) = status.daemon.as_mut() else {
            return;
        };
        if parse_loopback_daemon_addr(&daemon.address).is_err() {
            status.endpoint_probe_error = Some(
                "remote daemon status reported a non-loopback endpoint; refusing identity probe"
                    .to_string(),
            );
            return;
        }
        let command = format!(
            "curl --fail --silent --show-error --max-time 2 {}/version",
            shell::quote_arg(&format!("http://{}", daemon.address))
        );
        let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
        if !output.success {
            status.endpoint_probe_error = Some(command_failure_message(
                "remote daemon endpoint identity probe failed",
                &output,
            ));
            return;
        }
        let body: Value = match parse_json_from_mixed_stdout(&output.stdout) {
            Ok(body) => body,
            Err(error) => {
                status.endpoint_probe_error = Some(format!(
                    "remote daemon endpoint identity probe returned invalid JSON: {error}"
                ));
                return;
            }
        };
        daemon.version = body
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| body.pointer("/data/version").and_then(Value::as_str))
            .map(str::to_string);
        daemon.build_identity = body
            .pointer("/build_identity/display")
            .and_then(Value::as_str)
            .or_else(|| {
                body.pointer("/data/build_identity/display")
                    .and_then(Value::as_str)
            })
            .map(str::to_string);
        if daemon.version.is_none() || daemon.build_identity.is_none() {
            status.endpoint_probe_error = Some(
                "remote daemon endpoint identity probe did not return both version and build identity"
                    .to_string(),
            );
        }
    }

    fn remote_daemon_from_state(state: &Value) -> RemoteDaemon {
        RemoteDaemon {
            address: state
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            pid: state
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            lease_id: state
                .get("lease_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        }
    }

    pub(super) fn remote_daemon_active_jobs(data: &Value) -> usize {
        data.pointer("/freshness/active_jobs")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
    }

    pub(super) fn remote_daemon_ensure_running(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteDaemon, String> {
        let command = format!(
            "{} daemon ensure-running --addr 127.0.0.1:0",
            shell::quote_arg(homeboy)
        );
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote daemon ensure-running failed",
                &output,
            ));
        }
        let envelope = parse_envelope(&output.stdout).map_err(|err| {
            format!(
                "remote daemon ensure-running returned invalid JSON: {}",
                err
            )
        })?;
        if !envelope.success {
            return Err(format!(
                "remote daemon ensure-running failed: {}",
                envelope.error.unwrap_or(Value::Null)
            ));
        }
        let data = envelope
            .data
            .ok_or_else(|| "remote daemon ensure-running returned no data".to_string())?;
        Ok(RemoteDaemon {
            address: data
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            pid: data
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            lease_id: data
                .get("lease_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        })
    }

    fn remote_daemon_adopt_orphan(
        client: &SshClient,
        homeboy: &str,
        lease_id: &str,
    ) -> std::result::Result<RemoteDaemon, String> {
        let command = remote_daemon_adopt_orphan_command(homeboy, lease_id);
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote daemon orphan adoption failed",
                &output,
            ));
        }
        let envelope = parse_envelope(&output.stdout)
            .map_err(|err| format!("remote daemon orphan adoption returned invalid JSON: {err}"))?;
        if !envelope.success {
            return Err(format!(
                "remote daemon orphan adoption failed: {}",
                envelope.error.unwrap_or(Value::Null)
            ));
        }
        let data = envelope
            .data
            .ok_or_else(|| "remote daemon orphan adoption returned no data".to_string())?;
        let replacement = data.get("replacement").ok_or_else(|| {
            "remote daemon orphan adoption returned no replacement lease".to_string()
        })?;
        Ok(RemoteDaemon {
            address: replacement
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            pid: replacement
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            lease_id: replacement
                .get("lease_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            version: None,
            build_identity: None,
            inspected_freshness: None,
        })
    }

    pub(super) fn remote_daemon_adopt_orphan_command(homeboy: &str, lease_id: &str) -> String {
        format!(
            "{} daemon adopt-orphan --lease-id {} --confirm-pid-dead --addr 127.0.0.1:0",
            shell::quote_arg(homeboy),
            shell::quote_arg(lease_id),
        )
    }

    pub(super) fn parse_envelope(stdout: &str) -> serde_json::Result<CliEnvelope> {
        parse_json_from_mixed_stdout(stdout)
    }

    pub(crate) fn parse_json_from_mixed_stdout<T>(stdout: &str) -> serde_json::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        match serde_json::from_str(stdout.trim()) {
            Ok(value) => Ok(value),
            Err(original) => {
                for (index, ch) in stdout.char_indices() {
                    if ch != '{' {
                        continue;
                    }
                    let mut stream =
                        serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
                    if let Some(Ok(value)) = stream.next() {
                        return Ok(value);
                    }
                }
                Err(original)
            }
        }
    }

    pub(super) fn parse_loopback_daemon_addr(address: &str) -> std::result::Result<SocketAddr, ()> {
        let addr: SocketAddr = address.parse().map_err(|_| ())?;
        if addr.ip().is_loopback() {
            Ok(addr)
        } else {
            Err(())
        }
    }

    pub(super) fn reserve_loopback_port() -> Result<u16> {
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("reserve local tunnel port".to_string()),
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|err| {
                Error::internal_io(err.to_string(), Some("read local tunnel port".to_string()))
            })?
            .port();
        drop(listener);
        Ok(port)
    }

    pub(super) fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }
}
use remote_daemon::*;

mod session_store {
    use super::*;

    pub(super) fn session_is_live(session: &RunnerSession) -> bool {
        if session.mode != RunnerTunnelMode::DirectSsh {
            return false;
        }
        if let Some(pid) = session.tunnel_pid {
            if !crate::core::process::pid_is_running(pid) {
                return false;
            }
        }
        session
            .local_port
            .is_some_and(|port| wait_for_tcp(port, Duration::from_millis(200)))
    }

    pub(super) fn reverse_controller_session_is_live(session: &RunnerSession) -> bool {
        let Some(last_seen_at) = session.last_seen_at.as_deref() else {
            return false;
        };
        let Ok(last_seen_at) = DateTime::parse_from_rfc3339(last_seen_at) else {
            return false;
        };
        let age = Utc::now().signed_duration_since(last_seen_at.with_timezone(&Utc));
        match age.to_std() {
            Ok(age) => age <= REVERSE_RUNNER_HEARTBEAT_TTL,
            Err(_) => true,
        }
    }

    pub(super) fn session_state(session: Option<&RunnerSession>) -> RunnerSessionState {
        match session {
            Some(session)
                if session.mode == RunnerTunnelMode::Reverse
                    && session.role == RunnerSessionRole::Controller =>
            {
                if reverse_controller_session_is_live(session) {
                    RunnerSessionState::Connected
                } else {
                    RunnerSessionState::Recorded
                }
            }
            Some(session) if session.mode == RunnerTunnelMode::Reverse => {
                RunnerSessionState::Recorded
            }
            Some(session) if session_is_live(session) => RunnerSessionState::Connected,
            Some(_) => RunnerSessionState::Disconnected,
            None => RunnerSessionState::Disconnected,
        }
    }

    pub(super) fn hostname_fallback() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown-host".to_string())
    }

    pub(super) fn session_path(runner_id: &str) -> Result<PathBuf> {
        paths::runner_session_file(runner_id)
    }

    pub(super) fn read_session(runner_id: &str) -> Result<Option<RunnerSession>> {
        let path = session_path(runner_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))
    }

    pub(super) fn write_session(session: &RunnerSession) -> Result<()> {
        let path = session_path(&session.runner_id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let body = serde_json::to_string_pretty(session).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize runner session".to_string()),
            )
        })?;
        std::fs::write(&path, body).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
        })
    }

    pub(super) fn failed_connect(
        runner_id: &str,
        session_path: PathBuf,
        failure_kind: RunnerFailureKind,
        failure_message: String,
    ) -> (RunnerConnectReport, i32) {
        (
            RunnerConnectReport {
                runner_id: runner_id.to_string(),
                mode: None,
                role: None,
                connected: false,
                recorded: None,
                local_url: None,
                broker_url: None,
                controller_id: None,
                remote_daemon_address: None,
                tunnel_pid: None,
                remote_daemon_pid: None,
                connection_warning: None,
                homeboy_version: None,
                homeboy_build_identity: None,
                session_path: Some(session_path.display().to_string()),
                leaseless_recovery: None,
                state_loss_recovery: None,
                leaseless_recovery_evidence: None,
                failure_kind: Some(failure_kind),
                failure_message: Some(failure_message),
            },
            20,
        )
    }

    pub(super) fn failed_connect_after_recovery(
        runner_id: &str,
        session_path: PathBuf,
        failure_kind: RunnerFailureKind,
        failure_message: String,
        leaseless_recovery: Option<DaemonLeaselessRecoveryResult>,
        leaseless_recovery_evidence: Option<RunnerLeaselessRecoveryEvidence>,
    ) -> (RunnerConnectReport, i32) {
        let mut failure = failed_connect(runner_id, session_path, failure_kind, failure_message);
        attach_leaseless_recovery(
            &mut failure.0,
            leaseless_recovery,
            leaseless_recovery_evidence,
        );
        failure
    }

    pub(super) fn attach_leaseless_recovery(
        report: &mut RunnerConnectReport,
        leaseless_recovery: Option<DaemonLeaselessRecoveryResult>,
        leaseless_recovery_evidence: Option<RunnerLeaselessRecoveryEvidence>,
    ) {
        report.leaseless_recovery = leaseless_recovery;
        report.leaseless_recovery_evidence = leaseless_recovery_evidence;
    }

    pub(super) fn command_failure_message(
        prefix: &str,
        output: &crate::core::server::CommandOutput,
    ) -> String {
        format!(
            "{} (exit {}): stdout={}, stderr={}",
            prefix,
            output.exit_code,
            output.stdout.trim(),
            output.stderr.trim()
        )
    }

    pub(super) fn is_loopback_host(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }

    pub(super) fn terminate_pid(pid: u32) {
        if pid > i32::MAX as u32 {
            return;
        }
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}
use session_store::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::super::session::RunnerStaleRuntimePath;
    use super::connection_daemon::{
        daemon_identity_from_body, daemon_runtime_loaded_paths_from_body,
        daemon_runtime_stale_paths_from_body, daemon_version_from_body, versions_match,
    };
    use super::*;
    use crate::test_support;

    #[test]
    fn rejects_non_loopback_remote_daemon_address() {
        assert!(parse_loopback_daemon_addr("0.0.0.0:1234").is_err());
        assert!(parse_loopback_daemon_addr("127.0.0.1:1234").is_ok());
    }

    #[test]
    fn parses_daemon_status_envelope() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{"action":"status","running":true,"state":{"address":"127.0.0.1:49152","pid":123}}}"#,
        )
        .expect("parse envelope");

        assert!(envelope.success);
        assert_eq!(
            envelope
                .data
                .unwrap()
                .get("state")
                .unwrap()
                .get("address")
                .unwrap(),
            "127.0.0.1:49152"
        );
    }

    #[test]
    fn reads_remote_active_job_count_from_daemon_freshness() {
        let status = serde_json::json!({
            "freshness": { "active_jobs": 2 }
        });

        assert_eq!(remote_daemon_active_jobs(&status), 2);
    }

    #[test]
    fn reattaches_active_daemon_without_changing_lease_or_pid() {
        let session = direct_ssh_session("lease-active");
        let status = remote_daemon_status_for_test(true, true, 1, "lease-active", 4242);

        let action = remote_daemon_connect_action(Some(&session), &status).expect("reattach");

        assert_eq!(action, RemoteDaemonConnectAction::Reattach);
        let daemon = status.daemon.expect("daemon");
        assert_eq!(daemon.lease_id.as_deref(), Some("lease-active"));
        assert_eq!(daemon.pid, Some(4242));
        assert_eq!(
            status.active_jobs, 1,
            "active job must not trigger replacement"
        );
    }

    #[test]
    fn tunnel_only_failure_reattaches_the_persisted_daemon() {
        let mut session = direct_ssh_session("lease-tunnel");
        session.tunnel_pid = Some(999_999);
        session.local_url = Some("http://127.0.0.1:1".to_string());
        let status = remote_daemon_status_for_test(true, true, 0, "lease-tunnel", 4343);

        assert_eq!(
            remote_daemon_connect_action(Some(&session), &status).expect("reattach"),
            RemoteDaemonConnectAction::Reattach
        );
    }

    #[test]
    fn stale_daemon_without_jobs_reattaches_without_replacement() {
        let status = remote_daemon_status_for_test(false, true, 0, "lease-stale", 4444);

        assert_eq!(
            remote_daemon_connect_action(None, &status).expect("reattach stale daemon"),
            RemoteDaemonConnectAction::Reattach
        );
    }

    #[test]
    fn reattaches_live_stale_daemon_with_active_jobs_without_replacing_it() {
        let status = remote_daemon_status_for_test_with_reason(
            false,
            true,
            1,
            "lease-busy",
            4545,
            Some(DaemonStaleReasonCode::VersionMismatch),
        );

        let action = remote_daemon_connect_action(None, &status).expect("reattach live daemon");

        assert_eq!(action, RemoteDaemonConnectAction::Reattach);
        let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);
        assert!(!recovery.fresh);
        assert_eq!(recovery.active_jobs, 1);
        assert_eq!(
            recovery.stale_reason_code,
            Some(DaemonStaleReasonCode::VersionMismatch)
        );
        let warning = stale_reattach_warning_for_report("homeboy-lab", &recovery)
            .expect("stale reattach warning");
        assert!(warning.contains("VersionMismatch"));
        assert!(warning.contains("active jobs were preserved"));
        assert!(warning.contains("refresh-homeboy homeboy-lab --reconnect"));
    }

    #[test]
    fn recorded_dead_daemon_with_active_jobs_refuses_implicit_replacement() {
        let session = direct_ssh_session("lease-dead");
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        let err = remote_daemon_connect_action(Some(&session), &status)
            .expect_err("require explicit orphan adoption");

        assert!(err.contains("unreachable"));
        assert!(err.contains("1 active job(s) were not replaced"));
        assert!(err.contains("active-job recovery guidance"));
        assert!(err.contains("Inspect `homeboy daemon status`"));
    }

    #[test]
    fn remote_dead_lease_recovery_exposes_exact_adoption_command() {
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

        assert_eq!(recovery.lease_id.as_deref(), Some("lease-dead"));
        assert_eq!(recovery.pid, Some(4545));
        assert_eq!(recovery.active_jobs, 1);
        assert_eq!(
            recovery.recovery_evidence,
            Some(crate::core::daemon::DaemonRecoveryEvidence::ProvenDead)
        );
        assert_eq!(
            recovery.adoption_command.as_deref(),
            Some("homeboy runner connect homeboy-lab --adopt-orphan-lease lease-dead --confirm-pid-dead")
        );
    }

    #[test]
    fn remote_status_without_reason_is_evidence_unavailable_and_non_adoptable() {
        let status = remote_daemon_status_for_test(false, false, 1, "lease-unknown", 4545);

        let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

        assert_eq!(recovery.lease_id.as_deref(), Some("lease-unknown"));
        assert_eq!(recovery.pid, Some(4545));
        assert_eq!(recovery.active_jobs, 1);
        assert_eq!(
            recovery.recovery_evidence,
            Some(crate::core::daemon::DaemonRecoveryEvidence::Unavailable)
        );
        assert!(recovery.adoption_command.is_none());
    }

    #[test]
    fn unavailable_remote_recovery_is_fail_closed() {
        let recovery = unavailable_recovery_freshness("remote command timed out");

        assert_eq!(
            recovery.stale_reason_code,
            Some(DaemonStaleReasonCode::TransportUnreachable)
        );
        assert_eq!(
            recovery.recovery_evidence,
            Some(crate::core::daemon::DaemonRecoveryEvidence::Unavailable)
        );
        assert!(recovery.lease_id.is_none());
        assert!(recovery.pid.is_none());
        assert!(recovery.adoption_command.is_none());
    }

    #[test]
    fn remote_daemon_status_probe_has_a_bounded_deadline() {
        assert_eq!(
            remote_daemon::REMOTE_DAEMON_STATUS_TIMEOUT,
            Duration::from_secs(15)
        );
    }

    #[test]
    fn remote_leaseless_recovery_decodes_and_propagates_report() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{
            "affected_job_ids": [],
            "affected_job_count": 0,
            "evidence_snapshot_path": "/evidence/jobs.snapshot",
            "ownership_proof": ["owner lock acquired"],
            "retry_guidance": "retry",
            "replacement": {
                "pid": 42,
                "address": "127.0.0.1:7421",
                "state_path": "/state.json",
                "lease_id": "lease-new"
            }
        }}"#,
        )
        .expect("parse daemon envelope");
        let recovery = decode_leaseless_recovery(envelope.data).expect("decode recovery report");
        assert_eq!(recovery.replacement.lease_id, "lease-new");
        assert_eq!(recovery.evidence_snapshot_path, "/evidence/jobs.snapshot");
        assert_eq!(recovery.ownership_proof, vec!["owner lock acquired"]);
    }

    #[test]
    fn state_loss_recovery_delegation_decodes_and_serializes_auditable_evidence() {
        let command =
            remote_state_loss_recovery_command("/opt/homeboy", "lease-old", 4242, "127.0.0.1:7421");
        assert!(command.contains("--recorded-endpoint 127.0.0.1:7421"));
        let envelope = parse_envelope(
            r#"{"success":true,"data":{
            "recovered_lease_id":"lease-old",
            "recorded_dead_pid":4242,
            "recorded_endpoint":"127.0.0.1:7421",
            "affected_job_ids":["7ab96605-38b7-4a6a-bbb8-99db839fa6dc"],
            "affected_job_count":1,
            "evidence_snapshot_path":"/evidence/jobs.snapshot",
            "ownership_proof":["owner lock acquired","endpoint unreachable"],
            "retry_guidance":"retry",
            "replacement":{"pid":43,"address":"127.0.0.1:7422","state_path":"/state.json","lease_id":"lease-new"}
        }}"#,
        )
        .expect("parse daemon envelope");
        let recovery = decode_state_loss_recovery(envelope.data).expect("decode recovery report");
        let (mut report, _) = failed_connect(
            "runner",
            std::path::PathBuf::from("/session.json"),
            RunnerFailureKind::DaemonStartupFailure,
            "test".to_string(),
        );
        report.state_loss_recovery = Some(recovery);
        let data = serde_json::to_value(report).expect("serialize controller report");
        assert_eq!(
            data["state_loss_recovery"]["recovered_lease_id"],
            "lease-old"
        );
        assert_eq!(
            data["state_loss_recovery"]["affected_job_ids"][0],
            "7ab96605-38b7-4a6a-bbb8-99db839fa6dc"
        );
        assert_eq!(
            data["state_loss_recovery"]["replacement"]["lease_id"],
            "lease-new"
        );
    }

    #[test]
    fn ensure_or_tunnel_failure_report_retains_completed_state_loss_recovery() {
        let envelope = parse_envelope(r#"{"success":true,"data":{"recovered_lease_id":"lease-old","recorded_dead_pid":42,"recorded_endpoint":"127.0.0.1:7421","affected_job_ids":[],"affected_job_count":0,"evidence_snapshot_path":"/snapshot","ownership_proof":[],"retry_guidance":"retry","replacement":{"pid":43,"address":"127.0.0.1:7422","state_path":"/state","lease_id":"lease-new"}}}"#).expect("envelope");
        let recovery = decode_state_loss_recovery(envelope.data).expect("recovery");
        let (mut report, _) = failed_connect(
            "runner",
            std::path::PathBuf::from("/session"),
            RunnerFailureKind::TunnelFailure,
            "tunnel failed".to_string(),
        );
        attach_state_loss_recovery(&mut report, Some(recovery));
        assert_eq!(
            report
                .state_loss_recovery
                .as_ref()
                .map(|value| value.replacement.lease_id.as_str()),
            Some("lease-new")
        );
    }

    #[test]
    fn remote_leaseless_recovery_timeout_is_actionable() {
        let message = leaseless_recovery_failure_message(&crate::core::server::CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            exit_code: 124,
            timed_out: true,
            child_resource: None,
        });
        assert!(message.contains("daemon status"));
    }

    #[cfg(unix)]
    #[test]
    fn runner_connect_persists_recovery_evidence_after_daemon_failure() {
        test_support::with_isolated_home(|home| {
            let daemon = home.path().join("remote-homeboy");
            let argv_path = home.path().join("recovery-argv");
            std::fs::write(
                &daemon,
                r#"#!/bin/sh
case "$1 $2" in
  "self identity")
    printf '%s\n' '{"success":true,"data":{"version":"0.284.0","display":"test"}}'
    ;;
    "daemon reconcile-leaseless-orphans")
    if [ "$3" = "--help" ]; then
      printf '%s\n' 'OPTIONS:' '    --confirm-no-daemon-owner'
    else
      printf '%s\n' "$@" > "$HOMEBOY_TEST_RECOVERY_ARGV"
      printf '%s\n' '{"success":true,"data":{"affected_job_ids":[],"affected_job_count":0,"affected_jobs":[],"historical_lease_ids":[],"evidence_snapshot_path":"/tmp/jobs.snapshot","ownership_proof":["owner lock acquired"],"retry_guidance":"retry","replacement":{"pid":42,"address":"127.0.0.1:7421","state_path":"/tmp/state.json","lease_id":"lease-new"}}}'
    fi
    ;;
  "daemon status") exit 1 ;;
esac
"#,
            )
            .expect("write remote Homeboy shim");
            let mut permissions = std::fs::metadata(&daemon)
                .expect("read remote Homeboy shim metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&daemon, permissions)
                .expect("make remote Homeboy shim executable");

            server::create(
                &serde_json::json!({
                    "id": "local-runner",
                    "host": "localhost",
                    "user": "test",
                })
                .to_string(),
                false,
            )
            .expect("create local server");
            super::super::create(
                &serde_json::json!({
                    "id": "local-runner",
                    "kind": "ssh",
                    "homeboy_path": daemon,
                    "env": { "HOMEBOY_TEST_RECOVERY_ARGV": argv_path },
                })
                .to_string(),
                false,
            )
            .expect("enable local runner");

            let (report, exit_code) =
                connect_with_orphan_adoption("local-runner", None, true, None, None, None)
                    .expect("connect result");

            assert_eq!(
                exit_code, 20,
                "the shim intentionally rejects status after recovery"
            );
            assert!(!report.connected);
            assert_eq!(
                report.failure_kind,
                Some(RunnerFailureKind::DaemonStartupFailure)
            );
            assert_eq!(
                report
                    .leaseless_recovery
                    .as_ref()
                    .expect("recovery report")
                    .replacement
                    .lease_id,
                "lease-new"
            );
            let evidence = report
                .leaseless_recovery_evidence
                .as_ref()
                .expect("recovery evidence");
            assert_eq!(
                evidence.contract,
                RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner
            );
            assert_eq!(evidence.remote_command_identity, "test");
            assert_eq!(
                evidence
                    .recovery
                    .as_ref()
                    .expect("recovery evidence result")
                    .replacement
                    .lease_id,
                "lease-new"
            );
            let session = read_session("local-runner")
                .expect("read recovery session")
                .expect("recovery session");
            assert!(session.local_url.is_none());
            assert!(session.local_port.is_none());
            assert_eq!(
                session
                    .leaseless_recovery_evidence
                    .as_ref()
                    .expect("persisted recovery evidence")
                    .recovery
                    .as_ref()
                    .expect("persisted recovery result")
                    .replacement
                    .lease_id,
                "lease-new"
            );
            assert_eq!(
                std::fs::read_to_string(argv_path).expect("read dispatched recovery argv"),
                "daemon\nreconcile-leaseless-orphans\n--confirm-no-daemon-owner\n--addr\n127.0.0.1:0\n"
            );
        });
    }

    #[test]
    fn state_loss_recovery_delegation_uses_the_canonical_exact_contract() {
        let command = remote_state_loss_recovery_command(
            "/opt/homeboy",
            "lease exact",
            4242,
            "127.0.0.1:4242",
        );
        assert_eq!(
            command,
            "/opt/homeboy daemon recover-missing-lease-state --lease-id 'lease exact' --recorded-pid 4242 --recorded-endpoint 127.0.0.1:4242 --confirm-pid-dead --confirm-control-plane-lost --addr 127.0.0.1:0"
        );
    }

    #[test]
    fn leaseless_recovery_uses_confirm_no_daemon_owner_contract() {
        let contract = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "OPTIONS:\n    --confirm-no-daemon-owner\n",
            false,
        ))
        .expect("one-flag contract");

        assert_eq!(
            contract,
            RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner
        );
        let command = remote_leaseless_recovery_command("/opt/homeboy", "127.0.0.1:0", contract);
        assert!(command.contains("--confirm-no-daemon-owner"));
        assert!(!command.contains("--reconcile-leaseless-orphans"));
        assert!(!command.contains("--confirm-control-plane-lost"));
    }

    #[test]
    fn leaseless_recovery_uses_two_flag_contract() {
        let contract = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n",
            false,
        ))
        .expect("two-flag contract");

        assert_eq!(
            contract,
            RunnerLeaselessRecoveryContract::ReconcileLeaselessOrphansAndConfirmNoDaemonOwner
        );
        let command = remote_leaseless_recovery_command("/opt/homeboy", "127.0.0.1:0", contract);
        assert!(command.contains("--reconcile-leaseless-orphans"));
        assert!(command.contains("--confirm-no-daemon-owner"));
        assert!(!command.contains("--confirm-control-plane-lost"));
    }

    #[test]
    fn leaseless_recovery_uses_control_plane_lost_contract() {
        let contract = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "OPTIONS:\n    --confirm-control-plane-lost\n",
            false,
        ))
        .expect("control-plane-lost contract");

        assert_eq!(
            contract,
            RunnerLeaselessRecoveryContract::ConfirmControlPlaneLost
        );
        let command = remote_leaseless_recovery_command("/opt/homeboy", "127.0.0.1:0", contract);
        assert!(command.contains("--confirm-control-plane-lost"));
        assert!(!command.contains("--confirm-no-daemon-owner"));
    }

    #[test]
    fn leaseless_recovery_refuses_unsupported_or_ambiguous_help() {
        let unsupported = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "OPTIONS:\n    --addr <ADDR>\n",
            false,
        ))
        .expect_err("unsupported contract");
        assert!(unsupported.contains("did not advertise"));

        let ambiguous = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n    --confirm-control-plane-lost\n",
            false,
        ))
        .expect_err("ambiguous contract");
        assert!(ambiguous.contains("ambiguous"));
    }

    #[test]
    fn leaseless_recovery_parses_only_exact_option_declarations() {
        let options = declared_long_options(
            "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n    --addr <ADDR>\n",
        );
        assert!(options.contains("--reconcile-leaseless-orphans"));
        assert!(options.contains("--confirm-no-daemon-owner"));
        assert!(options.contains("--addr"));

        let prose = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "Examples:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n",
            false,
        ))
        .expect_err("example lines must not advertise a contract");
        assert!(prose.contains("did not advertise"));

        let prose = negotiate_leaseless_recovery_contract(&command_output(
            true,
            "Options:\n    --reconcile-leaseless-orphans after inspection\n    --confirm-no-daemon-owner after inspection\n",
            false,
        ))
        .expect_err("prose in the options section must not advertise a contract");
        assert!(prose.contains("did not advertise"));
    }

    #[test]
    fn leaseless_recovery_evidence_records_selected_contract_and_command_identity() {
        for (help, expected_contract) in [
            (
                "Options:\n    --confirm-no-daemon-owner\n",
                RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner,
            ),
            (
                "Options:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n",
                RunnerLeaselessRecoveryContract::ReconcileLeaselessOrphansAndConfirmNoDaemonOwner,
            ),
            (
                "OPTIONS:\n    --confirm-control-plane-lost\n",
                RunnerLeaselessRecoveryContract::ConfirmControlPlaneLost,
            ),
        ] {
            let contract =
                negotiate_leaseless_recovery_contract(&command_output(true, help, false))
                    .expect("advertised contract");
            let evidence = leaseless_recovery_evidence(
                contract,
                "homeboy 0.284.1+abc123",
                sample_leaseless_recovery(),
            );

            assert_eq!(evidence.contract, expected_contract);
            assert_eq!(evidence.remote_command_identity, "homeboy 0.284.1+abc123");
            assert_eq!(
                evidence
                    .recovery
                    .as_ref()
                    .expect("recovery result")
                    .replacement
                    .lease_id,
                "lease-new"
            );
        }
    }

    #[test]
    fn persisted_session_without_leaseless_recovery_evidence_deserializes() {
        let session: RunnerSession = serde_json::from_value(serde_json::json!({
            "runner_id": "homeboy-lab",
            "server_id": null,
            "tunnel_pid": null,
            "remote_daemon_pid": null,
            "homeboy_version": "test",
            "connected_at": "2026-07-14T00:00:00Z"
        }))
        .expect("legacy session");

        assert!(session.leaseless_recovery_evidence.is_none());
    }

    #[test]
    fn persisted_recovery_evidence_without_result_deserializes() {
        let evidence: RunnerLeaselessRecoveryEvidence = serde_json::from_value(serde_json::json!({
            "contract": "reconcile_leaseless_orphans_and_confirm_no_daemon_owner",
            "remote_command_identity": "homeboy 0.284.1+abc123"
        }))
        .expect("prior recovery evidence");

        assert!(evidence.recovery.is_none());
    }

    #[test]
    fn failed_connect_without_recovery_omits_recovery_evidence() {
        let (report, _) = failed_connect(
            "runner",
            PathBuf::from("/session.json"),
            RunnerFailureKind::DaemonStartupFailure,
            "daemon failed".to_string(),
        );

        let serialized = serde_json::to_value(report).expect("serialize failed connect");
        assert!(serialized.get("leaseless_recovery_evidence").is_none());
    }

    #[test]
    fn leaseless_recovery_refuses_failed_or_timed_out_probe() {
        let failed =
            negotiate_leaseless_recovery_contract(&command_output(false, String::new(), false))
                .expect_err("failed probe");
        assert!(failed.contains("capability probe failed"));

        let timed_out =
            negotiate_leaseless_recovery_contract(&command_output(false, String::new(), true))
                .expect_err("timed out probe");
        assert!(timed_out.contains("timed out"));
    }

    #[test]
    fn leaseless_recovery_does_not_mutate_before_successful_negotiation() {
        let events = std::cell::RefCell::new(Vec::new());
        let result = execute_remote_leaseless_recovery(
            || {
                events.borrow_mut().push("probe");
                command_output(true, "OPTIONS:\n    --addr <ADDR>\n", false)
            },
            |_| {
                events.borrow_mut().push("recover");
                command_output(true, String::new(), false)
            },
        );

        assert!(result.is_err());
        assert_eq!(*events.borrow(), vec!["probe"]);
    }

    fn command_output(
        success: bool,
        stdout: impl Into<String>,
        timed_out: bool,
    ) -> crate::core::server::CommandOutput {
        crate::core::server::CommandOutput {
            stdout: stdout.into(),
            stderr: String::new(),
            success,
            exit_code: if success { 0 } else { 1 },
            timed_out,
            child_resource: None,
        }
    }

    fn sample_leaseless_recovery() -> DaemonLeaselessRecoveryResult {
        serde_json::from_value(serde_json::json!({
            "affected_job_ids": [],
            "affected_job_count": 0,
            "evidence_snapshot_path": "/evidence/jobs.snapshot",
            "ownership_proof": ["owner lock acquired"],
            "retry_guidance": "retry",
            "replacement": {
                "pid": 42,
                "address": "127.0.0.1:7421",
                "state_path": "/state.json",
                "lease_id": "lease-new"
            }
        }))
        .expect("sample recovery")
    }

    fn lost_local_session_refuses_unreachable_daemon_with_active_jobs() {
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        let err = remote_daemon_connect_action(None, &status).expect_err("unreachable daemon");

        assert!(err.contains("unreachable"));
    }

    #[test]
    fn orphan_adoption_command_carries_exact_lease_and_dead_pid_confirmation() {
        let command = remote_daemon_adopt_orphan_command("/opt/homeboy", "lease dead");

        assert!(command.contains("daemon adopt-orphan"));
        assert!(command.contains("--lease-id 'lease dead'"));
        assert!(command.contains("--confirm-pid-dead"));
    }

    #[test]
    fn refuses_to_replace_proven_dead_daemon_with_active_jobs_when_lease_mismatches() {
        let session = direct_ssh_session("lease-recorded");
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        let err = remote_daemon_connect_action(Some(&session), &status)
            .expect_err("refuse mismatched dead lease");

        assert!(err.contains("1 active job(s)"));
        assert!(err.contains("unreachable"));
        assert!(err.contains("1 active job(s) were not replaced"));
        assert!(err.contains("active-job recovery guidance"));
    }

    #[test]
    fn dead_recorded_daemon_routes_to_idempotent_ensure_start() {
        let status = RemoteDaemonStatus {
            daemon: None,
            stale_reason: Some("daemon lease pid is not running".to_string()),
            stale_reason_code: Some(DaemonStaleReasonCode::PidDead),
            fresh: false,
            reachable: false,
            active_jobs: 0,
            endpoint_probe_error: None,
        };

        assert_eq!(
            remote_daemon_connect_action(Some(&direct_ssh_session("lease-dead")), &status)
                .expect("ensure start"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn first_connect_routes_to_idempotent_ensure_start_when_no_daemon_exists() {
        let status = RemoteDaemonStatus {
            daemon: None,
            stale_reason: None,
            stale_reason_code: None,
            fresh: false,
            reachable: false,
            active_jobs: 0,
            endpoint_probe_error: None,
        };

        assert_eq!(
            remote_daemon_connect_action(None, &status).expect("ensure start"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn missing_daemon_state_with_active_jobs_refuses_ensure_running() {
        let status = RemoteDaemonStatus {
            daemon: None,
            stale_reason: Some("daemon state is unavailable".to_string()),
            stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
            fresh: false,
            reachable: false,
            active_jobs: 1,
            endpoint_probe_error: None,
        };

        let error = remote_daemon_connect_action(None, &status)
            .expect_err("active jobs require explicit recovery");

        assert!(error.contains("1 active job(s)"));
        assert!(error.contains("refusing ensure-running"));
        assert!(error.contains("active-job recovery guidance"));
    }

    #[test]
    fn refuses_to_replace_live_daemon_with_a_different_persisted_lease() {
        let session = direct_ssh_session("lease-recorded");
        let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 4646);

        let err =
            remote_daemon_connect_action(Some(&session), &status).expect_err("lease mismatch");

        assert!(err.contains("does not match persisted session lease"));
        assert!(err.contains("refusing to replace"));
    }

    #[test]
    fn legacy_session_adopts_consistent_live_daemon_identity() {
        let mut session = direct_ssh_session("lease-old");
        session.remote_daemon_lease_id = None;
        let status = remote_daemon_status_for_test(true, true, 1, "lease-live", 4242);

        assert_eq!(
            remote_daemon_connect_action(Some(&session), &status).expect("adopt live lease"),
            RemoteDaemonConnectAction::Reattach
        );
    }

    #[test]
    fn legacy_session_refuses_pid_reuse_with_different_daemon_identity() {
        let mut session = direct_ssh_session("lease-old");
        session.remote_daemon_lease_id = None;
        let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 5555);

        assert!(remote_daemon_connect_action(Some(&session), &status)
            .expect_err("identity mismatch")
            .contains("does not match the live daemon PID/address"));
    }

    #[test]
    fn parses_daemon_envelope_after_noisy_stdout_preamble() {
        let envelope = parse_envelope(
            "Setting up Swift test infrastructure...\nSwift unavailable; Swift extension installed but not ready...\n{\"success\":true,\"data\":{\"action\":\"start\",\"address\":\"127.0.0.1:49152\",\"pid\":123,\"lease_id\":\"lease-1\"}}\n",
        )
        .expect("parse envelope after preamble");

        assert!(envelope.success);
        assert_eq!(
            envelope
                .data
                .unwrap()
                .get("address")
                .and_then(Value::as_str),
            Some("127.0.0.1:49152")
        );
    }

    #[test]
    fn compares_cli_and_daemon_version_shapes() {
        assert!(versions_match("homeboy 0.204.0", "0.204.0"));
        assert!(versions_match("0.204.0", "homeboy 0.204.0"));
        assert!(versions_match(
            "homeboy 0.204.0+19a41cd5102d",
            "0.204.0+19a41cd5102d"
        ));
        assert!(!versions_match("homeboy 0.201.3", "homeboy 0.204.0"));
    }

    #[test]
    fn extracts_current_daemon_version_shape() {
        assert_eq!(
            daemon_version_from_body(&serde_json::json!({"version":"0.204.0"})),
            Some("0.204.0")
        );
        assert_eq!(
            daemon_version_from_body(&serde_json::json!({
                "success": true,
                "data": {"version": "0.281.2"}
            })),
            Some("0.281.2")
        );
        assert_eq!(
            daemon_identity_from_body(
                &serde_json::json!({"version":"0.228.13","build_identity":{"display":"homeboy 0.228.13+f7569a5e"}})
            ),
            Some("homeboy 0.228.13+f7569a5e")
        );
        assert_eq!(
            daemon_identity_from_body(&serde_json::json!({
                "success": true,
                "data": {
                    "build_identity": {"display": "homeboy 0.281.2+b078972b3edd"}
                }
            })),
            Some("homeboy 0.281.2+b078972b3edd")
        );
        assert_eq!(
            daemon_identity_from_body(&serde_json::json!({"version":"0.228.13"})),
            None
        );
    }

    #[test]
    fn parses_self_identity_json_envelope() {
        let identity = parse_self_identity_output(
            r#"{"success":true,"data":{"version":"0.228.13","display":"homeboy 0.228.13+19a41cd5102d"}}"#,
        )
        .expect("identity");

        assert_eq!(identity.version, "0.228.13");
        assert_eq!(identity.display, "homeboy 0.228.13+19a41cd5102d");
    }

    #[test]
    fn stale_daemon_warning_includes_ordered_restart_recovery_commands() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.201.3".to_string(),
            "homeboy 0.204.0".to_string(),
            Some("homeboy 0.201.3+old".to_string()),
            Some("homeboy 0.204.0+new".to_string()),
        );

        assert_eq!(warning.session_homeboy_version, "homeboy 0.201.3");
        assert_eq!(warning.current_homeboy_version, "homeboy 0.204.0");
        assert_eq!(warning.severity, "warning");
        assert_eq!(
            warning.active_daemon_control_plane_version,
            "homeboy 0.201.3"
        );
        assert_eq!(warning.job_command_binary_version, "homeboy 0.204.0");
        assert_eq!(
            warning.session_homeboy_build_identity.as_deref(),
            Some("homeboy 0.201.3+old")
        );
        assert_eq!(
            warning
                .active_daemon_control_plane_build_identity
                .as_deref(),
            Some("homeboy 0.201.3+old")
        );
        assert_eq!(
            warning.job_command_binary_build_identity.as_deref(),
            Some("homeboy 0.204.0+new")
        );
        assert_eq!(
            warning.refresh_command,
            format!(
                "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect && homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(warning.message.contains("daemon control plane"));
        assert!(warning.message.contains("job command binary"));
        assert!(warning.message.contains("active jobs are drained"));
        assert_eq!(
            warning.recovery_commands,
            vec![
                format!(
                    "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect",
                    env!("CARGO_PKG_VERSION")
                ),
                "homeboy runner disconnect homeboy-lab".to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            ]
        );
    }

    #[test]
    fn parses_daemon_runtime_stale_paths_from_version_body() {
        let paths = daemon_runtime_stale_paths_from_body(&serde_json::json!({
            "version": "0.228.13",
            "runtime_paths": {
                "stale": [{
                    "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                    "path": "/home/chubes/Developer/sample-runtime",
                    "loaded_fingerprint": "files=10",
                    "current_fingerprint": "files=11"
                }]
            }
        }));

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
        assert_eq!(paths[0].loaded_fingerprint, "files=10");
        assert_eq!(paths[0].current_fingerprint, "files=11");
    }

    #[test]
    fn changed_runtime_paths_reports_runner_config_changes_since_daemon_start() {
        let mut runner_env = HashMap::new();
        runner_env.insert(
            "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
            "/home/chubes/Developer/sample-runtime@new".to_string(),
        );
        let loaded = daemon_runtime_loaded_paths_from_body(&serde_json::json!({
            "runtime_paths": {
                "loaded": [{
                    "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                    "path": "/home/chubes/Developer/sample-runtime@old",
                    "fingerprint": "files=10"
                }]
            }
        }));

        let changed = changed_runtime_paths(&runner_env, &loaded);

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
        assert_eq!(
            changed[0].loaded_path.as_deref(),
            Some("/home/chubes/Developer/sample-runtime@old")
        );
        assert_eq!(
            changed[0].configured_path.as_deref(),
            Some("/home/chubes/Developer/sample-runtime@new")
        );
    }

    #[test]
    fn runtime_path_warning_uses_rebuild_specific_message() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.228.13".to_string(),
            "0.228.13".to_string(),
            Some("homeboy 0.228.13+same".to_string()),
            Some("homeboy 0.228.13+same".to_string()),
        )
        .with_runtime_paths(
            "homeboy-lab",
            vec![RunnerStaleRuntimePath {
                env: "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
                path: "/home/chubes/Developer/sample-runtime".to_string(),
                loaded_fingerprint: "files=10".to_string(),
                current_fingerprint: "files=11".to_string(),
            }],
            Vec::new(),
        );

        assert!(warning.message.contains("runner-side rebuilds"));
        assert_eq!(
            warning.recovery_commands,
            vec![
                "homeboy runner disconnect homeboy-lab".to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            ]
        );
    }

    #[test]
    fn parses_remote_daemon_status_lease_as_single_source_of_truth() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{"action":"status","running":true,"fresh":true,"reachable":true,"state":{"lease_id":"lease-1","address":"127.0.0.1:49152","pid":123}}}"#,
        )
        .expect("parse envelope");
        let data = envelope.data.expect("status data");
        let state = data.get("state").expect("lease state");

        assert!(data.get("running").and_then(Value::as_bool).unwrap());
        assert_eq!(
            state.get("lease_id").and_then(Value::as_str),
            Some("lease-1")
        );
        assert_eq!(
            state.get("address").and_then(Value::as_str),
            Some("127.0.0.1:49152")
        );
    }

    #[test]
    fn remote_daemon_disconnect_command_guards_recorded_pid() {
        let command = remote_daemon_disconnect_command("/opt/homeboy", Some(42));

        assert!(command.contains("/opt/homeboy daemon stop"));
        assert!(command.contains("pid=42"));
        assert!(command.contains("/proc/$pid/cmdline"));
        assert!(command.contains("*\" daemon serve \"*)"));
        assert!(command.contains("kill -TERM \"$pid\""));
        assert!(command.contains("kill -KILL \"$pid\""));
    }

    #[test]
    fn test_open_loopback_tunnel_noops_for_local_runner() {
        let server = Server {
            id: "local".to_string(),
            aliases: Vec::new(),
            host: "127.0.0.1".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        };

        let tunnel = open_loopback_tunnel(&server, 49100, "127.0.0.1", 49200);

        assert!(tunnel.success);
        assert_eq!(tunnel.pid, None);
        assert_eq!(tunnel.stderr, "");
    }

    #[test]
    fn connect_reports_local_runner_as_unsupported() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");

            let (report, exit_code) = connect("lab-local").expect("connect report");

            assert_eq!(exit_code, 20);
            assert!(!report.connected);
            assert_eq!(report.failure_kind, Some(RunnerFailureKind::SshFailure));
            assert!(report
                .failure_message
                .as_deref()
                .unwrap_or_default()
                .contains("only SSH runners"));
        });
    }

    #[test]
    fn disconnect_removes_existing_session_file() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");
            let session = RunnerSession {
                runner_id: "lab-local".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: None,
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:49152".to_string()),
                local_port: Some(49153),
                local_url: Some("http://127.0.0.1:49153".to_string()),
                tunnel_pid: None,
                remote_daemon_pid: None,
                remote_daemon_lease_id: None,
                homeboy_version: "test".to_string(),
                homeboy_build_identity: Some("homeboy test+abc123".to_string()),
                connected_at: Utc::now().to_rfc3339(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
                leaseless_recovery_evidence: None,
            };
            write_session(&session).expect("write session");
            let path = session_path("lab-local").expect("session path");
            assert!(path.exists());

            let report = disconnect("lab-local").expect("disconnect");

            assert!(report.disconnected);
            assert_eq!(report.session.expect("session").runner_id, "lab-local");
            assert!(!path.exists());
        });
    }

    #[test]
    fn records_reverse_runner_session_without_marking_transport_live() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{"id":"homeboy-lab","kind":"local","workspace_root":"/home/user/Developer"}"#,
                false,
            )
            .expect("create runner");

            let (report, exit_code) = connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            assert_eq!(exit_code, 0);
            assert!(!report.connected);
            assert_eq!(report.recorded, Some(true));
            assert_eq!(report.mode, Some(RunnerTunnelMode::Reverse));
            assert_eq!(report.role, Some(RunnerSessionRole::Runner));
            assert_eq!(report.controller_id.as_deref(), Some("extra-chill"));

            let status = status("homeboy-lab").expect("status");
            assert!(!status.connected);
            assert_eq!(status.state, RunnerSessionState::Recorded);
            let session = status.session.expect("session");
            assert_eq!(session.mode, RunnerTunnelMode::Reverse);
            assert_eq!(session.role, RunnerSessionRole::Runner);
            assert_eq!(session.controller_id.as_deref(), Some("extra-chill"));
            assert_eq!(session.broker_url, None);
            assert_eq!(session.local_url, None);
            assert_eq!(session.local_port, None);
        });
    }

    #[test]
    fn status_lists_reverse_session_records() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
                .expect("create runner");
            connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            let reports = statuses().expect("statuses");
            let report = reports
                .iter()
                .find(|report| report.runner_id == "homeboy-lab")
                .expect("homeboy-lab status");

            assert_eq!(report.state, RunnerSessionState::Recorded);
        });
    }

    #[test]
    fn status_marks_connected_reverse_active_jobs_unavailable_without_broker_url() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
                .expect("create runner");
            let mut session = reverse_controller_session();
            session.broker_url = None;
            write_session(&session).expect("write session");

            let report = status("homeboy-lab").expect("status");

            assert!(report.connected);
            assert_eq!(report.active_job_count, 0);
            assert_eq!(report.active_jobs, Vec::new());
            assert_eq!(report.active_runner_jobs, Vec::new());
            assert_eq!(report.active_job_state, RunnerActiveJobState::Unavailable);
            assert_eq!(report.active_job_source, None);
            let error = report.active_job_error.expect("active job error");
            assert_eq!(error.code, "internal.unexpected");
            assert!(error.message.contains("no broker URL"));
        });
    }

    #[test]
    fn child_run_matching_accepts_durable_run_id_or_command_reference() {
        let run = sample_run_summary("run-child-1");
        let by_durable_id = sample_active_job(Some("run-child-1"), "homeboy test wpcom");
        let by_command = sample_active_job(None, "homeboy test wpcom --run run-child-1");
        let unrelated = sample_active_job(Some("other-run"), "homeboy test wpcom");

        assert!(child_run_has_active_job(&run, &[by_durable_id]));
        assert!(child_run_has_active_job(&run, &[by_command]));
        assert!(!child_run_has_active_job(&run, &[unrelated]));
    }

    #[test]
    fn orphaned_child_run_job_reports_stale_retryable_runner_state() {
        let job = orphaned_child_run_job("homeboy-lab", sample_run_summary("run-child-1"));

        assert_eq!(job.runner_id, "homeboy-lab");
        assert_eq!(job.job_id, "orphaned-child-run-run-child-1");
        assert_eq!(job.source, "runner-observation");
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.durable_run_id.as_deref(), Some("run-child-1"));
        assert_eq!(
            job.stale_reason.as_deref(),
            Some("child_run_running_without_active_runner_job")
        );
        assert_eq!(job.lifecycle_state.as_deref(), Some("recoverable_orphan"));
        assert_eq!(job.retryable, Some(true));
    }

    #[test]
    fn synthetic_active_job_run_summaries_are_not_child_runs() {
        let mut synthetic = sample_run_summary("runner-job-job-1");
        synthetic.status_note = Some(
            "active runner job: source=direct-daemon kind=test runner=homeboy-lab".to_string(),
        );

        assert!(is_synthetic_active_job_run_summary(&synthetic));
        assert!(!is_synthetic_active_job_run_summary(&sample_run_summary(
            "run-child-1"
        )));
    }

    #[test]
    fn active_runner_job_source_maps_direct_and_reverse_endpoints() {
        let mut direct = reverse_controller_session();
        direct.mode = RunnerTunnelMode::DirectSsh;
        direct.role = RunnerSessionRole::Controller;
        direct.local_url = Some("http://127.0.0.1:49153".to_string());
        direct.broker_url = None;
        assert_eq!(
            active_runner_job_source(&direct),
            Some(RunnerActiveJobSource::DirectDaemon)
        );

        let reverse = reverse_controller_session();
        assert_eq!(
            active_runner_job_source(&reverse),
            Some(RunnerActiveJobSource::ReverseBroker)
        );
    }

    #[test]
    fn reverse_controller_session_requires_fresh_heartbeat() {
        let mut session = reverse_controller_session();

        assert_eq!(session_state(Some(&session)), RunnerSessionState::Connected);

        session.last_seen_at = Some((Utc::now() - chrono::Duration::seconds(120)).to_rfc3339());
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);

        session.last_seen_at = None;
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);
    }

    fn reverse_controller_session() -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("extra-chill".to_string()),
            broker_url: Some("http://127.0.0.1:9876".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: Some("worker-1".to_string()),
            worker_pid: Some(1234),
            last_seen_at: Some(Utc::now().to_rfc3339()),
            leaseless_recovery_evidence: None,
        }
    }

    fn sample_run_summary(id: &str) -> RunSummary {
        RunSummary {
            id: id.to_string(),
            kind: "test".to_string(),
            status: "running".to_string(),
            started_at: "2026-07-03T13:00:00Z".to_string(),
            finished_at: None,
            component_id: Some("wpcom".to_string()),
            rig_id: None,
            git_sha: None,
            command: Some("homeboy test wpcom".to_string()),
            cwd: Some("/workspace/wpcom".to_string()),
            status_note: None,
        }
    }

    fn sample_active_job(durable_run_id: Option<&str>, command: &str) -> ActiveRunnerJobSummary {
        ActiveRunnerJobSummary {
            runner_id: "homeboy-lab".to_string(),
            job_id: "job-1".to_string(),
            operation: "runner.exec".to_string(),
            source: "direct-daemon".to_string(),
            kind: "test".to_string(),
            status: JobStatus::Running,
            command: command.to_string(),
            cwd: Some("/workspace/wpcom".to_string()),
            started_at_ms: 0,
            updated_at_ms: 0,
            elapsed_ms: 0,
            heartbeat_age_ms: 0,
            claim: JobClaimMetadata {
                claim_id: None,
                claimed_by_runner_id: Some("homeboy-lab".to_string()),
                claimed_at_ms: None,
                claim_expires_at_ms: None,
            },
            claim_expires_in_ms: None,
            lifecycle: None,
            durable_run_id: durable_run_id.map(str::to_string),
            stale_reason: None,
            lifecycle_state: Some("running".to_string()),
            retryable: Some(false),
            active_child_count: None,
            active_cell_count: None,
        }
    }

    fn direct_ssh_session(lease_id: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("homeboy-lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:49152".to_string()),
            local_port: Some(49153),
            local_url: Some("http://127.0.0.1:49153".to_string()),
            tunnel_pid: Some(1234),
            remote_daemon_pid: Some(4242),
            remote_daemon_lease_id: Some(lease_id.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn remote_daemon_status_for_test(
        fresh: bool,
        reachable: bool,
        active_jobs: usize,
        lease_id: &str,
        pid: u32,
    ) -> RemoteDaemonStatus {
        remote_daemon_status_for_test_with_reason(
            fresh,
            reachable,
            active_jobs,
            lease_id,
            pid,
            None,
        )
    }

    fn remote_daemon_status_for_test_with_reason(
        fresh: bool,
        reachable: bool,
        active_jobs: usize,
        lease_id: &str,
        pid: u32,
        stale_reason_code: Option<DaemonStaleReasonCode>,
    ) -> RemoteDaemonStatus {
        RemoteDaemonStatus {
            daemon: Some(RemoteDaemon {
                address: "127.0.0.1:49152".to_string(),
                pid: Some(pid),
                lease_id: Some(lease_id.to_string()),
                version: None,
                build_identity: None,
                inspected_freshness: None,
            }),
            stale_reason: (!fresh).then(|| "daemon is stale".to_string()),
            stale_reason_code,
            fresh,
            reachable,
            active_jobs,
            endpoint_probe_error: None,
        }
    }

    #[test]
    fn sessionless_active_daemon_reattaches_only_with_matching_endpoint_identity() {
        let mut status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);
        let daemon = status.daemon.as_mut().expect("daemon");
        daemon.version = Some("0.284.0".to_string());
        daemon.build_identity = Some("homeboy 0.284.0+live".to_string());

        assert_eq!(
            remote_daemon_connect_action_with_controller_identity(
                None,
                &status,
                "homeboy 0.284.0+live"
            )
            .expect("matching controller reattaches"),
            RemoteDaemonConnectAction::Reattach
        );

        let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);
        assert_eq!(recovery.daemon_version.as_deref(), Some("0.284.0"));
        assert_eq!(
            recovery.daemon_build_identity.as_deref(),
            Some("homeboy 0.284.0+live")
        );
    }

    #[test]
    fn sessionless_active_daemon_prescribes_matching_pinned_controller_on_identity_mismatch() {
        let mut status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);
        let daemon = status.daemon.as_mut().expect("daemon");
        daemon.version = Some("0.284.0".to_string());
        daemon.build_identity = Some("homeboy 0.284.0+live".to_string());

        let error = remote_daemon_connect_action_with_controller_identity(
            None,
            &status,
            "homeboy 0.284.0+other",
        )
        .expect_err("mismatched controller must not replace an active daemon");

        assert!(error.contains("Run a controller pinned to `homeboy 0.284.0+live`"));
        assert!(error.contains("refusing replacement"));
    }

    #[test]
    fn sessionless_active_daemon_fails_closed_when_endpoint_identity_is_ambiguous() {
        let status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);

        let error = remote_daemon_connect_action_with_controller_identity(
            None,
            &status,
            "homeboy 0.284.0+live",
        )
        .expect_err("missing endpoint identity must not authorize reattachment");

        assert!(error.contains("did not provide a build identity"));
    }
}
