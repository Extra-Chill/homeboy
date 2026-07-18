use super::*;

pub(super) fn session_is_live(session: &RunnerSession) -> bool {
    session_is_live_with_timeout(session, Duration::from_secs(2))
}

pub(super) fn session_is_live_with_timeout(session: &RunnerSession, timeout: Duration) -> bool {
    session_is_live_with_probe(session, timeout, |session, probe_timeout| {
        let Some(local_url) = session.local_url.as_deref() else {
            return false;
        };
        session.local_port.is_some_and(|port| {
            wait_for_tcp(port, probe_timeout)
                && super::connection_daemon::daemon_http_health_matches_with_timeout(
                    local_url,
                    session.remote_daemon_lease_id.as_deref(),
                    session.remote_daemon_pid,
                    probe_timeout,
                )
        })
    })
}

fn session_is_live_with_probe(
    session: &RunnerSession,
    timeout: Duration,
    probe: impl Fn(&RunnerSession, Duration) -> bool,
) -> bool {
    if session.mode != RunnerTunnelMode::DirectSsh {
        return false;
    }
    if let Some(pid) = session.tunnel_pid {
        if !homeboy_core::process::pid_is_running(pid) {
            return false;
        }
    }
    if session.local_url.is_none() || session.local_port.is_none() {
        return false;
    }

    const ATTEMPTS: u32 = 3;
    let deadline = std::time::Instant::now() + timeout;
    for attempt in 0..ATTEMPTS {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        // Reserve time for later attempts instead of allowing the first TCP or
        // HTTP request to consume the entire liveness budget under local load.
        let attempts_left = ATTEMPTS - attempt;
        let probe_timeout = remaining / attempts_left / 2;
        if probe_timeout.is_zero() {
            return false;
        }
        if probe(session, probe_timeout) {
            return true;
        }
    }
    false
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
    system_hostname().unwrap_or_else(|| "unknown-host".to_string())
}

pub(super) fn session_path(runner_id: &str) -> Result<PathBuf> {
    paths::runner_controller_session_file(runner_id, &controller_id())
}

pub(super) fn ownership_path(runner_id: &str) -> Result<PathBuf> {
    paths::runner_session_file(runner_id)
}

pub(super) fn controller_id() -> String {
    controller_id_from_scope(
        std::env::var("HOMEBOY_CONTROLLER_ID").ok().as_deref(),
        controller_scope(),
    )
}

fn controller_id_from_scope(explicit_scope: Option<&str>, controller_scope: String) -> String {
    explicit_scope
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        // A controller may promote its binary or change cwd while a daemon
        // tunnel remains authenticated. Keep that tunnel in one OS-derived,
        // per-user scope rather than trusting optional shell environment.
        .unwrap_or(controller_scope)
}

fn controller_scope() -> String {
    controller_scope_from_host_and_uid(system_hostname().as_deref(), effective_uid())
}

fn controller_scope_from_host_and_uid(hostname: Option<&str>, uid: u32) -> String {
    let host = hostname
        .map(str::trim)
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or("local");
    format!("{host}-uid-{uid}")
}

fn system_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buffer = [0_u8; 256];
        // `gethostname` does not depend on the optional HOSTNAME environment
        // variable, which launchd and other non-interactive macOS shells omit.
        let result = unsafe {
            libc::gethostname(
                buffer.as_mut_ptr().cast::<libc::c_char>(),
                buffer.len() as libc::size_t,
            )
        };
        if result != 0 {
            return None;
        }
        let length = buffer.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&buffer[..length])
            .ok()
            .map(str::to_string)
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").ok()
    }
}

fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

pub(super) fn read_session(runner_id: &str) -> Result<Option<RunnerSession>> {
    read_session_for_controller(runner_id, &controller_id())
}

/// Resolve this controller's session, or borrow a peer's live direct-SSH
/// tunnel for an in-process handoff. Borrowing never writes a controller
/// record, so only the original controller may later tear down that tunnel.
pub(super) fn read_session_or_live_peer(runner_id: &str) -> Result<Option<RunnerSession>> {
    read_session_or_live_peer_for_controller(runner_id, &controller_id())
}

fn read_session_or_live_peer_for_controller(
    runner_id: &str,
    controller_id: &str,
) -> Result<Option<RunnerSession>> {
    let session = read_session_for_controller(runner_id, controller_id)?;
    if session.as_ref().is_some_and(session_is_live) {
        return Ok(session);
    }

    let directory = paths::runner_sessions_dir()?.join(runner_id);
    resolve_session_or_live_peer_in(&directory, controller_id, session, session_is_live)
}

fn resolve_session_or_live_peer_in(
    directory: &PathBuf,
    controller_id: &str,
    session: Option<RunnerSession>,
    is_live: impl Fn(&RunnerSession) -> bool,
) -> Result<Option<RunnerSession>> {
    if session.as_ref().is_some_and(|session| is_live(session)) {
        return Ok(session);
    }

    let peer = live_peer_session_in(directory, Some(controller_id), is_live)?;
    if session
        .as_ref()
        .zip(peer.as_ref())
        .is_some_and(|(session, peer)| same_direct_daemon_identity(session, peer))
    {
        // A current tunnel that only timed out must not be displaced by an
        // older alias for the same daemon. The next status/read gets a fresh,
        // bounded probe of this controller's authoritative tunnel.
        return Ok(session);
    }
    Ok(peer.or(session))
}

fn same_direct_daemon_identity(left: &RunnerSession, right: &RunnerSession) -> bool {
    left.mode == RunnerTunnelMode::DirectSsh
        && right.mode == RunnerTunnelMode::DirectSsh
        && left.remote_daemon_address.is_some()
        && left.remote_daemon_lease_id.is_some()
        && left.remote_daemon_pid.is_some()
        && left.remote_daemon_address == right.remote_daemon_address
        && left.remote_daemon_lease_id == right.remote_daemon_lease_id
        && left.remote_daemon_pid == right.remote_daemon_pid
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
    homeboy_core::engine::local_files::write_json_file(path, session)
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
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

    fn serve_health(
        lease_id: &str,
        pid: u32,
        delay: Duration,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        let freshness = DaemonFreshnessReport {
            fresh: true,
            stale_reason_code: None,
            restartable: false,
            lease_id: Some(lease_id.to_string()),
            pid: Some(pid),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: Some("test".to_string()),
            daemon_build_identity: Some("homeboy test".to_string()),
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        };
        let body = serde_json::json!({ "freshness": freshness, "pid": pid }).to_string();
        let server = std::thread::spawn(move || {
            // wait_for_tcp opens and immediately closes the first connection.
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("health connection");
                let mut request = [0; 1024];
                let read = stream.read(&mut request).expect("read request");
                if read == 0 {
                    continue;
                }
                assert!(std::str::from_utf8(&request[..read])
                    .expect("request text")
                    .starts_with("GET /health HTTP/1.1"));
                std::thread::sleep(delay);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        )
                        .as_bytes(),
                    )
                    .expect("health response");
                return;
            }
            panic!("health request was not received");
        });
        (port, server)
    }

    fn session_for_health_endpoint(port: u16, lease_id: &str, pid: u32) -> RunnerSession {
        let mut session = session("controller", lease_id);
        session.local_port = Some(port);
        session.local_url = Some(format!("http://127.0.0.1:{port}"));
        session.remote_daemon_pid = Some(pid);
        session
    }

    #[test]
    fn session_health_accepts_a_healthy_endpoint_after_one_hundred_milliseconds() {
        let (port, server) = serve_health("lease-live", 42, Duration::from_millis(125));
        let session = session_for_health_endpoint(port, "lease-live", 42);

        assert!(session_is_live_with_timeout(
            &session,
            Duration::from_millis(200)
        ));
        server.join().expect("server");
    }

    #[test]
    fn session_health_rejects_a_mismatched_lease() {
        let (port, server) = serve_health("lease-other", 42, Duration::ZERO);
        let session = session_for_health_endpoint(port, "lease-live", 42);

        assert!(!session_is_live_with_timeout(
            &session,
            Duration::from_millis(200)
        ));
        server.join().expect("server");
    }

    #[test]
    fn session_health_rejects_a_mismatched_pid() {
        let (port, server) = serve_health("lease-live", 43, Duration::ZERO);
        let session = session_for_health_endpoint(port, "lease-live", 42);

        assert!(!session_is_live_with_timeout(
            &session,
            Duration::from_millis(200)
        ));
        server.join().expect("server");
    }

    #[test]
    fn session_health_rejects_a_closed_tunnel() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        let session = session_for_health_endpoint(port, "lease-live", 42);

        assert!(!session_is_live_with_timeout(
            &session,
            Duration::from_millis(100)
        ));
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
    fn direct_session_liveness_retries_a_transient_probe_failure() {
        let mut live = session("controller", "lease-live");
        live.local_port = Some(49152);
        live.local_url = Some("http://127.0.0.1:49152".to_string());
        let attempts = std::cell::Cell::new(0);

        assert!(session_is_live_with_probe(
            &live,
            Duration::from_millis(90),
            |_, probe_timeout| {
                assert!(probe_timeout > Duration::ZERO);
                attempts.set(attempts.get() + 1);
                attempts.get() == 2
            },
        ));
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn direct_session_liveness_caps_failed_probes_at_three_attempts() {
        let mut live = session("controller", "lease-live");
        live.local_port = Some(49152);
        live.local_url = Some("http://127.0.0.1:49152".to_string());
        let attempts = std::cell::Cell::new(0);

        assert!(!session_is_live_with_probe(
            &live,
            Duration::from_millis(90),
            |_, probe_timeout| {
                assert!(probe_timeout > Duration::ZERO);
                attempts.set(attempts.get() + 1);
                false
            },
        ));
        assert_eq!(attempts.get(), 3);
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
    fn stale_controller_scope_resolves_the_live_peer_for_refresh_and_availability() {
        let root = TempDir::new().expect("session directory");
        let stale = session("worktree-a", "lease-stale");
        let connected = session("worktree-b", "lease-live");
        let stale_path = root.path().join("worktree-a.json");
        let connected_path = root.path().join("worktree-b.json");
        write_session_at(&stale_path, &stale).expect("write stale controller session");
        write_session_at(&connected_path, &connected).expect("write connected controller session");

        let local = read_session_at(&stale_path).expect("read stale controller session");
        let resolved = resolve_session_or_live_peer_in(
            &root.path().to_path_buf(),
            "worktree-a",
            local,
            |candidate| candidate.remote_daemon_lease_id.as_deref() == Some("lease-live"),
        )
        .expect("resolve live peer")
        .expect("authoritative session");

        assert_eq!(resolved.controller_id.as_deref(), Some("worktree-b"));
        assert_eq!(
            resolved.remote_daemon_lease_id.as_deref(),
            Some("lease-live")
        );
        assert_eq!(
            read_session_at(&stale_path).expect("read stale session after resolution"),
            Some(stale)
        );
    }

    #[test]
    fn sequential_daemon_phases_keep_the_os_scoped_live_session_across_runtime_churn() {
        let root = TempDir::new().expect("session directory");
        let controller = controller_scope_from_host_and_uid(Some("macbook-pro"), 501);
        let connected = session(&controller, "lease-live");
        let session_path = root.path().join(format!("{controller}.json"));
        write_session_at(&session_path, &connected).expect("write connected session");

        // These phases may run binaries from different promoted paths and CWDs.
        // They share the OS controller scope, so all resolve the original tunnel.
        let status = read_session_at(&session_path)
            .expect("read status session")
            .expect("authoritative status session");
        let read = read_session_at(&session_path)
            .expect("read daemon job session")
            .expect("authoritative daemon job session");
        let mutation = read_session_at(&session_path)
            .expect("read mutation session")
            .expect("authoritative mutation session");

        assert_eq!(status.remote_daemon_lease_id.as_deref(), Some("lease-live"));
        assert_eq!(read.remote_daemon_lease_id, status.remote_daemon_lease_id);
        assert_eq!(
            mutation.remote_daemon_lease_id,
            status.remote_daemon_lease_id
        );
        assert_eq!(mutation.remote_daemon_address, status.remote_daemon_address);
        assert_eq!(
            read_session_at(&session_path).expect("read stored session"),
            Some(connected)
        );
    }

    #[test]
    fn stale_runtime_controller_alias_borrows_the_same_authoritative_session() {
        let root = TempDir::new().expect("session directory");
        let connected = session("macbook-pro-uid-501", "lease-live");
        let stale_alias = session("homeboy@old-worktree", "lease-stale");
        let connected_path = root.path().join("macbook-pro-uid-501.json");
        let stale_path = root.path().join("homeboy@old-worktree.json");
        write_session_at(&connected_path, &connected).expect("write connected session");
        write_session_at(&stale_path, &stale_alias).expect("write stale alias");

        let resolved = resolve_session_or_live_peer_in(
            &root.path().to_path_buf(),
            "homeboy@old-worktree",
            read_session_at(&stale_path).expect("read stale alias"),
            |candidate| candidate.remote_daemon_lease_id.as_deref() == Some("lease-live"),
        )
        .expect("resolve live peer")
        .expect("authoritative session");

        assert_eq!(resolved, connected);
        assert_eq!(
            read_session_at(&stale_path).expect("read stale alias after resolution"),
            Some(stale_alias)
        );
    }

    #[test]
    fn transient_current_probe_does_not_yield_to_same_daemon_aliases() {
        let root = TempDir::new().expect("session directory");
        let current = session("mac_lan-uid-501", "lease-live");
        let legacy = session("unknown-host", "lease-live");
        let second_alias = session("prior-worktree", "lease-live");
        write_session_at(&root.path().join("mac_lan-uid-501.json"), &current)
            .expect("write current session");
        write_session_at(&root.path().join("unknown-host.json"), &legacy)
            .expect("write legacy alias");
        write_session_at(&root.path().join("prior-worktree.json"), &second_alias)
            .expect("write second alias");
        let current_probe_attempts = std::cell::Cell::new(0);

        let resolved = resolve_session_or_live_peer_in(
            &root.path().to_path_buf(),
            "mac_lan-uid-501",
            Some(current.clone()),
            |candidate| {
                if candidate.controller_id == current.controller_id {
                    current_probe_attempts.set(current_probe_attempts.get() + 1);
                    return false;
                }
                true
            },
        )
        .expect("resolve session")
        .expect("current session remains authoritative");

        assert_eq!(current_probe_attempts.get(), 1);
        assert_eq!(resolved, current);
        assert_eq!(
            read_session_at(&root.path().join("unknown-host.json")).expect("read legacy alias"),
            Some(legacy)
        );
        assert_eq!(
            read_session_at(&root.path().join("prior-worktree.json")).expect("read second alias"),
            Some(second_alias)
        );
    }

    #[test]
    fn promoted_controller_scope_resolves_status_and_cancel_to_the_same_live_session() {
        let root = TempDir::new().expect("session directory");
        let status_alias = session("cargo-homeboy@worktree-a", "lease-live");
        let stale_mutation_alias = session("cargo-homeboy@worktree-b", "lease-stale");
        let status_path = root.path().join("cargo-homeboy@worktree-a.json");
        let mutation_path = root.path().join("cargo-homeboy@worktree-b.json");
        write_session_at(&status_path, &status_alias).expect("write status session");
        write_session_at(&mutation_path, &stale_mutation_alias)
            .expect("write stale mutation session");

        let status = resolve_session_or_live_peer_in(
            &root.path().to_path_buf(),
            "cargo-homeboy@worktree-a",
            read_session_at(&status_path).expect("read status session"),
            |candidate| candidate.remote_daemon_lease_id.as_deref() == Some("lease-live"),
        )
        .expect("resolve status session")
        .expect("authoritative status session");
        let cancellation = resolve_session_or_live_peer_in(
            &root.path().to_path_buf(),
            "cargo-homeboy@worktree-b",
            read_session_at(&mutation_path).expect("read mutation session"),
            |candidate| candidate.remote_daemon_lease_id.as_deref() == Some("lease-live"),
        )
        .expect("resolve mutation session")
        .expect("authoritative mutation session");

        assert_eq!(
            status.remote_daemon_lease_id,
            Some("lease-live".to_string())
        );
        assert_eq!(
            cancellation.remote_daemon_lease_id,
            status.remote_daemon_lease_id
        );
        assert_eq!(
            cancellation.remote_daemon_address,
            status.remote_daemon_address
        );
        assert_eq!(
            read_session_at(&mutation_path).expect("read stale mutation session"),
            Some(stale_mutation_alias)
        );
    }

    #[test]
    fn controller_scope_ignores_runtime_path_and_cwd_churn() {
        assert_eq!(
            controller_scope_from_host_and_uid(Some("controller-host"), 501),
            "controller-host-uid-501"
        );
        assert_eq!(
            controller_id_from_scope(
                Some("  controller-a  "),
                "controller-host-uid-501".to_string()
            ),
            "controller-a"
        );
        assert_eq!(
            controller_id_from_scope(Some("   "), "controller-host-uid-501".to_string()),
            "controller-host-uid-501"
        );
        assert_eq!(
            controller_scope_from_host_and_uid(None, 501),
            "local-uid-501",
            "the scope remains per-user when hostname lookup is unavailable"
        );
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
