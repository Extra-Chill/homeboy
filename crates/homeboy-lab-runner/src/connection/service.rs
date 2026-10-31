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

/// System directories every service PATH ends with. They are also the whole
/// PATH when the runner user's login shell cannot report one.
const SERVICE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin";

/// Placeholder in the rendered unit that the install script replaces with the
/// runner user's login-shell PATH. systemd user units do not read shell
/// profiles, so without this, toolchains installed per user (node/npm in
/// `~/.local/bin`, cargo in `~/.cargo/bin`) are invisible to every job and
/// dependency builds fail before a gate runs (Extra-Chill/homeboy#15391).
const SERVICE_PATH_PLACEHOLDER: &str = "@HOMEBOY_SERVICE_PATH@";

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
    /// Leases of the idle pre-#16752 legacy-layout daemons that
    /// install stopped (see `legacy_layout`).
    pub stopped_legacy_leases: Vec<String>,
}

pub(crate) fn service_unit_name(runner_id: &str) -> String {
    service_unit_name_for(runner_id, &controller_id())
}

fn service_unit_name_for(runner_id: &str, controller_id: &str) -> String {
    format!(
        "homeboy-runner-{}-{}.service",
        paths::sanitize_path_segment(runner_id),
        super::controller_scope_segment(controller_id)
    )
}

/// The stable binary link the unit executes, relative to `$HOME`.
fn service_binary_link_for(runner_id: &str, controller_id: &str) -> String {
    format!(
        ".local/share/homeboy/runner-service/{}/controllers/{}/homeboy",
        paths::sanitize_path_segment(runner_id),
        super::controller_scope_segment(controller_id)
    )
}

/// The daemon state directory, relative to `$HOME`. It is the directory the
/// read-only `daemon status` probe already inspects for this runner.
fn service_state_dir(runner_id: &str) -> String {
    super::daemon_state_dir(runner_id, &controller_id())
}

#[cfg(test)]
fn controller_segment() -> String {
    super::controller_scope_segment(&controller_id())
}

#[cfg(test)]
pub(crate) fn render_service_unit(runner_id: &str, startup_token: &str) -> String {
    render_service_unit_for(runner_id, &controller_id(), startup_token)
}

#[cfg(test)]
pub(super) fn render_service_unit_for(
    runner_id: &str,
    controller_id: &str,
    startup_token: &str,
) -> String {
    render_service_unit_with_env(runner_id, controller_id, startup_token, &[])
}

/// The runner-owned resource-guard policy entries from a runner's `env`,
/// sorted. The daemon reads these from its own process environment, never
/// from a job's env, so setting them in the runner's `env`
/// (`homeboy runner set <id> --json '{"env":{...}}'`) renders them into the
/// service unit on the next install or repoint (#15751).
/// Only allowlisted keys with plain non-negative integer values are rendered,
/// so nothing a runner config holds can inject into a systemd unit line.
pub(crate) fn daemon_policy_env(
    env: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> =
        homeboy_runner_contract::RESOURCE_GUARD_POLICY_ENV_KEYS
            .iter()
            .filter_map(|key| {
                let value = env.get(*key)?.trim();
                (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| ((*key).to_string(), value.to_string()))
            })
            .collect();
    entries.sort();
    entries
}

fn render_service_unit_with_env(
    runner_id: &str,
    controller_id: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    let policy_env: String = daemon_env
        .iter()
        .map(|(key, value)| format!("Environment={key}={value}\n"))
        .collect();
    format!(
        r#"[Unit]
Description=Homeboy runner daemon ({runner_id})
After=network-online.target

[Service]
Type=simple
Environment=HOMEBOY_DAEMON_STATE_DIR=%h/{state_dir}
Environment={token_env}={startup_token}
Environment=PATH={path}
{policy_env}ExecStart=%h/{link} daemon serve --addr 127.0.0.1:0
Restart=always
RestartSec=2s
TimeoutStopSec=30s
KillMode=process

[Install]
WantedBy=default.target
"#,
        state_dir = super::daemon_state_dir(runner_id, controller_id),
        token_env = paths::DAEMON_STARTUP_TOKEN_ENV,
        path = SERVICE_PATH_PLACEHOLDER,
        link = service_binary_link_for(runner_id, controller_id),
    )
}

#[cfg(test)]
/// Atomically point the unit's binary link at `binary`.
pub(crate) fn point_binary_script(runner_id: &str, binary: &str) -> String {
    point_binary_script_for(runner_id, &controller_id(), binary)
}

fn point_binary_script_for(runner_id: &str, controller_id: &str, binary: &str) -> String {
    format!(
        r#"link="$HOME/{link}"
mkdir -p "$(dirname "$link")"
ln -sfn {binary} "$link.next"
mv -f "$link.next" "$link""#,
        link = service_binary_link_for(runner_id, controller_id),
        binary = shell::quote_arg(binary),
    )
}

#[cfg(test)]
/// Write the rendered unit and reload systemd's view of it. Shared by
/// install (which also enables and starts the unit for the first time) and
/// repoint-and-restart (which only needs the file on disk to be current
/// before its own restart), so a repoint always carries the daemon's startup
/// token even when the unit predates this being rendered (#15087).
fn write_unit_script(runner_id: &str, startup_token: &str) -> String {
    write_unit_script_for(runner_id, &controller_id(), startup_token)
}

#[cfg(test)]
fn write_unit_script_for(runner_id: &str, controller_id: &str, startup_token: &str) -> String {
    write_unit_script_with_env(runner_id, controller_id, startup_token, &[])
}

fn write_unit_script_with_env(
    runner_id: &str,
    controller_id: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    let unit = service_unit_name_for(runner_id, controller_id);
    format!(
        r#"unit_dir="$HOME/.config/systemd/user"
mkdir -p "$unit_dir"
cat > "$unit_dir/{unit}.tmp" <<'HOMEBOY_UNIT'
{unit_text}HOMEBOY_UNIT
{resolve_path}
sed -i "s|{placeholder}|$service_path|" "$unit_dir/{unit}.tmp"
mv -f "$unit_dir/{unit}.tmp" "$unit_dir/{unit}"
systemctl --user daemon-reload"#,
        unit_text =
            render_service_unit_with_env(runner_id, controller_id, startup_token, daemon_env),
        resolve_path = resolve_service_path_script(),
        placeholder = SERVICE_PATH_PLACEHOLDER,
    )
}

/// Shell that sets `$service_path` to the runner user's login-shell PATH
/// followed by the system directories. A login PATH that is empty or carries
/// characters unsafe in a systemd `Environment=` line (whitespace, quotes,
/// `%`, `|`) falls back to the system directories alone.
fn resolve_service_path_script() -> String {
    format!(
        r#"login_path=$("${{SHELL:-/bin/sh}}" -lc 'printf %s "$PATH"' 2>/dev/null </dev/null || true)
case "$login_path" in
  ""|*[!A-Za-z0-9_./:+@=,~-]*) service_path='{system}' ;;
  *) service_path="$login_path:{system}" ;;
esac"#,
        system = SERVICE_PATH,
    )
}

#[cfg(test)]
pub(crate) fn install_script(runner_id: &str, binary: &str, startup_token: &str) -> String {
    install_script_for(runner_id, &controller_id(), binary, startup_token)
}

#[cfg(test)]
fn install_script_for(
    runner_id: &str,
    controller_id: &str,
    binary: &str,
    startup_token: &str,
) -> String {
    install_script_with_env(runner_id, controller_id, binary, startup_token, &[])
}

fn install_script_with_env(
    runner_id: &str,
    controller_id: &str,
    binary: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    let unit = service_unit_name_for(runner_id, controller_id);
    format!(
        r#"set -eu
{point}
{write_unit}
systemctl --user enable {unit}"#,
        point = point_binary_script_for(runner_id, controller_id, binary),
        write_unit =
            write_unit_script_with_env(runner_id, controller_id, startup_token, daemon_env),
    )
}

#[cfg(test)]
fn repoint_script_for(
    runner_id: &str,
    controller_id: &str,
    binary: &str,
    startup_token: &str,
) -> String {
    repoint_script_with_env(runner_id, controller_id, binary, startup_token, &[])
}

fn repoint_script_with_env(
    runner_id: &str,
    controller_id: &str,
    binary: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    let unit = service_unit_name_for(runner_id, controller_id);
    format!(
        "set -eu\n{point}\n{write_unit}\nsystemctl --user enable {unit}\nsystemctl --user restart {unit}",
        point = point_binary_script_for(runner_id, controller_id, binary),
        write_unit = write_unit_script_with_env(runner_id, controller_id, startup_token, daemon_env),
    )
}

/// The one-time legacy-layout step for a service runner, run before the unit
/// (re)starts its daemon in the new state dir. A unit written by older
/// Homeboy still points at a legacy dir; rewriting it here moves it.
fn move_off_legacy_layout(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
) -> Result<Vec<String>> {
    let stopped = super::legacy_layout::stop_remote_legacy_daemons(client, homeboy, runner_id)
        .map_err(|error| error.into_error(runner_id))?;
    super::legacy_layout::forget_stopped_legacy_session(runner_id, &stopped)?;
    Ok(stopped)
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

fn service_target(runner_id: &str) -> Result<ServiceTarget> {
    let runner = load(runner_id)?;
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
    stopped_legacy_leases: Vec<String>,
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
        stopped_legacy_leases,
    }
}

/// Install the runner daemon as a systemd user unit and hand the daemon over
/// to it. Requires an idle runner: the controller-started daemon is stopped
/// before the unit takes over its state directory.
pub fn install(runner_id: &str) -> Result<RunnerServiceReport> {
    let target = service_target(runner_id)?;
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

    let stopped_legacy_leases = move_off_legacy_layout(client, homeboy, runner_id)?;
    run_remote(
        client,
        &install_script_with_env(
            runner_id,
            &controller_id(),
            homeboy,
            &uuid::Uuid::new_v4().to_string(),
            &daemon_policy_env(&target.runner.env),
        ),
        "writing the runner service unit",
    )?;
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
        stopped_legacy_leases,
    ))
}

/// Report the runner service and the daemon lease it currently holds.
pub fn service_status(runner_id: &str) -> Result<RunnerServiceReport> {
    let target = service_target(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner service status")?;
    let daemon = remote_daemon_status(&target.client, homeboy, runner_id)
        .ok()
        .filter(|status| status.fresh && status.reachable)
        .and_then(|status| status.daemon);
    Ok(report_for(runner_id, homeboy, daemon.as_ref(), Vec::new()))
}

/// Point the service at `binary` and restart it in place. The unit keeps its
/// state directory and stops only the daemon process (`KillMode=process`), so
/// the restarted daemon re-adopts every job with a recorded child identity.
/// Callers prove every active job is re-adoptable, or explicitly force it.
///
/// The unit file is rewritten (not just the binary link) on every repoint, so
/// a unit installed before the daemon's startup token was rendered self-heals
/// here rather than staying stuck with the empty token an older install left
/// behind (#15087).
pub(crate) fn repoint_and_restart(runner_id: &str, binary: &str) -> Result<()> {
    let target = service_target(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner service restart")?;
    move_off_legacy_layout(&target.client, homeboy, runner_id)?;
    let startup_token = uuid::Uuid::new_v4().to_string();
    run_remote(
        &target.client,
        &repoint_script_with_env(
            runner_id,
            &controller_id(),
            binary,
            &startup_token,
            &daemon_policy_env(&target.runner.env),
        ),
        "restarting the runner service",
    )?;
    Ok(())
}

/// Marker that prefixes every inventory record, so unrelated stdout (login
/// banners, shell noise) can never be parsed as a unit.
const SERVICE_INVENTORY_MARKER: &str = "homeboy-runner-service";

/// One systemd user unit on the runner host that runs a Homeboy daemon for
/// this runner, as observed read-only (#15733).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceUnitObservation {
    pub unit: String,
    pub binary: String,
    /// First line of `<ExecStart binary> --version`.
    pub binary_version: Option<String>,
    /// First line of `/proc/<MainPID>/exe --version`: what actually runs.
    pub running_version: Option<String>,
    pub active_state: Option<String>,
}

/// Classify a unit against this runner's naming: this controller's scoped
/// unit, another controller's scoped unit, or `None` for any other unit. That
/// includes a unit for a different runner whose id merely shares a prefix and
/// an unscoped `homeboy-runner-<runner>.service`, which Homeboy no longer
/// manages.
pub(crate) fn service_unit_scope(runner_id: &str, unit: &str) -> Option<&'static str> {
    service_unit_scope_for(runner_id, &controller_id(), unit)
}

fn service_unit_scope_for(
    runner_id: &str,
    controller_id: &str,
    unit: &str,
) -> Option<&'static str> {
    if unit == service_unit_name_for(runner_id, controller_id) {
        return Some("controller");
    }
    let scoped = unit
        .strip_prefix(&format!(
            "homeboy-runner-{}-",
            paths::sanitize_path_segment(runner_id)
        ))?
        .strip_suffix(".service")?;
    // A controller scope segment always ends in `-<sha256 hex>`; anything
    // else is another runner such as `<runner>-2`.
    let (_, digest) = scoped.rsplit_once('-')?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some("other_controller")
}

/// Read-only shell that lists every controller-scoped runner unit for
/// `runner_id` with
/// the binary its `ExecStart` runs, that binary's version, the version of the
/// executable the main process is running, and the unit's active state. It
/// never starts, stops, reloads, or rewrites anything.
pub(crate) fn service_inventory_script(runner_id: &str) -> String {
    format!(
        r#"unit_dir="$HOME/.config/systemd/user"
for unit_path in "$unit_dir"/homeboy-runner-{segment}-*.service; do
  [ -f "$unit_path" ] || continue
  unit=$(basename "$unit_path")
  exec_line=$(sed -n 's/^ExecStart=//p' "$unit_path" | tail -n 1)
  binary=${{exec_line%% *}}
  case "$binary" in %h/*) binary="$HOME/${{binary#%h/}}" ;; esac
  binary_version=$("$binary" --version 2>/dev/null </dev/null | head -n 1) || binary_version=
  active=$(systemctl --user is-active "$unit" 2>/dev/null </dev/null) || true
  pid=$(systemctl --user show -p MainPID --value "$unit" 2>/dev/null </dev/null) || pid=
  running_version=
  if [ -n "$pid" ] && [ "$pid" != 0 ] && [ -e "/proc/$pid/exe" ]; then
    running_version=$("/proc/$pid/exe" --version 2>/dev/null </dev/null | head -n 1) || running_version=
  fi
  printf '{marker}\t%s\t%s\t%s\t%s\t%s\n' "$unit" "$binary" "$binary_version" "$running_version" "$active"
done"#,
        segment = paths::sanitize_path_segment(runner_id),
        marker = SERVICE_INVENTORY_MARKER,
    )
}

pub(crate) fn parse_service_inventory(stdout: &str) -> Vec<ServiceUnitObservation> {
    let present = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            if fields.next()? != SERVICE_INVENTORY_MARKER {
                return None;
            }
            let unit = present(fields.next()?)?;
            let binary = fields.next().unwrap_or_default().trim().to_string();
            Some(ServiceUnitObservation {
                unit,
                binary,
                binary_version: fields.next().and_then(present),
                running_version: fields.next().and_then(present),
                active_state: fields.next().and_then(present),
            })
        })
        .collect()
}

/// Observe every runner-service unit on the runner host over SSH, read-only.
/// Non-SSH runners have no runner-owned services and report none.
pub(crate) fn observe_service_units(runner_id: &str) -> Result<Vec<ServiceUnitObservation>> {
    let runner = load(runner_id)?;
    let Some((_, _, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(Vec::new());
    };
    let stdout = run_remote(
        &client,
        &service_inventory_script(runner_id),
        "inspecting runner service binaries",
    )?;
    Ok(parse_service_inventory(&stdout))
}

/// First line of `<homeboy_path> --version` on the runner host, read-only over
/// SSH. `upgrade --check` uses this instead of a runner job so a check never
/// records work on the runner.
pub(crate) fn probe_runner_binary_version(
    runner: &Runner,
    homeboy_path: &str,
) -> Result<Option<String>> {
    let Some((_, _, client)) = resolve_ssh_runner(runner)? else {
        return Ok(None);
    };
    let stdout = run_remote(
        &client,
        &format!("{} --version </dev/null", shell::quote_arg(homeboy_path)),
        "probing the runner's selected homeboy",
    )?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string))
}

/// Attach to the runner's service daemon: wait for its lease, open the tunnel,
/// and write the session. Nothing on the runner is started or stopped.
pub(crate) fn connect_service_runner(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    let session_path = session_path(runner_id)?;
    let target = service_target(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner connect")?;
    let client = &target.client;

    let identity = match remote_homeboy_identity(client, homeboy) {
        Ok(identity) => identity,
        Err(message) => {
            let detail = message.clone();
            return Ok(remote_connect_failure(
                runner_id,
                session_path,
                &target.server_id,
                &target.server.host,
                -1,
                &detail,
                RunnerFailureKind::MissingRemoteHomeboy,
                message,
            ));
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
    super::replacement_journal::promote_pending_replacement(runner_id, &session)?;
    write_session(&session)?;

    super::super::runner_probe_gate::invalidate_runner_probes(runner_id);
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
        let unit = render_service_unit("homeboy-lab", "test-startup-token");
        assert!(unit.contains(&format!(
            "ExecStart=%h/.local/share/homeboy/runner-service/homeboy-lab/controllers/{}/homeboy daemon serve --addr 127.0.0.1:0",
            controller_segment()
        )));
        assert!(unit.contains(
            &format!("Environment=HOMEBOY_DAEMON_STATE_DIR=%h/.config/homeboy/runner-daemons/homeboy-lab/{}\n", controller_segment())
        ));
        assert!(unit.contains("Restart=always"));
        // A restart stops only the daemon; re-adoptable job children lead
        // their own sessions and must outlive it.
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("WantedBy=default.target"));
        assert_eq!(
            service_unit_name("homeboy-lab"),
            format!(
                "homeboy-runner-homeboy-lab-{}.service",
                controller_segment()
            )
        );
    }

    #[test]
    fn unit_renders_runner_owned_guard_policy_from_runner_env() {
        let env: std::collections::HashMap<String, String> = [
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT", " 1024 "),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES", "68719476736"),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT", "512"),
            ("HOMEBOY_COMMAND", "/opt/homeboy"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let policy = daemon_policy_env(&env);
        assert_eq!(
            policy,
            vec![
                (
                    "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT".to_string(),
                    "1024".to_string()
                ),
                (
                    "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT".to_string(),
                    "512".to_string()
                ),
                (
                    "HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES".to_string(),
                    "68719476736".to_string()
                ),
            ],
            "only allowlisted runner-owned policy keys are rendered"
        );

        let unit = render_service_unit_with_env("homeboy-lab", "controller-a", "t", &policy);
        assert!(
            unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT=1024\n")
        );
        assert!(
            unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES=68719476736\n")
        );
        assert!(unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT=512\n"));
        assert!(!unit.contains("HOMEBOY_COMMAND"));
        assert!(unit.contains(
            "Environment=HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES=68719476736\nExecStart="
        ));
        assert_eq!(
            render_service_unit_with_env("homeboy-lab", "controller-a", "t", &[]),
            render_service_unit_for("homeboy-lab", "controller-a", "t"),
            "a runner without policy env renders the unit unchanged"
        );
    }

    #[test]
    fn daemon_policy_env_rejects_values_that_could_inject_into_the_unit() {
        let env: std::collections::HashMap<String, String> = [
            (
                "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT",
                "1024\nExecStartPre=/bin/evil",
            ),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES", "-1"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        assert!(daemon_policy_env(&env).is_empty());
    }

    #[test]
    fn repoint_and_install_scripts_carry_runner_policy_env() {
        let policy = vec![(
            "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT".to_string(),
            "1024".to_string(),
        )];
        for script in [
            install_script_with_env("homeboy-lab", "controller-a", "/opt/homeboy", "t", &policy),
            repoint_script_with_env("homeboy-lab", "controller-a", "/opt/homeboy", "t", &policy),
        ] {
            assert!(
                script.contains("Environment=HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT=1024")
            );
        }
    }

    /// The service-owned daemon must publish an attributable startup token in
    /// its own lease. Without this, `daemon status`/`ensure-running`/
    /// `reconcile-unleased-candidates` can never tell this process apart from
    /// an unrelated foreground `daemon serve`, and every controller operation
    /// that inspects ownership fails closed (#15087).
    #[test]
    fn unit_carries_the_daemon_startup_token_env_var() {
        let unit = render_service_unit("homeboy-lab", "a-fresh-token");
        assert!(unit.contains(&format!(
            "Environment={}=a-fresh-token",
            paths::DAEMON_STARTUP_TOKEN_ENV
        )));
    }

    #[test]
    fn each_install_and_repoint_gets_its_own_fresh_startup_token() {
        let first = render_service_unit("homeboy-lab", "token-a");
        let second = render_service_unit("homeboy-lab", "token-b");
        assert!(first.contains("token-a"));
        assert!(!first.contains("token-b"));
        assert!(second.contains("token-b"));
        assert!(!second.contains("token-a"));
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
            let link = home.path().join(format!(
                ".local/share/homeboy/runner-service/homeboy-lab/controllers/{}/homeboy",
                controller_segment()
            ));
            assert_eq!(std::fs::read_link(&link).unwrap(), *binary);
        }
    }

    #[test]
    fn installing_controller_b_touches_only_b_service_binary_and_state() {
        let home = tempfile::tempdir().expect("shared runner home");
        let a_unit = service_unit_name_for("homeboy-lab", "controller-a");
        let b_unit = service_unit_name_for("homeboy-lab", "controller-b");
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        let a_unit_path = unit_dir.join(&a_unit);
        std::fs::write(&a_unit_path, "A unit remains active\n").unwrap();

        let a_binary = home.path().join("homeboy-a");
        let b_binary = home.path().join("homeboy-b");
        std::fs::write(&a_binary, "A binary").unwrap();
        std::fs::write(&b_binary, "B binary").unwrap();
        let a_link = home
            .path()
            .join(service_binary_link_for("homeboy-lab", "controller-a"));
        std::fs::create_dir_all(a_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&a_binary, &a_link).unwrap();

        let a_state = home
            .path()
            .join(daemon_state_dir("homeboy-lab", "controller-a"));
        std::fs::create_dir_all(&a_state).unwrap();
        std::fs::write(a_state.join("state.json"), r#"{"lease_id":"lease-a"}"#).unwrap();
        std::fs::write(a_state.join("termination.json"), r#"{"pid":1}"#).unwrap();
        let a_state_before = std::fs::read_dir(&a_state)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();

        // systemctl is a harmless fixture: it records only the requested unit
        // names and never contacts a host or stops a service.
        let bin_dir = home.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let systemctl = bin_dir.join("systemctl");
        homeboy_test_support::write_executable(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/systemctl.log\"\n",
            0o755,
        )
        .unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(if cfg!(target_os = "macos") {
                install_script_for(
                    "homeboy-lab",
                    "controller-b",
                    &b_binary.display().to_string(),
                    "token-b",
                )
                .replace("sed -i ", "sed -i '' ")
            } else {
                install_script_for(
                    "homeboy-lab",
                    "controller-b",
                    &b_binary.display().to_string(),
                    "token-b",
                )
            })
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .status()
            .unwrap();
        assert!(status.success());

        let b_unit_path = unit_dir.join(&b_unit);
        let b_link = home
            .path()
            .join(service_binary_link_for("homeboy-lab", "controller-b"));
        assert!(b_unit_path.exists());
        assert_eq!(std::fs::read_link(&b_link).unwrap(), b_binary);
        assert_eq!(
            std::fs::read(&a_unit_path).unwrap(),
            b"A unit remains active\n"
        );
        assert_eq!(std::fs::read_link(&a_link).unwrap(), a_binary);
        let a_state_after = std::fs::read_dir(&a_state)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            a_state_after, a_state_before,
            "A's lease and active-job state are untouched"
        );

        let commands = std::fs::read_to_string(home.path().join("systemctl.log")).unwrap();
        assert!(commands.contains(&format!("enable {b_unit}")));
        assert!(!commands.contains(&a_unit));
        let b_rendered = std::fs::read_to_string(b_unit_path).unwrap();
        assert!(b_rendered.contains(&format!(
            "HOMEBOY_DAEMON_STATE_DIR=%h/{}",
            daemon_state_dir("homeboy-lab", "controller-b")
        )));
        assert!(!b_rendered.contains(&daemon_state_dir("homeboy-lab", "controller-a")));

        let b_replacement = home.path().join("homeboy-b-refreshed");
        std::fs::write(&b_replacement, "B refreshed binary").unwrap();
        let repoint = repoint_script_for(
            "homeboy-lab",
            "controller-b",
            &b_replacement.display().to_string(),
            "token-b-refresh",
        );
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(if cfg!(target_os = "macos") {
                repoint.replace("sed -i ", "sed -i '' ")
            } else {
                repoint
            })
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_link(&b_link).unwrap(), b_replacement);
        assert_eq!(std::fs::read_link(&a_link).unwrap(), a_binary);
        assert_eq!(
            std::fs::read(&a_unit_path).unwrap(),
            b"A unit remains active\n"
        );
        let a_state_after_repoint = std::fs::read_dir(&a_state)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            a_state_after_repoint, a_state_before,
            "B reconnect leaves A's active lease/job fixture unchanged"
        );
        let commands = std::fs::read_to_string(home.path().join("systemctl.log")).unwrap();
        assert!(commands.contains(&format!("enable {b_unit}")));
        assert!(commands.contains(&format!("restart {b_unit}")));
        assert!(!commands.contains(&format!("enable {a_unit}")));
        assert!(!commands.contains(&format!("restart {a_unit}")));
    }

    #[test]
    fn service_paths_and_unit_names_compose_sanitized_runner_and_controller_ids() {
        let a = ("runner-a", "controller-a");
        let b = ("runner-a", "controller-b");
        assert_eq!(
            service_unit_name_for(a.0, a.1),
            format!(
                "homeboy-runner-runner-a-{}.service",
                super::super::controller_scope_segment(a.1)
            )
        );
        assert_eq!(
            service_binary_link_for(a.0, a.1),
            format!(
                ".local/share/homeboy/runner-service/runner-a/controllers/{}/homeboy",
                super::super::controller_scope_segment(a.1)
            )
        );
        assert_eq!(
            daemon_state_dir(a.0, a.1),
            format!(
                ".config/homeboy/runner-daemons/runner-a/{}",
                super::super::controller_scope_segment(a.1)
            )
        );
        assert_ne!(
            service_unit_name_for(a.0, a.1),
            service_unit_name_for(b.0, b.1)
        );
        assert_ne!(
            service_binary_link_for(a.0, a.1),
            service_binary_link_for(b.0, b.1)
        );
        assert_ne!(daemon_state_dir(a.0, a.1), daemon_state_dir(b.0, b.1));
        assert_eq!(
            service_unit_name_for("runner/a", "controller b"),
            format!(
                "homeboy-runner-runner_a-{}.service",
                super::super::controller_scope_segment("controller b")
            )
        );
    }

    #[test]
    fn controller_scope_hash_disambiguates_sanitized_collisions_and_bounds_unit_names() {
        let first = "a/b-controller";
        let second = "a_b-controller";
        assert_eq!(
            paths::sanitize_path_segment(first),
            paths::sanitize_path_segment(second)
        );
        let first_scope = super::super::controller_scope_segment(first);
        let second_scope = super::super::controller_scope_segment(second);
        assert_ne!(first_scope, second_scope);
        assert_ne!(
            service_unit_name_for("homeboy-lab", first),
            service_unit_name_for("homeboy-lab", second)
        );
        assert_ne!(
            service_binary_link_for("homeboy-lab", first),
            service_binary_link_for("homeboy-lab", second)
        );
        assert_ne!(
            daemon_state_dir("homeboy-lab", first),
            daemon_state_dir("homeboy-lab", second)
        );

        let long_id = "controller/identity/".repeat(128);
        let segment = super::super::controller_scope_segment(&long_id);
        let unit = service_unit_name_for("homeboy-lab", &long_id);
        assert_eq!(segment.len(), 24 + 1 + 64);
        assert!(
            unit.len() < 255,
            "systemd unit filename exceeds budget: {}",
            unit.len()
        );
        let readable_prefix = paths::sanitize_path_segment(&long_id)
            .chars()
            .take(24)
            .collect::<String>();
        assert!(segment.starts_with(&format!("{readable_prefix}-")));
    }

    #[test]
    fn unit_path_is_resolved_from_the_runner_login_shell() {
        let unit = render_service_unit("homeboy-lab", "t");
        assert!(unit.contains(&format!("Environment=PATH={SERVICE_PATH_PLACEHOLDER}")));
        let write_unit = write_unit_script("homeboy-lab", "t");
        assert!(write_unit.contains("-lc 'printf %s \"$PATH\"'"));
        assert!(write_unit.contains(&format!(
            "sed -i \"s|{SERVICE_PATH_PLACEHOLDER}|$service_path|\""
        )));
        let sed_at = write_unit.find("sed -i").unwrap();
        assert!(
            sed_at < write_unit.find("mv -f").unwrap(),
            "PATH is resolved before the unit is moved into place"
        );
    }

    /// Executes the resolver: a login PATH with per-user toolchains is kept
    /// ahead of the system dirs; an unsafe one falls back to the system dirs.
    #[test]
    fn service_path_resolver_keeps_login_toolchains_and_rejects_unsafe_paths() {
        let scratch = tempfile::tempdir().unwrap();
        let shell = scratch.path().join("fake-login-shell");
        let run = |login_path: &str| -> String {
            homeboy_test_support::write_executable(
                &shell,
                format!("#!/bin/sh\nprintf %s '{login_path}'\n"),
                0o755,
            )
            .unwrap();

            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!(
                    "{}\nprintf %s \"$service_path\"",
                    resolve_service_path_script()
                ))
                .env("SHELL", &shell)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(
            run("/home/u/.local/bin:/home/u/.cargo/bin:/usr/bin"),
            format!("/home/u/.local/bin:/home/u/.cargo/bin:/usr/bin:{SERVICE_PATH}")
        );
        assert_eq!(run(""), SERVICE_PATH);
        assert_eq!(run("/home/u/my dir/bin:/usr/bin"), SERVICE_PATH);
        assert_eq!(run("/x|y:/usr/bin"), SERVICE_PATH);
    }

    #[test]
    fn install_script_writes_the_rendered_unit_verbatim() {
        let script = install_script("homeboy-lab", "/opt/homeboy", "install-token");
        let start = script.find("<<'HOMEBOY_UNIT'\n").unwrap() + "<<'HOMEBOY_UNIT'\n".len();
        let end = script.find("HOMEBOY_UNIT\nlogin_path=").unwrap();
        assert_eq!(
            &script[start..end],
            render_service_unit("homeboy-lab", "install-token")
        );
        assert!(script.ends_with(&format!(
            "systemctl --user enable {}",
            service_unit_name("homeboy-lab")
        )));
    }

    /// Repoint-and-restart must rewrite the unit (not just the binary link),
    /// so a unit installed before the startup token existed self-heals to
    /// carry one the next time refresh restarts it (#15087).
    #[test]
    fn repoint_script_rewrites_the_unit_with_a_fresh_token() {
        let write_unit = write_unit_script("homeboy-lab", "repoint-token");
        assert!(write_unit.contains("repoint-token"));
        assert!(write_unit.contains("systemctl --user daemon-reload"));
        assert!(write_unit.contains(&format!(
            "cat > \"$unit_dir/{}.tmp\"",
            service_unit_name("homeboy-lab")
        )));
    }

    /// #15733: the inventory sees every scoped unit, resolves `%h` in
    /// `ExecStart`, reads each binary's version, and never invokes a mutating
    /// systemctl verb. An unscoped `homeboy-runner-<runner>.service` is not a
    /// Homeboy-managed unit and is not inventoried.
    #[test]
    fn service_inventory_reports_scoped_unit_binaries_read_only() {
        let home = tempfile::tempdir().expect("home");
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        let fake_binary = |name: &str, version: &str| {
            let path = home.path().join(name);
            homeboy_test_support::write_executable(
                &path,
                format!("#!/bin/sh\necho 'homeboy {version}'\n"),
                0o755,
            )
            .unwrap();

            path
        };
        let unscoped_slot = fake_binary("slot-unscoped", "0.395.1+5171701906f7");
        let own_slot = fake_binary("slot-own", "0.417.15+708e00a1d439");
        let unscoped_link = home
            .path()
            .join(".local/share/homeboy/runner-service/homeboy-lab/homeboy");
        std::fs::create_dir_all(unscoped_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&unscoped_slot, &unscoped_link).unwrap();
        let own_link = home
            .path()
            .join(service_binary_link_for("homeboy-lab", "controller-a"));
        std::fs::create_dir_all(own_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&own_slot, &own_link).unwrap();

        let unscoped_unit = "homeboy-runner-homeboy-lab.service";
        std::fs::write(
            unit_dir.join(unscoped_unit),
            "[Service]\nExecStart=%h/.local/share/homeboy/runner-service/homeboy-lab/homeboy daemon serve --addr 127.0.0.1:0\n",
        )
        .unwrap();
        let own_unit = service_unit_name_for("homeboy-lab", "controller-a");
        std::fs::write(
            unit_dir.join(&own_unit),
            render_service_unit_for("homeboy-lab", "controller-a", "token"),
        )
        .unwrap();
        std::fs::write(unit_dir.join("homeboy-runner-other.service"), "[Service]\n").unwrap();

        let bin_dir = home.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let systemctl = bin_dir.join("systemctl");
        homeboy_test_support::write_executable(&systemctl, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/systemctl.log\"\ncase \"$*\" in\n  *is-active*) echo active ;;\n  *MainPID*) echo 0 ;;\nesac\n", 0o755)
        .unwrap();

        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "echo 'login banner'\n{}",
                service_inventory_script("homeboy-lab")
            ))
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let observed = parse_service_inventory(&String::from_utf8_lossy(&output.stdout));

        assert_eq!(observed.len(), 1, "{observed:?}");
        assert!(observed.iter().all(|unit| unit.unit != unscoped_unit));
        let own = &observed[0];
        assert_eq!(own.unit, own_unit);
        assert_eq!(own.binary, own_link.display().to_string());
        assert_eq!(
            own.binary_version.as_deref(),
            Some("homeboy 0.417.15+708e00a1d439")
        );
        assert_eq!(own.running_version, None);
        assert_eq!(own.active_state.as_deref(), Some("active"));

        let commands = std::fs::read_to_string(home.path().join("systemctl.log")).unwrap();
        for verb in [
            "start",
            "stop",
            "restart",
            "enable",
            "disable",
            "daemon-reload",
        ] {
            assert!(
                !commands
                    .lines()
                    .any(|line| line.split_whitespace().any(|word| word == verb)),
                "inventory must stay read-only, saw `{verb}` in {commands}"
            );
        }
    }

    #[test]
    fn service_unit_scope_distinguishes_own_other_controllers_and_unmanaged_units() {
        let own = service_unit_name_for("homeboy-lab", "controller-a");
        let other = service_unit_name_for("homeboy-lab", "controller-b");
        let other_runner = service_unit_name_for("homeboy-lab-2", "controller-a");
        assert_eq!(
            service_unit_scope_for("homeboy-lab", "controller-a", &own),
            Some("controller")
        );
        assert_eq!(
            service_unit_scope_for(
                "homeboy-lab",
                "controller-a",
                "homeboy-runner-homeboy-lab.service"
            ),
            None,
            "an unscoped runner unit is not Homeboy-managed"
        );
        assert_eq!(
            service_unit_scope_for("homeboy-lab", "controller-a", &other),
            Some("other_controller")
        );
        assert_eq!(
            service_unit_scope_for(
                "homeboy-lab",
                "controller-a",
                "homeboy-runner-homeboy-lab-2.service"
            ),
            None
        );
        // `homeboy-lab-2`'s scoped unit shares the `homeboy-runner-homeboy-lab-`
        // prefix and ends in a digest, so the other-runner case is resolved by
        // the runner's own glob rather than mistaken for its own unit.
        assert_ne!(
            service_unit_scope_for("homeboy-lab", "controller-a", &other_runner),
            Some("controller")
        );
    }

    #[test]
    fn service_inventory_parser_ignores_unmarked_lines_and_empty_fields() {
        let parsed = parse_service_inventory(
            "banner\thomeboy-runner-x.service\nhomeboy-runner-service\tu.service\t/b\t\t\tfailed\n",
        );
        assert_eq!(
            parsed,
            vec![ServiceUnitObservation {
                unit: "u.service".to_string(),
                binary: "/b".to_string(),
                binary_version: None,
                running_version: None,
                active_state: Some("failed".to_string()),
            }]
        );
    }

    #[test]
    fn repoint_migration_enables_and_restarts_only_the_scoped_unit() {
        let home = tempfile::tempdir().expect("home");
        let scoped = service_unit_name_for("homeboy-lab", "controller-b");
        let unscoped = "homeboy-runner-homeboy-lab.service";
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        let unscoped_path = unit_dir.join(unscoped);
        std::fs::write(&unscoped_path, "unmanaged unit owned elsewhere\n").unwrap();

        let bin_dir = home.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let systemctl = bin_dir.join("systemctl");
        homeboy_test_support::write_executable(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/systemctl.log\"\n",
            0o755,
        )
        .unwrap();

        let binary = home.path().join("homeboy-new");
        std::fs::write(&binary, "new binary").unwrap();
        let script = repoint_script_for(
            "homeboy-lab",
            "controller-b",
            &binary.display().to_string(),
            "migration-token",
        );
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(if cfg!(target_os = "macos") {
                script.replace("sed -i ", "sed -i '' ")
            } else {
                script
            })
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .status()
            .unwrap();
        assert!(status.success());

        assert!(unit_dir.join(&scoped).exists());
        assert_eq!(
            std::fs::read_to_string(&unscoped_path).unwrap(),
            "unmanaged unit owned elsewhere\n"
        );
        let commands = std::fs::read_to_string(home.path().join("systemctl.log")).unwrap();
        assert!(commands.contains(&format!("enable {scoped}")));
        assert!(commands.contains(&format!("restart {scoped}")));
        assert!(!commands.contains(unscoped));
    }
}
