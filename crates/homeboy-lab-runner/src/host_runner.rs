//! Read-only discovery of the daemon owned by this runner host.
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::{Runner, RunnerSession};

pub(crate) const ID: &str = "host-daemon";

fn lease_path() -> Option<std::path::PathBuf> {
    Some(
        homeboy_core::paths::home_root()
            .ok()?
            .join(crate::connection::daemon_state_dir())
            .join("state.json"),
    )
}

pub(crate) fn session() -> Option<RunnerSession> {
    session_at(&lease_path()?)
}

/// The lease of a daemon that is alive and answering on loopback.
fn live_lease_at(path: &std::path::Path) -> Option<serde_json::Value> {
    let lease: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if lease["schema"] != "homeboy.daemon.session_lease.v2" {
        return None;
    }
    let pid = u32::try_from(lease["pid"].as_u64()?).ok()?;
    if pid == 0 || unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return None;
    }
    let address: SocketAddr = lease["address"].as_str()?.parse().ok()?;
    if !address.ip().is_loopback() {
        return None;
    }
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).ok()?;
    Some(lease)
}

fn session_at(path: &std::path::Path) -> Option<RunnerSession> {
    session_from_lease(&live_lease_at(path)?)
}

fn session_from_lease(lease: &serde_json::Value) -> Option<RunnerSession> {
    let address = lease["address"].as_str()?;
    serde_json::from_value(serde_json::json!({
        "runner_id": ID,
        "mode": "loopback",
        "role": "runner",
        "local_url": format!("http://{address}"),
        "remote_daemon_address": address,
        "remote_daemon_pid": lease["pid"],
        "remote_daemon_lease_id": lease["lease_id"],
        "homeboy_version": lease["build_identity"]["version"].as_str().unwrap_or_default(),
        "homeboy_build_identity": lease["build_identity"]["display"],
        "connected_at": lease["started_at"].as_str().unwrap_or_default()
    }))
    .ok()
}

pub(crate) fn runner() -> Option<Runner> {
    runner_at(&lease_path()?)
}

/// The host runner executes gates with the exact binary its daemon runs, as
/// the lease records it; that is the pin an SSH runner gets from
/// `refresh-homeboy`.
fn runner_at(path: &std::path::Path) -> Option<Runner> {
    let lease = live_lease_at(path)?;
    let mut runner = Runner {
        id: ID.to_string(),
        ..crate::builtin_local_runner()
    };
    runner.settings.homeboy_path = lease["executable"]["path"].as_str().map(str::to_string);
    Some(runner)
}

pub(crate) fn is_host(runner: &Runner) -> bool {
    runner.id == ID && runner.kind == crate::RunnerKind::Local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{connected_daemon_session, select_runner_transport, RunnerTransport};

    fn write_lease(home: &std::path::Path, pid: u32, address: &str) {
        let dir = home.join(crate::connection::daemon_state_dir());
        std::fs::create_dir_all(&dir).expect("state dir");
        let lease = serde_json::json!({
            "schema": "homeboy.daemon.session_lease.v2",
            "lease_id": "lease-host",
            "address": address,
            "pid": pid,
            "started_at": "2026-01-01T00:00:00Z",
            "build_identity": { "version": "1.0.0", "display": "homeboy 1.0.0" },
            "lab_contract_version": 1,
            "executable": { "path": "/opt/homeboy/bin/homeboy-pinned", "dev": 1, "ino": 2 }
        });
        std::fs::write(dir.join("state.json"), lease.to_string()).expect("lease");
    }

    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        pid
    }

    #[test]
    fn live_host_daemon_lease_is_the_lab_runner_and_stale_lease_is_not() {
        crate::test_support::with_isolated_home(|home| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let address = listener.local_addr().expect("addr").to_string();

            write_lease(home.path(), dead_pid(), &address);
            assert!(session().is_none(), "a dead daemon pid is not a runner");
            assert!(!crate::list().unwrap().iter().any(|r| r.id == ID));
            assert!(crate::configured_lab_runner_ids().unwrap().is_empty());

            write_lease(home.path(), std::process::id(), &address);
            let runner = crate::load(ID).expect("host runner loads");
            assert!(is_host(&runner));
            // Gates run the daemon's own binary (#18707 follow-up).
            assert_eq!(
                runner.settings.homeboy_path.as_deref(),
                Some("/opt/homeboy/bin/homeboy-pinned")
            );
            assert!(crate::list().unwrap().iter().any(|r| r.id == ID));
            assert_eq!(
                crate::configured_lab_runner_ids().unwrap(),
                vec![ID.to_string()]
            );
            assert_eq!(
                crate::lab_runner_wait_target().unwrap().as_deref(),
                Some(ID)
            );

            let session = session().expect("live lease is a connected session");
            assert_eq!(session.local_url, Some(format!("http://{address}")));
            assert_eq!(
                session.remote_daemon_lease_id.as_deref(),
                Some("lease-host")
            );

            drop(listener);
            assert!(
                session_at_home(home.path()).is_none(),
                "an unanswering address is not a runner"
            );
        });
    }

    fn session_at_home(home: &std::path::Path) -> Option<RunnerSession> {
        session_at(
            &home
                .join(crate::connection::daemon_state_dir())
                .join("state.json"),
        )
    }

    #[test]
    fn host_daemon_runner_selects_direct_daemon_transport_at_lease_address() {
        crate::test_support::with_isolated_home(|home| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let address = listener.local_addr().expect("addr").to_string();
            write_lease(home.path(), std::process::id(), &address);
            let runner = runner().expect("host runner");
            let session = session().expect("session");
            let status = crate::RunnerStatusReport {
                runner_id: ID.to_string(),
                connected: true,
                state: crate::RunnerSessionState::Connected,
                session: Some(session),
                stale_daemon: None,
                configured_job_binary_build_identity: None,
                daemon_freshness: None,
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: crate::RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                active_job_recovery_evidence: None,
                recent_observation_failure: None,
                session_path: String::new(),
            };
            assert_eq!(
                select_runner_transport(&runner, Some(&status), false),
                Some(RunnerTransport::Daemon)
            );
            let handle = connected_daemon_session(Some(&status)).expect("daemon session");
            assert_eq!(handle.endpoint_url(), format!("http://{address}"));
        });
    }
}
