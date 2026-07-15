use std::time::Duration;

use serde_json::Value;

use super::*;

const REMOTE_LEASE_BOUND_STOP_TIMEOUT: Duration = Duration::from_secs(30);

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
    output: &crate::core::server::CommandOutput,
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
            || Ok(stopped),
        )
        .expect("exact live owner is stopped through bounded SSH fallback");

        assert!(stop_called);
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
    fn lease_bound_ssh_stop_rejects_timeout_and_malformed_output() {
        let timeout = crate::core::server::CommandOutput {
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

        let malformed = crate::core::server::CommandOutput {
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
