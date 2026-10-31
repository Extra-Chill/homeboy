//! End-to-end acceptance for refresh's in-place daemon restart (#16680).
//!
//! A refresh whose active jobs are all re-adoptable restarts the connected
//! daemon on its own state directory instead of starting a replacement daemon.
//! These tests drive the real `connection::restart_daemon_in_place` against a
//! localhost SSH runner. A fake `homeboy` handles the lease lifecycle
//! (`self identity`, `daemon stop`, `daemon ensure-running`, `daemon status`)
//! and a fake `systemctl` handles the service unit. A loopback HTTP server
//! plays the restarted daemon. The controller then observes the job through
//! the production polling path, from the stale lease's endpoint onto the
//! restarted one.
//!
//! Re-adoption on the runner side, where the restarted daemon continues the
//! job without spawning its child a second time, is covered by
//! `crates/homeboy-core/src/daemon/runner_exec_job.rs::restart_mid_run_re_adopts_the_child_without_a_second_spawn`.
//! It is not duplicated here: these tests cover the controller half of the
//! contract.

use super::*;
use homeboy_core::api_jobs::{Job, JobStatus};
use reqwest::blocking::Client;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const RUNNER: &str = "local-runner";
const OLD_LEASE: &str = "lease-old";
const NEW_LEASE: &str = "lease-new";
const NEW_PID: u32 = 42;
const OLD_IDENTITY: &str = "homeboy 0.283.0+old";
const NEW_VERSION: &str = "0.284.0";
const NEW_IDENTITY: &str = "homeboy 0.284.0+test";

/// The restarted daemon's loopback endpoint. It serves `/health` and
/// `/version` for the restarted lease, and `/jobs/<id>` for the re-adopted
/// job. The first poll reports the job running and every later poll reports
/// its real terminal status.
struct RestartedDaemon {
    address: String,
    terminal_responses: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl RestartedDaemon {
    fn serve(job: Job) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("restarted daemon");
        listener
            .set_nonblocking(true)
            .expect("non-blocking restarted daemon listener");
        let address = listener.local_addr().expect("address").to_string();
        let terminal_responses = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let served_terminal = Arc::clone(&terminal_responses);
        let stopping = Arc::clone(&stop);
        let server = std::thread::spawn(move || {
            let job_path = format!("/jobs/{}", job.id);
            let mut job_polls = 0usize;
            while !stopping.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("restarted daemon accept: {error}"),
                };
                stream.set_nonblocking(false).expect("blocking stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                let path = request_path(&mut stream);
                let (status, body) = match path.as_str() {
                    "/health" => (
                        "200 OK",
                        serde_json::json!({
                            "freshness": {
                                "fresh": true,
                                "restartable": false,
                                "lease_id": NEW_LEASE,
                                "pid": NEW_PID,
                                "active_jobs": 1,
                            },
                            "pid": NEW_PID,
                        }),
                    ),
                    "/version" => (
                        "200 OK",
                        serde_json::json!({
                            "version": NEW_VERSION,
                            "build_identity": { "display": NEW_IDENTITY },
                        }),
                    ),
                    path if path == job_path => {
                        job_polls += 1;
                        let mut observed = job.clone();
                        if job_polls > 1 {
                            observed.status = JobStatus::Succeeded;
                            observed.finished_at_ms = Some(1_700_000_002_000);
                            served_terminal.fetch_add(1, Ordering::SeqCst);
                        }
                        (
                            "200 OK",
                            serde_json::json!({
                                "success": true,
                                "data": { "body": { "job": observed } },
                            }),
                        )
                    }
                    _ => (
                        "404 Not Found",
                        serde_json::json!({
                            "success": false,
                            "error": { "code": "not_found", "message": path },
                        }),
                    ),
                };
                let body = body.to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        Self {
            address,
            terminal_responses,
            stop,
            server: Some(server),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for RestartedDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn request_path(stream: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&request)
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

/// The fake runner-side `homeboy`. It logs every invocation as
/// `<HOMEBOY_DAEMON_STATE_DIR>|<argv>` and materializes the state directory it
/// was pointed at, so the test can prove no second directory appeared. The
/// lease file is the daemon on that state directory: `lease-old` until it is
/// stopped, then `stopped`, then `lease-new` once it is started again (by
/// `ensure-running` or by the unit restart).
const FAKE_HOMEBOY: &str = r#"#!/bin/sh
printf '%s|%s\n' "${HOMEBOY_DAEMON_STATE_DIR:-}" "$*" >> '__LOG__'
if [ -n "${HOMEBOY_DAEMON_STATE_DIR:-}" ]; then
  mkdir -p "$HOMEBOY_DAEMON_STATE_DIR"
fi
lease=$(cat '__LEASE__')
case "$1 $2" in
  "self identity")
    printf '%s\n' '{"success":true,"data":{"version":"__VERSION__","display":"__IDENTITY__"}}'
    ;;
  "daemon stop")
    if [ "$3" != "--lease-id" ] || [ "$4" != "$lease" ]; then
      printf '%s\n' '{"success":false,"error":{"code":"lease_mismatch"}}'
      exit 2
    fi
    printf 'stopped' > '__LEASE__'
    printf '%s\n' '{"success":true,"data":{"action":"stop","stopped":true}}'
    ;;
  "daemon ensure-running")
    if [ "$lease" != stopped ]; then
      printf '%s\n' '{"success":false,"error":{"code":"daemon_owner_lock_held"}}'
      exit 2
    fi
    printf '__NEW_LEASE__' > '__LEASE__'
    printf '%s\n' '{"success":true,"data":{"address":"__ADDRESS__","pid":__PID__,"lease_id":"__NEW_LEASE__"}}'
    ;;
  "daemon status")
    case "$lease" in
      stopped)
        printf '%s\n' '{"success":true,"data":{"running":false,"reachable":false,"fresh":false}}'
        ;;
      __NEW_LEASE__)
        printf '%s\n' '{"success":true,"data":{"running":true,"reachable":true,"fresh":true,"active_jobs":1,"state":{"address":"__ADDRESS__","pid":__PID__,"lease_id":"__NEW_LEASE__","active_version":"__VERSION__","active_build":"__IDENTITY__"}}}'
        ;;
      *)
        printf '%s\n' '{"success":true,"data":{"running":true,"reachable":true,"fresh":false,"active_jobs":1,"state":{"address":"127.0.0.1:1","pid":41,"lease_id":"__OLD_LEASE__","active_version":"0.283.0","active_build":"__OLD_IDENTITY__"}}}'
        ;;
    esac
    ;;
  *)
    exit 64
    ;;
esac
"#;

/// The runner's `curl`, used only for the endpoint re-probe of a restarted
/// direct daemon. It answers like the restarted daemon, which still owns the
/// re-adopted job.
const FAKE_CURL: &str = r#"#!/bin/sh
for url; do :; done
case "$url" in
  */version)
    printf '%s\n' '{"version":"__VERSION__","build_identity":{"display":"__IDENTITY__"}}'
    ;;
  */jobs)
    printf '%s\n' '{"success":true,"data":{"body":{"active_runner_jobs":[{"job_id":"__JOB__","re_adoptable":true}],"stale_runner_jobs":[]}}}'
    ;;
  *)
    exit 22
    ;;
esac
"#;

/// The runner's `systemctl`. It records each request, and a unit restart
/// brings the daemon back up on the unit's state directory under a new lease.
/// A restart does not stop the job's process: the rendered unit carries
/// `KillMode=process`, which the test asserts.
const FAKE_SYSTEMCTL: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> '__SYSTEMCTL_LOG__'
case "$*" in
  "--user restart "*)
    printf '__NEW_LEASE__' > '__LEASE__'
    ;;
esac
"#;

struct RestartFixture {
    daemon: RestartedDaemon,
    homeboy: PathBuf,
    homeboy_log: PathBuf,
    systemctl_log: PathBuf,
    lease_file: PathBuf,
    previous: crate::RunnerSession,
    job_id: String,
}

fn render(template: &str, replacements: &[(&str, &str)]) -> String {
    replacements
        .iter()
        .fold(template.to_string(), |rendered, (from, to)| {
            rendered.replace(from, to)
        })
}

/// A connected direct-SSH runner whose daemon (`lease-old`) owns one active,
/// re-adoptable exec job. Its endpoint is already gone, as it is once refresh
/// has stopped it.
fn connected_runner_with_re_adoptable_job(
    home: &tempfile::TempDir,
    service_managed: bool,
) -> RestartFixture {
    let job_uuid = uuid::Uuid::new_v4();
    let job_id = job_uuid.to_string();
    let daemon = RestartedDaemon::serve(Job {
        id: job_uuid,
        operation: "runner.exec".to_string(),
        status: JobStatus::Running,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
        started_at_ms: Some(1_700_000_000_000),
        finished_at_ms: None,
        event_count: 0,
        source_snapshot: None,
        path_materialization_plan: None,
        stale_reason: None,
        daemon_lease_id: Some(OLD_LEASE.to_string()),
        target_runner_id: Some(RUNNER.to_string()),
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: Vec::new(),
        runner_job_projection: None,
    });

    let homeboy = home.path().join("remote-homeboy");
    let homeboy_log = home.path().join("remote-homeboy.log");
    let systemctl_log = home.path().join("systemctl.log");
    let lease_file = home.path().join("remote-daemon-lease");
    std::fs::write(&lease_file, OLD_LEASE).expect("seed the running lease");
    let pid = NEW_PID.to_string();
    let replacements = [
        ("__LOG__", homeboy_log.to_str().expect("log path")),
        (
            "__SYSTEMCTL_LOG__",
            systemctl_log.to_str().expect("log path"),
        ),
        ("__LEASE__", lease_file.to_str().expect("lease path")),
        ("__ADDRESS__", daemon.address.as_str()),
        ("__PID__", pid.as_str()),
        ("__NEW_LEASE__", NEW_LEASE),
        ("__OLD_LEASE__", OLD_LEASE),
        ("__VERSION__", NEW_VERSION),
        ("__IDENTITY__", NEW_IDENTITY),
        ("__OLD_IDENTITY__", OLD_IDENTITY),
        ("__JOB__", job_id.as_str()),
    ];
    homeboy_test_support::write_executable(&homeboy, render(FAKE_HOMEBOY, &replacements), 0o755)
        .expect("write remote homeboy");
    // The runner shell puts `$HOME/.local/bin` first on PATH.
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).expect("runner bin");
    homeboy_test_support::write_executable(
        bin.join("curl"),
        render(FAKE_CURL, &replacements),
        0o755,
    )
    .expect("write runner curl");
    homeboy_test_support::write_executable(
        bin.join("systemctl"),
        render(FAKE_SYSTEMCTL, &replacements),
        0o755,
    )
    .expect("write runner systemctl");

    homeboy_core::server::create(
        &serde_json::json!({ "id": RUNNER, "host": "localhost", "user": "test" }).to_string(),
        false,
    )
    .expect("create local server");
    crate::create(
        &serde_json::json!({
            "id": RUNNER,
            "kind": "ssh",
            "homeboy_path": homeboy,
            "service_managed": service_managed,
        })
        .to_string(),
        false,
    )
    .expect("enable local runner");

    let stale_endpoint = std::net::TcpListener::bind("127.0.0.1:0").expect("stale endpoint");
    let stale_address = stale_endpoint.local_addr().expect("stale address");
    drop(stale_endpoint);
    let controller_id = crate::connection::controller_id();
    let previous = crate::RunnerSession {
        runner_id: RUNNER.to_string(),
        mode: crate::RunnerTunnelMode::DirectSsh,
        role: crate::RunnerSessionRole::Controller,
        server_id: Some(RUNNER.to_string()),
        controller_id: Some(controller_id.clone()),
        broker_url: None,
        remote_daemon_address: Some(stale_address.to_string()),
        local_port: Some(stale_address.port()),
        local_url: Some(format!("http://{stale_address}")),
        tunnel_pid: None,
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(41),
        remote_daemon_lease_id: Some(OLD_LEASE.to_string()),
        homeboy_version: "0.283.0".to_string(),
        homeboy_build_identity: Some(OLD_IDENTITY.to_string()),
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };
    let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
    let session_path = homeboy_core::paths::runner_controller_session_file_in_root(
        roots.config(),
        RUNNER,
        &controller_id,
    );
    std::fs::create_dir_all(session_path.parent().expect("session directory"))
        .expect("session directory");
    std::fs::write(
        session_path,
        serde_json::to_vec(&previous).expect("serialize connected session"),
    )
    .expect("persist connected session");
    crate::admission_fence::record_admitted_job(RUNNER, &job_id)
        .expect("the stopped lease admitted the job");
    assert_eq!(
        crate::connection::read_session(RUNNER)
            .expect("route job")
            .and_then(|session| session.remote_daemon_lease_id),
        Some(OLD_LEASE.to_string())
    );

    RestartFixture {
        daemon,
        homeboy,
        homeboy_log,
        systemctl_log,
        lease_file,
        previous,
        job_id,
    }
}

/// `HOMEBOY_DAEMON_STATE_DIR` as the runner shell expands it.
fn expected_state_dir() -> String {
    format!(
        "{}/{}",
        std::env::var("HOME").expect("HOME"),
        crate::connection::daemon_state_dir(RUNNER, &crate::connection::controller_id())
    )
}

/// `(state_dir, argv)` for each `daemon ...` invocation. Other subcommands
/// (`self identity`) are not daemon commands and carry no pinned state dir.
fn daemon_invocations(log: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('|'))
        .filter(|(state_dir, argv)| !state_dir.is_empty() && argv.starts_with("daemon "))
        .map(|(state_dir, argv)| (state_dir.to_string(), argv.to_string()))
        .collect()
}

/// Only the one daemon state directory exists for this runner and controller:
/// no second state directory was created next to it.
fn assert_single_state_dir() {
    let state_dir = PathBuf::from(expected_state_dir());
    let runner_dir = state_dir.parent().expect("runner state root");
    let children = std::fs::read_dir(runner_dir)
        .expect("runner state root exists")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        children,
        vec![state_dir
            .file_name()
            .expect("controller dir")
            .to_os_string()],
        "only this controller's state directory exists"
    );
}

/// The runner's one current session is the restarted lease, and the
/// re-adopted job routes through it to the restarted daemon's endpoint.
fn assert_owner_rebound_to_restarted_lease(fixture: &RestartFixture) {
    let owner = crate::connection::read_session(RUNNER)
        .expect("route job")
        .expect("current session");
    assert_eq!(owner.remote_daemon_lease_id.as_deref(), Some(NEW_LEASE));
    assert_eq!(owner.remote_daemon_pid, Some(NEW_PID));
    assert_eq!(owner.local_url, Some(fixture.daemon.url()));
    assert_eq!(owner.homeboy_build_identity.as_deref(), Some(NEW_IDENTITY));
}

/// Poll the job the way a foreground observer does once refresh has
/// restarted its daemon. The poll starts from the stopped lease's endpoint and
/// carries the identity the job was admitted under. The observer must follow
/// the current session to the restarted lease, see the job still running, then see its
/// real terminal status exactly once. It must not report a lost job, a dead
/// lease or a replaced daemon.
fn assert_observer_follows_job_to_terminal(fixture: &RestartFixture) {
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");
    let mut endpoint = fixture
        .previous
        .local_url
        .clone()
        .expect("stopped lease endpoint");
    let mut observed = Vec::new();
    loop {
        let (job, next_endpoint) = fetch_daemon_job_resilient_with_endpoint_reload(
            &client,
            &endpoint,
            &fixture.job_id,
            || super::super::daemon::refreshed_daemon_endpoint(RUNNER, Some(OLD_IDENTITY)),
        )
        .expect("the re-adopted job stays observable across the restart");
        assert_eq!(job.id.to_string(), fixture.job_id);
        endpoint = next_endpoint;
        observed.push(job.status);
        if job.status.is_terminal() {
            break;
        }
        assert!(observed.len() < 8, "job never settled: {observed:?}");
    }
    assert_eq!(observed, vec![JobStatus::Running, JobStatus::Succeeded]);
    assert_eq!(endpoint, fixture.daemon.url());
    assert_eq!(
        fixture.daemon.terminal_responses.load(Ordering::SeqCst),
        1,
        "the terminal status is observed exactly once"
    );
}

#[test]
fn direct_refresh_restarts_the_daemon_in_place_and_keeps_its_re_adoptable_job() {
    crate::test_support::with_isolated_home(|home| {
        let fixture = connected_runner_with_re_adoptable_job(home, false);

        let (report, exit_code) = crate::connection::restart_daemon_in_place(
            RUNNER,
            &fixture.previous,
            fixture.homeboy.to_str().expect("binary path"),
        )
        .expect("restart in place");

        assert_eq!(exit_code, 0, "{report:?}");
        assert!(report.connected);
        assert_eq!(report.remote_daemon_pid, Some(NEW_PID));
        assert_eq!(
            report.remote_daemon_address.as_deref(),
            Some(fixture.daemon.address.as_str())
        );

        // The exact old lease was stopped, then ensure-running started the
        // daemon again on the same HOMEBOY_DAEMON_STATE_DIR.
        let state_dir = expected_state_dir();
        let invocations = daemon_invocations(&fixture.homeboy_log);
        let stop = invocations
            .iter()
            .position(|(_, argv)| argv == &format!("daemon stop --lease-id {OLD_LEASE}"))
            .unwrap_or_else(|| panic!("old lease was not stopped: {invocations:?}"));
        let ensure = invocations
            .iter()
            .position(|(_, argv)| argv.starts_with("daemon ensure-running"))
            .unwrap_or_else(|| panic!("ensure-running was not called: {invocations:?}"));
        assert!(stop < ensure, "the old lease stops before the restart");
        assert_eq!(
            invocations
                .iter()
                .filter(|(_, argv)| argv.starts_with("daemon ensure-running"))
                .count(),
            1
        );
        assert!(
            !invocations[ensure].1.contains("--replacement-operation-id"),
            "an in-place restart is not a daemon replacement: {invocations:?}"
        );
        assert!(
            invocations.iter().all(|(dir, _)| dir == &state_dir),
            "every daemon command used the one state dir {state_dir}: {invocations:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&fixture.lease_file).expect("lease"),
            NEW_LEASE
        );
        assert_single_state_dir();
        assert!(
            !fixture.systemctl_log.exists(),
            "a direct runner never touches a service unit"
        );

        let session = crate::connection::recorded_session(RUNNER)
            .expect("read session")
            .expect("restarted session");
        assert_eq!(session.remote_daemon_lease_id.as_deref(), Some(NEW_LEASE));
        assert_eq!(session.local_url, Some(fixture.daemon.url()));
        assert_owner_rebound_to_restarted_lease(&fixture);
        assert_observer_follows_job_to_terminal(&fixture);
    });
}

#[test]
fn service_refresh_repoints_and_restarts_the_unit_in_place_and_keeps_its_re_adoptable_job() {
    crate::test_support::with_isolated_home(|home| {
        let fixture = connected_runner_with_re_adoptable_job(home, true);
        let promoted = home.path().join("promoted-homeboy");
        std::fs::copy(&fixture.homeboy, &promoted).expect("promote binary");

        let (report, exit_code) = crate::connection::restart_daemon_in_place(
            RUNNER,
            &fixture.previous,
            promoted.to_str().expect("binary path"),
        )
        .expect("restart in place");

        assert_eq!(exit_code, 0, "{report:?}");
        assert!(report.connected);
        assert_eq!(report.remote_daemon_pid, Some(NEW_PID));

        // `service::repoint_and_restart` rewrote the unit onto the promoted
        // binary. The unit kills only the daemon process and keeps the same
        // state dir.
        let unit = crate::connection::runner_service_unit_name(RUNNER);
        let rendered =
            std::fs::read_to_string(home.path().join(".config/systemd/user").join(&unit))
                .expect("rendered unit");
        assert!(rendered.contains("KillMode=process"), "{rendered}");
        assert!(
            rendered.contains(&format!(
                "Environment=HOMEBOY_DAEMON_STATE_DIR=%h/{}",
                crate::connection::daemon_state_dir(RUNNER, &crate::connection::controller_id())
            )),
            "{rendered}"
        );
        let link = rendered
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart=%h/"))
            .and_then(|exec| exec.split_once(" daemon serve"))
            .map(|(link, _)| home.path().join(link))
            .expect("unit runs the stable binary link");
        assert_eq!(std::fs::read_link(link).expect("binary link"), promoted);
        let systemctl = std::fs::read_to_string(&fixture.systemctl_log).expect("systemctl log");
        assert!(
            systemctl
                .lines()
                .any(|line| line == format!("--user restart {unit}")),
            "{systemctl}"
        );
        assert!(
            !systemctl.lines().any(|line| line.contains(" stop ")),
            "{systemctl}"
        );

        // The controller neither stopped the daemon nor started one itself.
        // It reconnected to the unit's daemon through `connect_service_runner`,
        // which reads the daemon's status on the unit's state dir.
        let state_dir = expected_state_dir();
        let invocations = daemon_invocations(&fixture.homeboy_log);
        assert!(
            invocations
                .iter()
                .all(|(_, argv)| !argv.starts_with("daemon stop")
                    && !argv.starts_with("daemon ensure-running")),
            "the service restart interrupts nothing: {invocations:?}"
        );
        assert!(
            invocations
                .iter()
                .any(|(dir, argv)| dir == &state_dir && argv == "daemon status"),
            "{invocations:?}"
        );
        assert!(
            invocations.iter().all(|(dir, _)| dir == &state_dir),
            "{invocations:?}"
        );
        assert_single_state_dir();

        let session = crate::connection::recorded_session(RUNNER)
            .expect("read session")
            .expect("reconnected session");
        assert_eq!(session.remote_daemon_lease_id.as_deref(), Some(NEW_LEASE));
        assert_eq!(session.local_url, Some(fixture.daemon.url()));
        assert_owner_rebound_to_restarted_lease(&fixture);
        assert_observer_follows_job_to_terminal(&fixture);
    });
}
