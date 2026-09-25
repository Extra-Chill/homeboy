//! Runner-owned daemon service (#13881 step 3).
//!
//! The runner daemon runs as a systemd user unit on the runner host. The
//! controller installs the unit once. After that it only attaches: it waits
//! for the service's lease, opens a tunnel, and writes a session. It never
//! starts, replaces, rotates, or retires the daemon process.
//!
//! An upgrade repoints the unit's binary link and restarts the unit once no
//! job is active. The daemon's own startup recovers anything a previous owner
//! left behind, so a restart needs nothing from the controller.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::*;

/// How long the controller waits for the service daemon to publish a fresh,
/// reachable lease after installing or restarting the unit.
const SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(60);
const SERVICE_READY_POLL_INTERVAL: Duration = Duration::from_millis(500);
const SERVICE_SSH_TIMEOUT: Duration = Duration::from_secs(60);

/// The PATH the daemon ran with when the controller started it over SSH. The
/// unit keeps it so jobs see the same tools after the move to systemd.
const SERVICE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin";

#[derive(Debug, Clone, Serialize)]
pub struct RunnerServiceReport {
    pub runner_id: String,
    pub unit: String,
    pub binary: String,
    pub state_dir: String,
    pub active: bool,
    pub daemon_pid: Option<u32>,
    pub daemon_lease_id: Option<String>,
    pub daemon_address: Option<String>,
    pub daemon_build_identity: Option<String>,
    /// Generation daemons from the controller-managed era that install
    /// stopped. Each entry is the generation state directory.
    pub retired_generations: Vec<String>,
}

pub(crate) fn service_unit_name(runner_id: &str) -> String {
    format!(
        "homeboy-runner-{}.service",
        paths::sanitize_path_segment(runner_id)
    )
}

/// The stable binary link the unit executes, relative to `$HOME`.
fn service_binary_link(runner_id: &str) -> String {
    format!(
        ".local/share/homeboy/runner-service/{}/homeboy",
        paths::sanitize_path_segment(runner_id)
    )
}

/// The daemon state directory, relative to `$HOME`. It is the directory the
/// read-only `daemon status` probe already inspects for this runner.
fn service_state_dir(runner_id: &str) -> String {
    format!(
        ".config/homeboy/daemon-generations/{}/primary",
        paths::sanitize_path_segment(runner_id)
    )
}

pub(crate) fn render_service_unit(runner_id: &str) -> String {
    format!(
        r#"[Unit]
Description=Homeboy runner daemon ({runner_id})
After=network-online.target

[Service]
Type=simple
Environment=HOMEBOY_DAEMON_STATE_DIR=%h/{state_dir}
Environment=PATH={path}
ExecStart=%h/{link} daemon serve --addr 127.0.0.1:0
Restart=always
RestartSec=2s
TimeoutStopSec=30s

[Install]
WantedBy=default.target
"#,
        state_dir = service_state_dir(runner_id),
        path = SERVICE_PATH,
        link = service_binary_link(runner_id),
    )
}

/// Atomically point the unit's binary link at `binary`.
pub(crate) fn point_binary_script(runner_id: &str, binary: &str) -> String {
    format!(
        r#"link="$HOME/{link}"
mkdir -p "$(dirname "$link")"
ln -sfn {binary} "$link.next"
mv -f "$link.next" "$link""#,
        link = service_binary_link(runner_id),
        binary = shell::quote_arg(binary),
    )
}

pub(crate) fn install_script(runner_id: &str, binary: &str) -> String {
    let unit = service_unit_name(runner_id);
    format!(
        r#"set -eu
{point}
unit_dir="$HOME/.config/systemd/user"
mkdir -p "$unit_dir"
cat > "$unit_dir/{unit}.tmp" <<'HOMEBOY_UNIT'
{unit_text}HOMEBOY_UNIT
mv -f "$unit_dir/{unit}.tmp" "$unit_dir/{unit}"
systemctl --user daemon-reload
systemctl --user enable {unit}"#,
        point = point_binary_script(runner_id, binary),
        unit_text = render_service_unit(runner_id),
    )
}

/// Stop every idle generation daemon left by controller-managed rotation.
/// `daemon stop` refuses a daemon with active jobs, so busy generations stay.
fn retire_generations_script(runner_id: &str, homeboy: &str) -> String {
    format!(
        r#"for dir in "$HOME/.config/homeboy/daemon-generations/{segment}"/*/; do
  dir="${{dir%/}}"
  [ "$(basename "$dir")" = primary ] && continue
  if out=$(HOMEBOY_DAEMON_STATE_DIR="$dir" {homeboy} daemon stop 2>/dev/null) \
    && printf '%s' "$out" | grep -q '"stopped": *true'; then
    echo "$dir"
  fi
done"#,
        segment = paths::sanitize_path_segment(runner_id),
        homeboy = shell::quote_arg(homeboy),
    )
}

fn run_remote(client: &SshClient, script: &str, what: &str) -> Result<String> {
    let output = client.execute_with_timeout(script, SERVICE_SSH_TIMEOUT);
    if output.success {
        return Ok(output.stdout);
    }
    Err(Error::internal_unexpected(format!(
        "{what} failed on the runner: {}",
        output.stderr.trim()
    )))
}

struct ServiceTarget {
    runner: Runner,
    server_id: String,
    server: Server,
    client: SshClient,
}

fn service_target(roots: &paths::PathRoots, runner_id: &str) -> Result<ServiceTarget> {
    let runner = load_in_roots(roots, runner_id)?;
    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "the runner service requires an SSH runner",
            Some(runner_id.to_string()),
            None,
        ));
    };
    Ok(ServiceTarget {
        runner,
        server_id,
        server,
        client,
    })
}

/// Poll the read-only daemon status until the service daemon holds a fresh,
/// reachable, loopback lease.
fn wait_for_service_daemon(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
) -> std::result::Result<RemoteDaemon, String> {
    let deadline = Instant::now() + SERVICE_READY_TIMEOUT;
    let mut last: String;
    loop {
        match remote_daemon_status(client, homeboy, runner_id) {
            Ok(status) => match status.daemon {
                Some(daemon)
                    if status.fresh
                        && status.reachable
                        && daemon.pid.is_some()
                        && daemon.lease_id.as_deref().is_some_and(|id| !id.is_empty())
                        && parse_loopback_daemon_addr(&daemon.address).is_ok() =>
                {
                    return Ok(daemon)
                }
                Some(_) => {
                    last = format!(
                        "service daemon lease is not ready (fresh: {}, reachable: {}, stale reason: {})",
                        status.fresh,
                        status.reachable,
                        status.stale_reason.as_deref().unwrap_or("none"),
                    )
                }
                None => last = "service daemon has not published a lease".to_string(),
            },
            Err(error) => last = error,
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{last}. Inspect `systemctl --user status {}` on the runner.",
                service_unit_name(runner_id)
            ));
        }
        std::thread::sleep(SERVICE_READY_POLL_INTERVAL);
    }
}

fn report_for(
    runner_id: &str,
    binary: &str,
    daemon: Option<&RemoteDaemon>,
    retired_generations: Vec<String>,
) -> RunnerServiceReport {
    RunnerServiceReport {
        runner_id: runner_id.to_string(),
        unit: service_unit_name(runner_id),
        binary: binary.to_string(),
        state_dir: format!("~/{}", service_state_dir(runner_id)),
        active: daemon.is_some(),
        daemon_pid: daemon.and_then(|daemon| daemon.pid),
        daemon_lease_id: daemon.and_then(|daemon| daemon.lease_id.clone()),
        daemon_address: daemon.map(|daemon| daemon.address.clone()),
        daemon_build_identity: daemon.and_then(|daemon| daemon.build_identity.clone()),
        retired_generations,
    }
}

/// Install the runner daemon as a systemd user unit and hand the daemon over
/// to it. Requires an idle runner: the controller-started daemon is stopped
/// before the unit takes over its state directory.
pub fn install(runner_id: &str) -> Result<RunnerServiceReport> {
    let roots = paths::PathRoots::from_environment()?;
    let target = service_target(&roots, runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner service install")?;
    let client = &target.client;

    let status = remote_daemon_status(client, homeboy, runner_id).map_err(|error| {
        Error::internal_unexpected(format!("read runner daemon status: {error}"))
    })?;
    if status.active_jobs > 0 {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "runner `{runner_id}` has {} active job(s); install the service once it is idle",
                status.active_jobs
            ),
            Some(runner_id.to_string()),
            None,
        ));
    }

    run_remote(
        client,
        &install_script(runner_id, homeboy),
        "writing the runner service unit",
    )?;
    let retired_generations = run_remote(
        client,
        &retire_generations_script(runner_id, homeboy),
        "retiring generation daemons",
    )?
    .lines()
    .map(str::to_string)
    .collect();
    if let Some(lease_id) = status
        .daemon
        .as_ref()
        .and_then(|daemon| daemon.lease_id.as_deref())
    {
        // The controller-started daemon still owns the state directory. It is
        // idle, so stop it and let the unit take the owner lock.
        remote_daemon_force_stop(client, homeboy, runner_id, lease_id)
            .map_err(Error::internal_unexpected)?;
    }
    run_remote(
        client,
        &format!("systemctl --user restart {}", service_unit_name(runner_id)),
        "starting the runner service",
    )?;
    let daemon =
        wait_for_service_daemon(client, homeboy, runner_id).map_err(Error::internal_unexpected)?;

    super::super::merge(Some(runner_id), r#"{"service_managed":true}"#, &[])?;
    Ok(report_for(
        runner_id,
        homeboy,
        Some(&daemon),
        retired_generations,
    ))
}

/// Report the runner service and the daemon lease it currently holds.
pub fn service_status(runner_id: &str) -> Result<RunnerServiceReport> {
    let roots = paths::PathRoots::from_environment()?;
    let target = service_target(&roots, runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner service status")?;
    let daemon = remote_daemon_status(&target.client, homeboy, runner_id)
        .ok()
        .filter(|status| status.fresh && status.reachable)
        .and_then(|status| status.daemon);
    Ok(report_for(runner_id, homeboy, daemon.as_ref(), Vec::new()))
}

/// Point the service at `binary` and restart it. Callers first prove the
/// runner is idle (or explicitly force the restart), because restarting the
/// unit stops the daemon's child processes.
pub(crate) fn repoint_and_restart(
    roots: &paths::PathRoots,
    runner_id: &str,
    binary: &str,
) -> Result<()> {
    let target = service_target(roots, runner_id)?;
    run_remote(
        &target.client,
        &format!(
            "set -eu\n{}\nsystemctl --user restart {}",
            point_binary_script(runner_id, binary),
            service_unit_name(runner_id)
        ),
        "restarting the runner service",
    )?;
    Ok(())
}

/// Attach to the runner's service daemon: wait for its lease, open the tunnel,
/// and write the session. Nothing on the runner is started or stopped.
pub(crate) fn connect_service_runner(
    roots: &paths::PathRoots,
    runner_id: &str,
) -> Result<(RunnerConnectReport, i32)> {
    let session_path = session_path_in_root(roots.config(), runner_id);
    let target = service_target(roots, runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner connect")?;
    let client = &target.client;

    let identity = match remote_homeboy_identity(client, homeboy) {
        Ok(identity) => identity,
        Err(message) => {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::MissingRemoteHomeboy,
                message,
            ))
        }
    };
    let Some(expected_identity) = identity.build_identity.clone() else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            "the runner's homeboy did not report a build identity".to_string(),
        ));
    };
    let daemon = match wait_for_service_daemon(client, homeboy, runner_id) {
        Ok(daemon) => daemon,
        Err(message) => {
            return Ok(failed_connect(
                runner_id,
                session_path,
                RunnerFailureKind::DaemonStartupFailure,
                message,
            ))
        }
    };
    if daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim()) {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            format!(
                "the runner service runs `{}` but the runner's configured homeboy is `{expected_identity}`. Run `homeboy runner refresh-homeboy {} --reconnect`.",
                daemon.build_identity.as_deref().unwrap_or("unknown"),
                shell::quote_arg(runner_id),
            ),
        ));
    }

    let expected_version = daemon.version.clone().unwrap_or(identity.version.clone());
    let (local_port, tunnel_pid, tunnel_process_start_identity, local_url, daemon) =
        match connect_remote_daemon(connection_daemon::RemoteDaemonConnectRequest {
            server: &target.server,
            homeboy,
            daemon,
            expected_version: &expected_version,
            expected_identity: &expected_identity,
            runner_id,
            session_path: &session_path,
        }) {
            Ok(connection) => connection,
            Err(report) => return Ok(*report),
        };

    if let Some(previous) = read_session(runner_id)? {
        if previous.tunnel_pid != tunnel_pid {
            terminate_tunnel_if_owned(&previous);
        }
    }
    let session = RunnerSession {
        runner_id: runner_id.to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(target.server_id),
        controller_id: Some(controller_id()),
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        tunnel_process_start_identity,
        proxy_forward: None,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id: daemon.lease_id,
        homeboy_version: expected_version,
        homeboy_build_identity: Some(expected_identity),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };
    super::super::generation_store::promote_pending_replacement(runner_id, &session)?;
    write_session(&session)?;

    super::super::runner_probe_gate::invalidate_runner_probes(runner_id);
    wake_unmaterialized_admission_reconciliation();
    Ok((
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
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
            connection_warning: None,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            leaseless_recovery: None,
            state_loss_recovery: None,
            leaseless_recovery_evidence: None,
            failure_kind: None,
            failure_message: None,
            failure_evidence: None,
        },
        0,
    ))
}

/// Detach from a service runner: close the tunnel and remove the session. The
/// daemon and its jobs keep running under the service.
pub(crate) fn detach_service_runner(runner_id: &str) -> Result<RunnerDisconnectReport> {
    let session_path = session_path(runner_id)?.display().to_string();
    let Some(session) = read_session(runner_id)? else {
        return Ok(RunnerDisconnectReport {
            runner_id: runner_id.to_string(),
            disconnected: true,
            partial: false,
            remote_error: None,
            local_recovery_command: None,
            session: None,
            session_path,
        });
    };
    terminate_tunnel_if_owned(&session);
    let removed = remove_session_if_matches(runner_id, &session)?;
    if removed {
        let _ = remove_ownership_if_matches(runner_id, &session)?;
    }
    Ok(RunnerDisconnectReport {
        runner_id: runner_id.to_string(),
        disconnected: removed,
        partial: false,
        remote_error: None,
        local_recovery_command: None,
        session: (!removed).then_some(session),
        session_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_serve_from_the_stable_link_in_the_runner_state_dir() {
        let unit = render_service_unit("homeboy-lab");
        assert!(unit.contains(
            "ExecStart=%h/.local/share/homeboy/runner-service/homeboy-lab/homeboy daemon serve --addr 127.0.0.1:0"
        ));
        assert!(unit.contains(
            "Environment=HOMEBOY_DAEMON_STATE_DIR=%h/.config/homeboy/daemon-generations/homeboy-lab/primary"
        ));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
        assert_eq!(
            service_unit_name("homeboy-lab"),
            "homeboy-runner-homeboy-lab.service"
        );
    }

    #[test]
    fn scripts_run_under_a_real_shell_and_point_the_link_atomically() {
        let home = tempfile::tempdir().expect("home");
        let first = home.path().join("bin-a");
        let second = home.path().join("bin-b");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&second, "b").unwrap();
        for binary in [&first, &second] {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "set -eu\n{}",
                    point_binary_script("homeboy-lab", &binary.display().to_string())
                ))
                .env("HOME", home.path())
                .output()
                .expect("run point script");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let link = home
                .path()
                .join(".local/share/homeboy/runner-service/homeboy-lab/homeboy");
            assert_eq!(std::fs::read_link(&link).unwrap(), *binary);
        }
    }

    #[test]
    fn retire_script_reports_only_generations_it_actually_stopped() {
        let home = tempfile::tempdir().expect("home");
        let generations = home
            .path()
            .join(".config/homeboy/daemon-generations/homeboy-lab");
        for name in ["primary", "live", "idle"] {
            std::fs::create_dir_all(generations.join(name)).unwrap();
        }
        let fake = home.path().join("homeboy");
        std::fs::write(
            &fake,
            "#!/bin/sh\ncase \"$HOMEBOY_DAEMON_STATE_DIR\" in\n  */live) echo '{\"data\": {\"stopped\": true}}' ;;\n  *) echo '{\"data\": {\"stopped\": false}}' ;;\nesac\n",
        )
        .unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&fake)
            .status()
            .unwrap();
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(retire_generations_script(
                "homeboy-lab",
                &fake.display().to_string(),
            ))
            .env("HOME", home.path())
            .output()
            .expect("run retire script");
        assert!(output.status.success());
        let stopped = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stopped.trim(),
            generations.join("live").display().to_string()
        );
    }

    #[test]
    fn install_script_writes_the_rendered_unit_verbatim() {
        let script = install_script("homeboy-lab", "/opt/homeboy");
        let start = script.find("<<'HOMEBOY_UNIT'\n").unwrap() + "<<'HOMEBOY_UNIT'\n".len();
        let end = script.find("HOMEBOY_UNIT\nmv").unwrap();
        assert_eq!(&script[start..end], render_service_unit("homeboy-lab"));
        assert!(script.ends_with("systemctl --user enable homeboy-runner-homeboy-lab.service"));
    }
}
