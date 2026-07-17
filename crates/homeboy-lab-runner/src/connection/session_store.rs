use super::*;

pub(super) fn session_is_live(session: &RunnerSession) -> bool {
    session_is_live_with_timeout(session, Duration::from_millis(200))
}

pub(super) fn session_is_live_with_timeout(session: &RunnerSession, timeout: Duration) -> bool {
    if session.mode != RunnerTunnelMode::DirectSsh {
        return false;
    }
    if let Some(pid) = session.tunnel_pid {
        if !homeboy_core::process::pid_is_running(pid) {
            return false;
        }
    }
    let Some(local_url) = session.local_url.as_deref() else {
        return false;
    };
    let probe_timeout = timeout / 2;
    session.local_port.is_some_and(|port| {
        wait_for_tcp(port, probe_timeout)
            && super::connection_daemon::daemon_http_health_matches_with_timeout(
                local_url,
                session.remote_daemon_lease_id.as_deref(),
                session.remote_daemon_pid,
                probe_timeout,
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
    paths::runner_controller_session_file(runner_id, &controller_id())
}

pub(super) fn ownership_path(runner_id: &str) -> Result<PathBuf> {
    paths::runner_session_file(runner_id)
}

pub(super) fn controller_id() -> String {
    if let Ok(value) = std::env::var("HOMEBOY_CONTROLLER_ID") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "homeboy".to_string());
    let directory = std::env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unknown-directory".to_string());
    format!("{executable}@{directory}")
}

pub(super) fn read_session(runner_id: &str) -> Result<Option<RunnerSession>> {
    read_session_for_controller(runner_id, &controller_id())
}

/// Resolve this controller's session, or borrow a peer's live direct-SSH
/// tunnel for an in-process handoff. Borrowing never writes a controller
/// record, so only the original controller may later tear down that tunnel.
pub(super) fn read_session_or_live_peer(runner_id: &str) -> Result<Option<RunnerSession>> {
    let session = read_session(runner_id)?;
    if session.as_ref().is_some_and(session_is_live) {
        return Ok(session);
    }

    let directory = paths::runner_sessions_dir()?.join(runner_id);
    let peer = live_peer_session_in(&directory, None, session_is_live)?;
    Ok(peer.or(session))
}

pub(super) fn read_session_for_controller(
    runner_id: &str,
    controller_id: &str,
) -> Result<Option<RunnerSession>> {
    read_session_at(&paths::runner_controller_session_file(
        runner_id,
        controller_id,
    )?)
}

pub(super) fn read_ownership(runner_id: &str) -> Result<Option<RunnerSession>> {
    read_session_at(&ownership_path(runner_id)?)
}

fn read_session_at(path: &PathBuf) -> Result<Option<RunnerSession>> {
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
    let controller_id = if session.mode == RunnerTunnelMode::DirectSsh {
        session.controller_id.clone().unwrap_or_else(controller_id)
    } else {
        controller_id()
    };
    write_session_at(
        &paths::runner_controller_session_file(&session.runner_id, &controller_id)?,
        session,
    )
}

pub(super) fn write_ownership(session: &RunnerSession) -> Result<()> {
    write_session_at(&ownership_path(&session.runner_id)?, session)
}

pub(super) fn claim_ownership_if_owner_not_live(session: &RunnerSession) -> Result<bool> {
    Ok(!read_ownership(&session.runner_id)?
        .as_ref()
        .is_some_and(session_is_live))
}

fn write_session_at(path: &PathBuf, session: &RunnerSession) -> Result<()> {
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

pub(super) fn remove_session(runner_id: &str) -> Result<()> {
    let path = session_path(runner_id)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("delete {}", path.display())))
        })?;
    }
    Ok(())
}

pub(super) fn remove_ownership(runner_id: &str) -> Result<()> {
    let path = ownership_path(runner_id)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("delete {}", path.display())))
        })?;
    }
    Ok(())
}

pub(super) fn has_live_peer_session(session: &RunnerSession) -> Result<bool> {
    let directory = paths::runner_sessions_dir()?.join(&session.runner_id);
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("read runner controller sessions".to_string()),
            ))
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read runner controller session".to_string()),
            )
        })?;
        let Some(peer) = read_session_at(&entry.path())? else {
            continue;
        };
        if peer.controller_id != session.controller_id
            && peer.remote_daemon_lease_id == session.remote_daemon_lease_id
            && session_is_live(&peer)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn live_peer_session_in(
    directory: &PathBuf,
    controller_id: Option<&str>,
    is_live: impl Fn(&RunnerSession) -> bool,
) -> Result<Option<RunnerSession>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("read runner controller sessions".to_string()),
            ))
        }
    };
    let mut live_peer: Option<RunnerSession> = None;
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read runner controller session".to_string()),
            )
        })?;
        let Some(peer) = read_session_at(&entry.path())? else {
            continue;
        };
        if controller_id
            .is_none_or(|controller_id| peer.controller_id.as_deref() != Some(controller_id))
            && peer.mode == RunnerTunnelMode::DirectSsh
            && is_live(&peer)
        {
            if let Some(existing) = &live_peer {
                if existing.remote_daemon_address != peer.remote_daemon_address
                    || existing.remote_daemon_lease_id != peer.remote_daemon_lease_id
                    || existing.remote_daemon_pid != peer.remote_daemon_pid
                {
                    // Two live sessions for this runner disagree on daemon
                    // identity. Refuse an ambiguous handoff rather than route
                    // a Cook job to an arbitrary peer tunnel.
                    return Ok(None);
                }
            } else {
                live_peer = Some(peer);
            }
        }
    }
    Ok(live_peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerSessionRole, RunnerTunnelMode};
    use tempfile::TempDir;

    fn session(controller_id: &str, lease_id: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some(controller_id.to_string()),
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:4444".to_string()),
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: Some(42),
            remote_daemon_lease_id: Some(lease_id.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-07-17T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn controller_sessions_have_distinct_paths_and_share_a_lease_record() {
        let first = paths::runner_controller_session_file("lab", "controller-a").expect("path");
        let second = paths::runner_controller_session_file("lab", "controller-b").expect("path");
        let ownership = paths::runner_session_file("lab").expect("path");

        assert_ne!(first, second);
        assert_ne!(first, ownership);
        assert_ne!(second, ownership);
    }

    #[test]
    fn stale_owner_can_be_replaced_without_reusing_its_tunnel() {
        let stale = session("controller-a", "lease-old");
        let replacement = session("controller-b", "lease-live");

        assert_ne!(stale.controller_id, replacement.controller_id);
        assert_ne!(
            stale.remote_daemon_lease_id,
            replacement.remote_daemon_lease_id
        );
    }

    #[test]
    fn cook_handoff_adopts_a_live_peer_direct_ssh_session_without_claiming_it() {
        let root = TempDir::new().expect("session directory");
        let peer = session("cook-readiness", "lease-accepted");
        write_session_at(&root.path().join("cook-readiness.json"), &peer)
            .expect("write readiness session");

        let adopted =
            live_peer_session_in(&root.path().to_path_buf(), Some("cook-handoff"), |_| true)
                .expect("read live peer")
                .expect("accepted session");

        assert_eq!(
            adopted.remote_daemon_lease_id.as_deref(),
            Some("lease-accepted")
        );
        assert_eq!(adopted.controller_id.as_deref(), Some("cook-readiness"));
        assert!(root.path().join("cook-readiness.json").exists());
        assert!(!root.path().join("cook-handoff.json").exists());
    }

    #[test]
    fn fresh_peer_session_survives_repeated_cook_preflight_and_handoff_reads() {
        let root = TempDir::new().expect("session directory");
        let peer = session("cook-readiness", "lease-accepted");
        let path = root.path().join("cook-readiness.json");
        write_session_at(&path, &peer).expect("write readiness session");

        let preflight = live_peer_session_in(&root.path().to_path_buf(), Some("cook"), |_| true)
            .expect("read preflight session")
            .expect("live preflight session");
        let handoff = live_peer_session_in(&root.path().to_path_buf(), Some("cook"), |_| true)
            .expect("read handoff session")
            .expect("live handoff session");

        assert_eq!(preflight, peer);
        assert_eq!(handoff, peer);
        assert_eq!(
            read_session_at(&path).expect("read stored session"),
            Some(peer)
        );
    }

    #[test]
    fn concurrent_status_observers_do_not_mutate_a_borrowed_session() {
        use std::sync::{Arc, Barrier};

        let root = TempDir::new().expect("session directory");
        let peer = session("cook-readiness", "lease-accepted");
        let path = root.path().join("cook-readiness.json");
        write_session_at(&path, &peer).expect("write readiness session");
        let directory = Arc::new(root.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(3));

        let observers: Vec<_> = ["status-a", "status-b"]
            .into_iter()
            .map(|controller| {
                let directory = Arc::clone(&directory);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    live_peer_session_in(&directory, Some(controller), |_| true)
                        .expect("observe peer session")
                        .expect("live peer session")
                })
            })
            .collect();

        barrier.wait();
        for observer in observers {
            assert_eq!(observer.join().expect("status observer"), peer);
        }
        assert_eq!(
            read_session_at(&path).expect("read stored session"),
            Some(peer)
        );
    }

    #[test]
    fn repeated_peer_handoffs_reject_ambiguous_daemon_ownership_without_mutation() {
        let root = TempDir::new().expect("session directory");
        let accepted = session("cook-readiness", "lease-accepted");
        let conflicting = session("other-controller", "lease-other");
        let accepted_path = root.path().join("cook-readiness.json");
        let conflicting_path = root.path().join("other-controller.json");
        write_session_at(&accepted_path, &accepted).expect("write accepted session");
        write_session_at(&conflicting_path, &conflicting).expect("write conflicting session");

        for _ in 0..2 {
            assert!(
                live_peer_session_in(&root.path().to_path_buf(), Some("cook"), |_| true)
                    .expect("read peer sessions")
                    .is_none()
            );
        }

        assert_eq!(
            read_session_at(&accepted_path).expect("read accepted session"),
            Some(accepted)
        );
        assert_eq!(
            read_session_at(&conflicting_path).expect("read conflicting session"),
            Some(conflicting)
        );
    }
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
    output: &homeboy_core::server::CommandOutput,
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
        // Direct SSH tunnels lead their own group so Cook's command cleanup
        // cannot tear them down. Stop that whole group on explicit disconnect,
        // with a root-PID fallback for sessions recorded before isolation.
        if libc::kill(-(pid as libc::pid_t), libc::SIGTERM) != 0 {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}
