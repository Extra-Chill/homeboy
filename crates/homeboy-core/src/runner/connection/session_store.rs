use super::*;

pub(super) fn session_is_live(session: &RunnerSession) -> bool {
    if session.mode != RunnerTunnelMode::DirectSsh {
        return false;
    }
    if let Some(pid) = session.tunnel_pid {
        if !crate::process::pid_is_running(pid) {
            return false;
        }
    }
    let Some(local_url) = session.local_url.as_deref() else {
        return false;
    };
    session.local_port.is_some_and(|port| {
        wait_for_tcp(port, Duration::from_millis(200))
            && super::connection_daemon::daemon_http_health_matches(
                local_url,
                session.remote_daemon_lease_id.as_deref(),
                session.remote_daemon_pid,
            )
    })
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
        Some(session) if session.mode == RunnerTunnelMode::Reverse => RunnerSessionState::Recorded,
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
    output: &crate::server::CommandOutput,
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
