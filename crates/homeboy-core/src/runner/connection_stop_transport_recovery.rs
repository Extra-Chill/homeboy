use std::time::Duration;

use serde_json::Value;

use super::*;

const REMOTE_LEASE_BOUND_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Capture the recorded session before a binary promotion changes runner
/// configuration. The caller can later require this remote daemon owner.
pub(crate) fn recorded_session(runner_id: &str) -> Result<Option<RunnerSession>> {
    read_session(runner_id)
}

pub(crate) fn disconnect_with_force(
    runner_id: &str,
    force: bool,
) -> Result<RunnerDisconnectReport> {
    disconnect_with_session(runner_id, None, force)
}

/// Stop the daemon through the current live session after confirming it still
/// owns the remote daemon observed by a caller's promotion transaction.
pub(crate) fn disconnect_with_session(
    runner_id: &str,
    expected_session: Option<&RunnerSession>,
    force: bool,
) -> Result<RunnerDisconnectReport> {
    let promotion_lease =
        crate::runtime_promotion::acquire("runner daemon disconnect", runner_id.to_string())?;
    promotion_lease.assert_generation()?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    if let Some(expected_session) = expected_session {
        if !session.as_ref().is_some_and(|current_session| {
            same_remote_daemon_ownership(runner_id, expected_session, current_session)
        }) {
            return Err(Error::validation_invalid_argument(
                "disconnect",
                format!(
                    "runner `{runner_id}` remote daemon ownership changed during refresh; refusing to stop a different daemon"
                ),
                Some(runner_id.to_string()),
                Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
            ));
        }
    }
    if let Some(session) = &session {
        if session.mode == RunnerTunnelMode::DirectSsh {
            disconnect_remote_daemon(session, force).map_err(|err| {
                Error::validation_invalid_argument(
                    "disconnect",
                    format!("refusing to disconnect runner `{runner_id}` because its remote daemon was not stopped safely: {err}"),
                    Some(runner_id.to_string()),
                    Some(vec![format!("homeboy runner status {}", shell::quote_arg(runner_id))]),
                )
            })?;
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

fn same_remote_daemon_ownership(
    runner_id: &str,
    expected: &RunnerSession,
    current: &RunnerSession,
) -> bool {
    expected.runner_id == runner_id
        && current.runner_id == runner_id
        && expected.mode == RunnerTunnelMode::DirectSsh
        && current.mode == RunnerTunnelMode::DirectSsh
        && expected.role == RunnerSessionRole::Controller
        && current.role == RunnerSessionRole::Controller
        && expected.server_id == current.server_id
        && expected.remote_daemon_address == current.remote_daemon_address
        && expected.remote_daemon_lease_id == current.remote_daemon_lease_id
        && expected.remote_daemon_pid == current.remote_daemon_pid
}

pub(super) fn disconnect_remote_daemon(
    session: &RunnerSession,
    force: bool,
) -> std::result::Result<(), String> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        "direct SSH runner session has no live daemon tunnel; refusing unbound remote stop"
            .to_string()
    })?;
    let lease_id = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        "direct SSH runner session has no daemon lease; refusing unbound remote stop".to_string()
    })?;
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("build daemon lifecycle client: {error}"))?;
    let response = match client
        .post(format!(
            "{}/lifecycle/stop",
            local_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({ "lease_id": lease_id, "force": force }))
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return recover_remote_daemon_stop_after_transport_error(
                session,
                &format!("request lease-bound daemon stop: {error}"),
            )
        }
    };
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("read lease-bound daemon stop response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "lease-bound daemon stop was refused with HTTP {}: {}",
            status.as_u16(),
            response_body_excerpt(&body)
        ));
    }
    verify_remote_daemon_stopped(session)
}

fn response_body_excerpt(body: &str) -> String {
    const LIMIT: usize = 2_000;
    let trimmed = body.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_string();
    }
    format!(
        "{}...<truncated>",
        trimmed.chars().take(LIMIT).collect::<String>()
    )
}

pub(super) fn recover_remote_daemon_stop_after_transport_error(
    session: &RunnerSession,
    transport_error: &str,
) -> std::result::Result<(), String> {
    verify_remote_daemon_stopped(session).map_err(|error| format!("{transport_error}; {error}"))
}

/// A lifecycle stop acknowledgement alone is not proof that the daemon exited.
/// Verify the exact recorded owner before reconnect can create a new session,
/// otherwise it could reattach the stale lease it was meant to rotate.
fn verify_remote_daemon_stopped(session: &RunnerSession) -> std::result::Result<(), String> {
    let runner = load(&session.runner_id)
        .map_err(|error| format!("re-probe runner after daemon stop: {}", error.message))?;
    let homeboy = remote_runner_homeboy_path(&runner, "runner disconnect stop verification")
        .map_err(|error| format!("re-probe daemon after stop: {}", error.message))?;
    let lease_id = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        "re-probe daemon after stop: persisted daemon lease is unavailable".to_string()
    })?;
    let (_, _, client) = remote_daemon::resolve_ssh_runner(&runner)
        .map_err(|error| format!("re-probe daemon after stop: {}", error.message))?
        .ok_or_else(|| "re-probe daemon after stop: runner is not SSH-backed".to_string())?;
    let status = remote_daemon::remote_daemon_status(&client, homeboy)
        .map_err(|error| format!("authoritative daemon re-probe after stop failed: {error}"))?;
    let fallback_command =
        remote_lease_bound_stop_recovery_command(session, runner.server_id.as_deref(), homeboy);

    complete_stop_transport_recovery(
        session,
        &status,
        || execute_remote_lease_bound_daemon_stop(&client, homeboy, lease_id),
        || remote_daemon::remote_daemon_status(&client, homeboy),
    )
    .map_err(|error| format!("{error}. Recovery: {fallback_command}"))
}

pub(super) fn complete_stop_transport_recovery<Stop, Probe>(
    session: &RunnerSession,
    initial_status: &remote_daemon::RemoteDaemonStatus,
    stop: Stop,
    probe: Probe,
) -> std::result::Result<(), String>
where
    Stop: FnOnce() -> std::result::Result<(), String>,
    Probe: FnOnce() -> std::result::Result<remote_daemon::RemoteDaemonStatus, String>,
{
    match confirm_remote_daemon_stopped_after_transport_error(session, initial_status) {
        Ok(()) => return Ok(()),
        Err(_initial_error) if exact_live_remote_daemon_owner(session, initial_status) => {
            stop()
                .map_err(|error| format!("bounded SSH lease-bound daemon stop failed: {error}"))?;
            let final_status = probe().map_err(|error| {
                format!("authoritative daemon re-probe after SSH stop failed: {error}")
            })?;
            confirm_remote_daemon_stopped_after_transport_error(session, &final_status)
        }
        Err(initial_error) => Err(initial_error),
    }
}

fn exact_live_remote_daemon_owner(
    session: &RunnerSession,
    status: &remote_daemon::RemoteDaemonStatus,
) -> bool {
    status.active_jobs == 0
        && status.reachable
        && status.daemon.as_ref().is_some_and(|daemon| {
            daemon.lease_id.as_deref() == session.remote_daemon_lease_id.as_deref()
                && daemon.pid == session.remote_daemon_pid
        })
}

fn remote_lease_bound_stop_recovery_command(
    session: &RunnerSession,
    server_id: Option<&str>,
    homeboy: &str,
) -> String {
    let lease_id = session
        .remote_daemon_lease_id
        .as_deref()
        .unwrap_or("<lease-id>");
    match server_id {
        Some(server_id) => format!(
            "homeboy ssh {} -- {}",
            shell::quote_arg(server_id),
            remote_lease_bound_daemon_stop_command(homeboy, lease_id)
        ),
        None => remote_lease_bound_daemon_stop_command(homeboy, lease_id),
    }
}

fn confirm_remote_daemon_stopped_after_transport_error(
    session: &RunnerSession,
    status: &remote_daemon::RemoteDaemonStatus,
) -> std::result::Result<(), String> {
    let expected_lease = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        "authoritative daemon re-probe cannot verify stop without the persisted lease".to_string()
    })?;
    let expected_pid = session.remote_daemon_pid.ok_or_else(|| {
        "authoritative daemon re-probe cannot verify stop without the persisted PID".to_string()
    })?;
    if status.active_jobs != 0 {
        return Err(format!(
            "authoritative daemon re-probe reports {} active job(s); refusing disconnect",
            status.active_jobs
        ));
    }
    if let Some(daemon) = &status.daemon {
        if status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
            && !status.reachable
            && daemon.lease_id.as_deref() == Some(expected_lease)
            && daemon.pid == Some(expected_pid)
        {
            return Ok(());
        }
        return Err(format!(
            "authoritative daemon re-probe still reports lease `{}` and PID {}; refusing disconnect",
            daemon.lease_id.as_deref().unwrap_or("unavailable"),
            daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable")
        ));
    }
    if status.stale_reason_code == Some(DaemonStaleReasonCode::LeaseMissing) && !status.reachable {
        // A lease-bound stop can already have removed a dead lease. With no
        // active work, reconnect may continue to ensure-running, which still
        // reattaches any demonstrably live owner instead of replacing it.
        return Ok(());
    }
    let clean_stop = status
        .termination_evidence
        .as_ref()
        .is_some_and(|evidence| {
            evidence.classification == crate::daemon::DaemonTerminationClassification::CleanStop
                && evidence.stop_requested
                && evidence.lease_id.as_deref() == Some(expected_lease)
                && evidence.pid == Some(expected_pid)
        });
    if clean_stop && !status.reachable {
        return Ok(());
    }
    Err("authoritative daemon re-probe did not prove the exact lease/PID stopped; refusing disconnect".to_string())
}

pub(super) fn execute_remote_lease_bound_daemon_stop(
    client: &SshClient,
    homeboy: &str,
    lease_id: &str,
) -> std::result::Result<(), String> {
    let command = remote_lease_bound_daemon_stop_command(homeboy, lease_id);
    let output = client.execute_with_timeout(&command, REMOTE_LEASE_BOUND_STOP_TIMEOUT);
    validate_remote_lease_bound_daemon_stop_output(&output)
}

fn validate_remote_lease_bound_daemon_stop_output(
    output: &crate::server::CommandOutput,
) -> std::result::Result<(), String> {
    if output.timed_out {
        return Err(format!(
            "timed out after {}s",
            REMOTE_LEASE_BOUND_STOP_TIMEOUT.as_secs()
        ));
    }
    if !output.success {
        return Err(command_failure_message(
            "remote lease-bound daemon stop failed",
            output,
        ));
    }
    let envelope = parse_envelope(&output.stdout).map_err(|error| {
        format!("remote lease-bound daemon stop returned invalid JSON: {error}")
    })?;
    if !envelope.success {
        return Err("remote lease-bound daemon stop returned an error envelope".to_string());
    }
    if envelope
        .data
        .as_ref()
        .and_then(|data| data.get("action"))
        .and_then(Value::as_str)
        != Some("stop")
    {
        return Err("remote lease-bound daemon stop returned an unexpected response".to_string());
    }
    Ok(())
}

pub(super) fn remote_lease_bound_daemon_stop_command(homeboy: &str, lease_id: &str) -> String {
    format!(
        "{} daemon stop --lease-id {}",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn remote_daemon_status(
        reachable: bool,
        active_jobs: usize,
        lease_id: &str,
        pid: u32,
        stale_reason_code: Option<DaemonStaleReasonCode>,
    ) -> remote_daemon::RemoteDaemonStatus {
        remote_daemon::RemoteDaemonStatus {
            daemon: Some(remote_daemon::RemoteDaemon {
                address: "127.0.0.1:49152".to_string(),
                pid: Some(pid),
                lease_id: Some(lease_id.to_string()),
                version: None,
                build_identity: None,
                inspected_freshness: None,
            }),
            stale_reason: (!reachable).then(|| "daemon is stale".to_string()),
            stale_reason_code,
            fresh: reachable,
            reachable,
            active_jobs,
            endpoint_probe_error: None,
            termination_evidence: None,
        }
    }

    #[test]
    fn transport_drop_uses_bounded_ssh_stop_for_exact_live_owner() {
        let session = direct_ssh_session("lease-live");
        let initial = remote_daemon_status(true, 0, "lease-live", 4242, None);
        let stopped = remote_daemon_status(
            false,
            0,
            "lease-live",
            4242,
            Some(DaemonStaleReasonCode::PidDead),
        );
        let mut stop_called = false;

        complete_stop_transport_recovery(
            &session,
            &initial,
            || {
                stop_called = true;
                Ok(())
            },
            || Ok(stopped.clone()),
        )
        .expect("exact live owner is stopped through bounded SSH fallback");

        assert!(stop_called);
        assert_eq!(
            remote_daemon::remote_daemon_connect_action(Some(&session), &stopped)
                .expect("the stopped lease is replaced on reconnect"),
            remote_daemon::RemoteDaemonConnectAction::Start,
            "refresh must start a replacement rather than reattach the stale lease"
        );
    }

    #[test]
    fn transport_drop_refuses_mismatch_or_active_jobs_without_ssh_stop() {
        let session = direct_ssh_session("lease-live");
        for status in [
            remote_daemon_status(true, 0, "lease-other", 4242, None),
            remote_daemon_status(true, 1, "lease-live", 4242, None),
        ] {
            let mut stop_called = false;
            let error = complete_stop_transport_recovery(
                &session,
                &status,
                || {
                    stop_called = true;
                    Ok(())
                },
                || unreachable!("ineligible fallback must not re-probe"),
            )
            .expect_err("mismatched or active daemon must stay protected");

            assert!(!stop_called);
            assert!(error.contains("refusing disconnect"));
        }
    }

    #[test]
    fn transport_drop_refuses_daemon_still_live_after_ssh_stop() {
        let session = direct_ssh_session("lease-live");
        let live = remote_daemon_status(true, 0, "lease-live", 4242, None);

        let error =
            complete_stop_transport_recovery(&session, &live, || Ok(()), || Ok(live.clone()))
                .expect_err("a live daemon after SSH fallback must remain protected");

        assert!(error.contains("still reports lease `lease-live`"));
    }

    #[test]
    fn transport_drop_after_successful_stop_accepts_matching_clean_stop_evidence() {
        let session = direct_ssh_session("lease-stopped");
        let status = remote_daemon::RemoteDaemonStatus {
            daemon: None,
            stale_reason: None,
            stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
            fresh: false,
            reachable: false,
            active_jobs: 0,
            endpoint_probe_error: None,
            termination_evidence: Some(crate::daemon::DaemonTerminationEvidence {
                classification: crate::daemon::DaemonTerminationClassification::CleanStop,
                observed_at: Utc::now().to_rfc3339(),
                lease_id: Some("lease-stopped".to_string()),
                pid: Some(4242),
                binary_identity: None,
                active_jobs: 0,
                resource_evidence: "test".to_string(),
                os_evidence: "test".to_string(),
                exit_code: None,
                signal: None,
                stdout: None,
                stderr: None,
                stop_requested: true,
            }),
        };

        confirm_remote_daemon_stopped_after_transport_error(&session, &status)
            .expect("matching clean-stop evidence resolves the transport drop");
    }

    #[test]
    fn transport_drop_accepts_already_dead_exact_owner_without_active_jobs() {
        let session = direct_ssh_session("lease-dead");
        let status = remote_daemon_status(
            false,
            0,
            "lease-dead",
            4242,
            Some(DaemonStaleReasonCode::PidDead),
        );

        confirm_remote_daemon_stopped_after_transport_error(&session, &status)
            .expect("the exact already-dead owner is safe to disconnect");
    }

    #[test]
    fn transport_drop_reconciles_missing_lease_after_idempotent_stop() {
        let session = direct_ssh_session("lease-removed");
        let status = remote_daemon::RemoteDaemonStatus {
            daemon: None,
            stale_reason: None,
            stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
            fresh: false,
            reachable: false,
            active_jobs: 0,
            endpoint_probe_error: None,
            termination_evidence: None,
        };

        confirm_remote_daemon_stopped_after_transport_error(&session, &status)
            .expect("missing lease after a zero-job stop is idempotently reconciled");
    }

    #[test]
    fn transport_drop_refuses_live_or_mismatched_daemon_evidence() {
        let session = direct_ssh_session("lease-owned");
        let live = remote_daemon_status(true, 0, "lease-owned", 4242, None);
        let live_error = confirm_remote_daemon_stopped_after_transport_error(&session, &live)
            .expect_err("a live daemon must remain protected");
        assert!(live_error.contains("still reports lease `lease-owned`"));

        let mismatched = remote_daemon_status(
            false,
            0,
            "lease-other",
            4242,
            Some(DaemonStaleReasonCode::PidDead),
        );
        let mismatch_error =
            confirm_remote_daemon_stopped_after_transport_error(&session, &mismatched)
                .expect_err("a different dead lease must not authorize disconnect");
        assert!(mismatch_error.contains("lease `lease-other`"));
    }

    #[test]
    fn lease_bound_ssh_stop_rejects_timeout_and_malformed_output() {
        let timeout = crate::server::CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            exit_code: 124,
            timed_out: true,
            child_resource: None,
        };
        let timeout_error = validate_remote_lease_bound_daemon_stop_output(&timeout)
            .expect_err("timed out SSH stop must fail closed");
        assert!(timeout_error.contains("timed out"));

        let malformed = crate::server::CommandOutput {
            stdout: "not JSON".to_string(),
            stderr: String::new(),
            success: true,
            exit_code: 0,
            timed_out: false,
            child_resource: None,
        };
        let malformed_error = validate_remote_lease_bound_daemon_stop_output(&malformed)
            .expect_err("malformed SSH stop output must fail closed");
        assert!(malformed_error.contains("invalid JSON"));
    }
}
