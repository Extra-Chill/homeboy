use super::*;
use crate::daemon::{DaemonFreshnessReport, DaemonRecoveryEvidence};
use std::time::Duration;

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

    let mut args = crate::server::ssh_args::server_option_args(
        server,
        crate::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            exit_on_forward_failure: true,
            port_flag: Some(crate::server::ssh_args::SshPortFlag::Lowercase),
            ..crate::server::ssh_args::SshArgOptions::default()
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
    pub(super) termination_evidence: Option<crate::daemon::DaemonTerminationEvidence>,
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
}

pub(super) fn ensure_remote_daemon(
    client: &SshClient,
    homeboy: &str,
    previous_session: Option<&RunnerSession>,
    orphan_lease_id: Option<&str>,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
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
    match remote_daemon_connect_action_with_controller_identity(
        previous_session,
        &status,
        &crate::build_identity::current().display,
    )? {
        RemoteDaemonConnectAction::Reattach => {
            let mut daemon = status.daemon.ok_or_else(|| {
                "remote daemon reattach selected without a daemon lease".to_string()
            })?;
            daemon.inspected_freshness = Some(inspected_freshness);
            return Ok(daemon);
        }
        RemoteDaemonConnectAction::Start => return remote_daemon_ensure_running(client, homeboy),
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
        &crate::build_identity::current().display,
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
    if let Some(session) = previous_session.filter(|session| {
        session.mode == RunnerTunnelMode::DirectSsh && session.role == RunnerSessionRole::Controller
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
