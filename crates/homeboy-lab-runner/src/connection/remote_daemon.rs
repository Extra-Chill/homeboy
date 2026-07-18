use super::*;
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonRecoveryEvidence};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(super) const REMOTE_DAEMON_STATUS_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn resolve_ssh_runner(runner: &Runner) -> Result<Option<(String, Server, SshClient)>> {
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

    let mut args = homeboy_core::server::ssh_args::server_option_args(
        server,
        homeboy_core::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            exit_on_forward_failure: true,
            port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Lowercase),
            ..homeboy_core::server::ssh_args::SshArgOptions::default()
        },
    );
    args.extend([
        "-N".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{}:{}:{}", local_port, remote_host, remote_port),
        format!("{}@{}", server.user, server.host),
    ]);

    let mut command = std::process::Command::new("ssh");
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = spawn_tunnel_process(&mut command);
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

pub(super) fn spawn_tunnel_process(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    command.spawn()
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
    /// The typed `/jobs` view is independently required before replacing a
    /// reachable stale daemon. Missing or malformed evidence fails closed.
    pub(super) work_evidence: RemoteDaemonWorkEvidence,
    pub(super) endpoint_probe_error: Option<String>,
    pub(super) termination_evidence: Option<homeboy_core::daemon::DaemonTerminationEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteDaemonWorkEvidence {
    Unknown,
    ActiveOrUnresolved(usize),
    AuthoritativelyIdle,
}

impl RemoteDaemonWorkEvidence {
    fn from_unresolved_count(count: usize) -> Self {
        if count == 0 {
            Self::AuthoritativelyIdle
        } else {
            Self::ActiveOrUnresolved(count)
        }
    }

    fn is_authoritatively_idle(self) -> bool {
        self == Self::AuthoritativelyIdle
    }
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
    let leaseless_reconciliation_available = status.active_jobs > 0
        && matches!(
            status.stale_reason_code,
            Some(
                DaemonStaleReasonCode::LeaseMissing
                    | DaemonStaleReasonCode::LeaseCorrupt
                    | DaemonStaleReasonCode::VersionMismatch
            )
        );
    let mut ownership_evidence = if proven_dead {
        Some(format!(
            "remote daemon status over SSH proved PID {} is dead for lease `{}`",
            pid.expect("proven dead PID"),
            lease_id.as_deref().expect("proven dead lease")
        ))
    } else if leaseless_reconciliation_available {
        Some("active durable jobs require explicit reconciliation; it will verify the owner lock, process list, and configured listener before terminalizing them".to_string())
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
    let adoption_command = if proven_dead {
        Some(format!(
            "homeboy runner connect {} --adopt-orphan-lease {} --confirm-pid-dead",
            shell::quote_arg(runner_id),
            shell::quote_arg(lease_id.as_deref().expect("proven dead lease"))
        ))
    } else if leaseless_reconciliation_available {
        Some(format!(
            "homeboy runner connect {} --reconcile-leaseless-orphans --confirm-no-daemon-owner",
            shell::quote_arg(runner_id),
        ))
    } else {
        None
    };
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
        termination_evidence: status.termination_evidence.clone(),
        repair_plan: Vec::new(),
    }
}

pub(super) fn unavailable_recovery_freshness(error: impl Into<String>) -> DaemonFreshnessReport {
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
        termination_evidence: None,
        repair_plan: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RemoteDaemonConnectAction {
    Reattach,
    Start,
    ReplaceIdleStale,
}

pub(super) fn ensure_remote_daemon(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
    previous_session: Option<&RunnerSession>,
    configured_identity: &str,
    orphan_lease_id: Option<&str>,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
    live_lease_expectation: Option<(&str, u32)>,
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
            return remote_daemon_adopt_orphan(client, homeboy, lease_id, confirmed_no_pid_job_ids);
        }
    }
    if !confirmed_no_pid_job_ids.is_empty() {
        return Err(
            "--confirm-untracked-child-dead applies only when the remote daemon reports the exact requested lease as PID-dead"
                .to_string(),
        );
    }
    let inspected_freshness = remote_daemon_recovery_freshness_from_status("<runner-id>", &status);
    match remote_daemon_connect_action_for_runner(
        previous_session,
        &status,
        configured_identity,
        runner_id,
        live_lease_expectation,
    )? {
        RemoteDaemonConnectAction::Reattach => {
            let mut daemon = status.daemon.ok_or_else(|| {
                "remote daemon reattach selected without a daemon lease".to_string()
            })?;
            daemon.inspected_freshness = Some(inspected_freshness);
            return Ok(daemon);
        }
        RemoteDaemonConnectAction::Start => return remote_daemon_ensure_running(client, homeboy),
        RemoteDaemonConnectAction::ReplaceIdleStale => {
            let daemon = status.daemon.as_ref().expect("replacement requires daemon");
            let lease_id = daemon
                .lease_id
                .as_deref()
                .expect("replacement requires lease");
            remote_daemon_force_stop(client, homeboy, lease_id)?;
            let replacement = remote_daemon_ensure_running(client, homeboy)?;
            return verify_remote_daemon_replacement(
                client,
                homeboy,
                &replacement,
                configured_identity,
            );
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
        &homeboy_product_identity::build_identity().display,
    )
}

pub(super) fn remote_daemon_connect_action_with_controller_identity(
    previous_session: Option<&RunnerSession>,
    status: &RemoteDaemonStatus,
    expected_identity: &str,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    remote_daemon_connect_action_for_runner(
        previous_session,
        status,
        expected_identity,
        "<runner-id>",
        None,
    )
}

pub(super) fn remote_daemon_connect_action_for_runner(
    previous_session: Option<&RunnerSession>,
    status: &RemoteDaemonStatus,
    expected_identity: &str,
    runner_id: &str,
    live_lease_expectation: Option<(&str, u32)>,
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

    // `daemon status` proves this exact recorded PID is no longer running.
    // With no durable work left to reconcile, ensure-running can safely replace
    // the stale lease without requiring another orphan-adoption cycle.
    if status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead) && status.active_jobs == 0 {
        return Ok(RemoteDaemonConnectAction::Start);
    }

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
    // A lease-less freshness report ordinarily prevents replacement. The one
    // bounded exception is an idle daemon whose identity differs from the
    // configured executable and whose typed `/jobs` endpoint independently
    // proves no active or unresolved work. The stop itself remains lease-bound
    // through Homeboy's daemon lifecycle command.
    if !status.fresh {
        if status.active_jobs == 0
            && status.work_evidence.is_authoritatively_idle()
            && status.endpoint_probe_error.is_none()
            && daemon
                .build_identity
                .as_deref()
                .is_some_and(|identity| identity.trim() != expected_identity.trim())
        {
            return Ok(RemoteDaemonConnectAction::ReplaceIdleStale);
        }
        return reattach_only_if_same_lease(previous_session, daemon, runner_id, status);
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
            if daemon_identity.trim() != expected_identity.trim() {
                return Err(format!(
                    "remote daemon has {} active job(s) under reachable lease `{}` (PID {}) but build identity `{daemon_identity}` / version `{daemon_version}` does not match this configured runner binary `{expected_identity}`; refusing replacement. Run a controller pinned to `{daemon_identity}` and retry `homeboy runner connect <runner-id>` to reattach this exact lease.",
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
                if live_lease_expectation
                    == Some((actual_lease, daemon.pid.expect("checked above")))
                    && daemon.build_identity.as_deref().map(str::trim)
                        == Some(expected_identity.trim())
                    && status.endpoint_probe_error.is_none()
                {
                    return Ok(RemoteDaemonConnectAction::Reattach);
                }
                return Err(lease_reconciliation_failure(
                    expected_lease,
                    actual_lease,
                    daemon,
                    status,
                    expected_identity,
                    runner_id,
                ));
            }
        } else if live_lease_expectation
            != Some((
                daemon.lease_id.as_deref().expect("checked above"),
                daemon.pid.expect("checked above"),
            ))
            || daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim())
            || status.endpoint_probe_error.is_some()
        {
            return Err(lease_reconciliation_failure(
                "none or corrupt",
                daemon.lease_id.as_deref().expect("checked above"),
                daemon,
                status,
                expected_identity,
                runner_id,
            ));
        }
        return Ok(RemoteDaemonConnectAction::Reattach);
    }

    Ok(RemoteDaemonConnectAction::Start)
}

fn reattach_only_if_same_lease(
    previous_session: Option<&RunnerSession>,
    daemon: &RemoteDaemon,
    runner_id: &str,
    status: &RemoteDaemonStatus,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    let persisted_lease = previous_session
        .filter(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
        })
        .and_then(|session| session.remote_daemon_lease_id.as_deref());
    if persisted_lease == daemon.lease_id.as_deref() {
        Ok(RemoteDaemonConnectAction::Reattach)
    } else {
        Err(lease_reconciliation_failure(
            persisted_lease.unwrap_or("none or corrupt"),
            daemon.lease_id.as_deref().unwrap_or("unavailable"),
            daemon,
            status,
            "not evaluated for stale daemon",
            runner_id,
        ))
    }
}

fn lease_reconciliation_failure(
    expected_lease: &str,
    actual_lease: &str,
    daemon: &RemoteDaemon,
    status: &RemoteDaemonStatus,
    expected_identity: &str,
    runner_id: &str,
) -> String {
    format!(
        "live remote daemon lease `{actual_lease}` differs from persisted session lease `{expected_lease}`; refusing to adopt or replace it because runner ownership is not proven (fresh={}, reachable={}, live identity `{}`, configured identity `{}`, endpoint probe `{}`). No session state was changed. Run `homeboy runner connect {} --adopt-live-lease {} --expected-live-pid {}` to explicitly adopt this observed lease/PID/build after revalidation. This is operator-confirmed recovery within the trusted remote SSH UID boundary; it never stops or replaces a daemon, and later lease drift fails closed. Run `homeboy runner status {} --json` to inspect it.",
        status.fresh,
        status.reachable,
        daemon.build_identity.as_deref().unwrap_or("unavailable"),
        expected_identity,
        status.endpoint_probe_error.as_deref().unwrap_or("verified"),
        shell::quote_arg(runner_id),
        shell::quote_arg(actual_lease),
        daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
        shell::quote_arg(runner_id),
    )
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
    let termination_evidence = data
        .get("termination_evidence")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
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
            work_evidence: RemoteDaemonWorkEvidence::Unknown,
            endpoint_probe_error: None,
            termination_evidence,
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
            work_evidence: RemoteDaemonWorkEvidence::Unknown,
            endpoint_probe_error: None,
            termination_evidence,
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
        work_evidence: RemoteDaemonWorkEvidence::Unknown,
        endpoint_probe_error: None,
        termination_evidence,
    })
}

pub(super) fn probe_remote_daemon_endpoint(client: &SshClient, status: &mut RemoteDaemonStatus) {
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
    let command = format!(
        "curl --fail --silent --show-error --max-time 2 {}/jobs",
        shell::quote_arg(&format!("http://{}", daemon.address))
    );
    let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
    if !output.success {
        status.endpoint_probe_error = Some(command_failure_message(
            "remote daemon typed job probe failed",
            &output,
        ));
        return;
    }
    let body: Value = match parse_json_from_mixed_stdout(&output.stdout) {
        Ok(body) => body,
        Err(error) => {
            status.endpoint_probe_error = Some(format!(
                "remote daemon typed job probe returned invalid JSON: {error}"
            ));
            return;
        }
    };
    let jobs = body
        .pointer("/data/body")
        .or_else(|| body.get("body"))
        .unwrap_or(&body);
    let Some(active) = jobs.get("active_runner_jobs").and_then(Value::as_array) else {
        status.endpoint_probe_error =
            Some("remote daemon typed job probe did not return active_runner_jobs".to_string());
        return;
    };
    let Some(stale) = jobs.get("stale_runner_jobs").and_then(Value::as_array) else {
        status.endpoint_probe_error =
            Some("remote daemon typed job probe did not return stale_runner_jobs".to_string());
        return;
    };
    status.work_evidence =
        RemoteDaemonWorkEvidence::from_unresolved_count(active.len().saturating_add(stale.len()));
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

pub(super) fn remote_daemon_force_stop(
    client: &SshClient,
    homeboy: &str,
    lease_id: &str,
) -> std::result::Result<(), String> {
    let command = format!(
        "{} daemon stop --force --lease-id {}",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
    );
    let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
    if !output.success {
        return Err(command_failure_message(
            "remote bounded stale-daemon replacement stop failed",
            &output,
        ));
    }
    let envelope = parse_envelope(&output.stdout).map_err(|error| {
        format!("remote bounded stale-daemon replacement stop returned invalid JSON: {error}")
    })?;
    if !envelope.success {
        return Err(
            "remote bounded stale-daemon replacement stop returned an error envelope".to_string(),
        );
    }
    if envelope
        .data
        .as_ref()
        .and_then(|data| data.get("action"))
        .and_then(Value::as_str)
        != Some("stop")
    {
        return Err(
            "remote bounded stale-daemon replacement stop returned an unexpected response"
                .to_string(),
        );
    }
    Ok(())
}

fn verify_remote_daemon_replacement(
    client: &SshClient,
    homeboy: &str,
    replacement: &RemoteDaemon,
    configured_identity: &str,
) -> std::result::Result<RemoteDaemon, String> {
    let mut status = remote_daemon_status(client, homeboy)?;
    probe_remote_daemon_endpoint(client, &mut status);
    let daemon = status.daemon.ok_or_else(|| {
        "remote stale-daemon replacement re-probe returned no daemon state".to_string()
    })?;
    if !status.fresh || !status.reachable {
        return Err(
            "remote stale-daemon replacement re-probe did not prove a fresh reachable daemon"
                .to_string(),
        );
    }
    if status.endpoint_probe_error.is_some() {
        return Err(format!(
            "remote stale-daemon replacement endpoint re-probe failed: {}",
            status.endpoint_probe_error.unwrap_or_default()
        ));
    }
    if daemon.lease_id != replacement.lease_id
        || daemon.pid != replacement.pid
        || daemon.address != replacement.address
    {
        return Err(
            "remote stale-daemon replacement ownership changed before re-probe; refusing to persist a different daemon"
                .to_string(),
        );
    }
    if daemon.build_identity.as_deref().map(str::trim) != Some(configured_identity.trim()) {
        return Err(format!(
            "remote stale-daemon replacement identity `{}` does not match configured runner binary `{}`",
            daemon.build_identity.as_deref().unwrap_or("unavailable"),
            configured_identity,
        ));
    }
    Ok(daemon)
}

fn remote_daemon_adopt_orphan(
    client: &SshClient,
    homeboy: &str,
    lease_id: &str,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
) -> std::result::Result<RemoteDaemon, String> {
    let command = remote_daemon_adopt_orphan_command(homeboy, lease_id, confirmed_no_pid_job_ids);
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
    let replacement = data
        .get("replacement")
        .ok_or_else(|| "remote daemon orphan adoption returned no replacement lease".to_string())?;
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

pub(super) fn remote_daemon_adopt_orphan_command(
    homeboy: &str,
    lease_id: &str,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
) -> String {
    let confirmations = confirmed_no_pid_job_ids
        .iter()
        .map(|job_id| format!(" --confirm-untracked-child-dead {job_id}"))
        .collect::<String>();
    format!(
        "{} daemon adopt-orphan --lease-id {} --confirm-pid-dead{} --addr 127.0.0.1:0",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
        confirmations,
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
                let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
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
