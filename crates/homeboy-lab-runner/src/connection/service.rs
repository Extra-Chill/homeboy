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
/// Each poll is already an SSH round trip; the pause between polls only needs
/// to keep a fast restart from being observed one long interval late.
const SERVICE_READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
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
}

/// The machine's one host daemon unit, shared by every controller and runner id.
pub(crate) fn service_unit_name() -> String {
    "homeboy-runner.service".to_string()
}

/// The delegated unit that holds every runner job's cgroup (#18493).
///
/// Jobs outlive a daemon restart, so their tree cannot sit in the daemon's
/// unit: systemd spawns a starting service into its unit's root cgroup, and
/// cgroup v2 refuses that while the root enables controllers for a populated
/// subtree. This unit's only process sleeps in its `holder/` leaf, upgrades
/// never restart it, and stopping it kills its whole tree, so nothing can
/// block its own next start.
pub(crate) fn job_tree_unit_name() -> String {
    "homeboy-runner-jobs.service".to_string()
}

fn render_job_tree_unit() -> String {
    format!(
        r#"[Unit]
Description=Homeboy runner job cgroups

[Service]
Type=exec
ExecStart=/bin/sleep infinity
Restart=always
Delegate=yes
DelegateSubgroup={holder}

[Install]
WantedBy=default.target
"#,
        holder = homeboy_core::job_cgroup::HOLDER_LEAF,
    )
}

/// The stable binary link the unit executes, relative to `$HOME`.
pub(crate) fn service_binary_link() -> String {
    ".local/share/homeboy/runner-service/host/homeboy".to_string()
}

#[cfg(test)]
pub(crate) fn render_service_unit(startup_token: &str) -> String {
    render_service_unit_with_env(startup_token, &[])
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
            .copied()
            .chain(std::iter::once("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS"))
            .filter_map(|key| {
                let value = env.get(key)?.trim();
                (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| (key.to_string(), value.to_string()))
            })
            .collect();
    entries.sort();
    entries
}

fn render_service_unit_with_env(startup_token: &str, daemon_env: &[(String, String)]) -> String {
    let idle_timeout = daemon_env
        .iter()
        .find(|(key, _)| key == "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS")
        .map(|(_, value)| value.as_str())
        .unwrap_or("21600");
    let policy_env: String = daemon_env
        .iter()
        .filter(|(key, _)| key != "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS")
        .map(|(key, value)| format!("Environment={key}={value}\n"))
        .collect();
    format!(
        r#"[Unit]
Description=Homeboy runner daemon
After=network-online.target {job_tree}
Wants={job_tree}

[Service]
Type=simple
Environment=HOMEBOY_DAEMON_STATE_DIR=%h/{state_dir}
Environment={token_env}={startup_token}
Environment=PATH={path}
Environment=HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS={idle_timeout}
Environment={job_tree_env}={job_tree}
{policy_env}ExecStart=%h/{link} daemon serve --addr 127.0.0.1:0
Restart=on-failure
RestartSec=2s
TimeoutStopSec=30s
KillMode=process

[Install]
WantedBy=default.target
"#,
        state_dir = super::daemon_state_dir(),
        token_env = paths::DAEMON_STARTUP_TOKEN_ENV,
        path = SERVICE_PATH_PLACEHOLDER,
        link = service_binary_link(),
        job_tree = job_tree_unit_name(),
        job_tree_env = homeboy_core::job_cgroup::JOB_TREE_UNIT_ENV,
    )
}

/// Atomically point the unit's binary link at `binary`.
fn point_binary_script(binary: &str) -> String {
    format!(
        r#"link="$HOME/{link}"
mkdir -p "$(dirname "$link")"
ln -sfn {binary} "$link.next"
mv -f "$link.next" "$link""#,
        link = service_binary_link(),
        binary = shell::quote_arg(binary),
    )
}

/// Write the rendered unit and reload systemd's view of it. Shared by
/// install (which also enables and starts the unit for the first time) and
/// repoint-and-restart (which only needs the file on disk to be current
/// before its own restart), so a repoint always carries the daemon's startup
/// token even when the unit predates this being rendered (#15087).
fn write_unit_script(startup_token: &str, daemon_env: &[(String, String)]) -> String {
    let unit = service_unit_name();
    format!(
        r#"unit_dir="$HOME/.config/systemd/user"
mkdir -p "$unit_dir"
cat > "$unit_dir/{job_tree}" <<'HOMEBOY_UNIT'
{job_tree_text}HOMEBOY_UNIT
cat > "$unit_dir/{unit}.tmp" <<'HOMEBOY_UNIT'
{unit_text}HOMEBOY_UNIT
{resolve_path}
sed -i "s|{placeholder}|$service_path|" "$unit_dir/{unit}.tmp"
mv -f "$unit_dir/{unit}.tmp" "$unit_dir/{unit}"
systemctl --user daemon-reload
systemctl --user enable --now {job_tree}"#,
        job_tree = job_tree_unit_name(),
        job_tree_text = render_job_tree_unit(),
        unit_text = render_service_unit_with_env(startup_token, daemon_env),
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

fn install_script(binary: &str, startup_token: &str, daemon_env: &[(String, String)]) -> String {
    format!(
        r#"set -eu
{point}
{write_unit}
systemctl --user enable --now {unit}"#,
        unit = service_unit_name(),
        point = point_binary_script(binary),
        write_unit = write_unit_script(startup_token, daemon_env),
    )
}

fn repoint_script(binary: &str, startup_token: &str, daemon_env: &[(String, String)]) -> String {
    format!(
        "set -eu\n{point}\n{write_unit}\nsystemctl --user enable {unit}\nsystemctl --user stop {unit}\n{adopt}\nsystemctl --user start {unit}",
        point = point_binary_script(binary),
        write_unit = write_unit_script(startup_token, daemon_env),
        unit = service_unit_name(),
        adopt = adopt_in_unit_jobs_script(),
    )
}

/// Move what a pre-#18493 daemon left running inside the runner unit into the
/// job tree unit, so the unit's next start is not refused. Each job cgroup
/// keeps its job id, so the new daemon re-adopts it; children the old daemon
/// left in its `daemon/` leaf move to `jobs/legacy-daemon`. Each source is
/// frozen while its processes move, so none forks behind the move. The runner
/// unit's enabled controllers are cleared last, since they alone refuse a
/// start. A no-op on the new layout.
fn adopt_in_unit_jobs_script() -> String {
    format!(
        r#"tree="/sys/fs/cgroup$(systemctl --user show --property=ControlGroup --value {job_tree})"
runner="$(dirname "$tree")/{unit}"
adopt() {{
  # cgroupfs files report size 0, so test content, not `-s`.
  if grep -q . "$1/cgroup.procs"; then
    mkdir -p "$tree/{jobs}" "$2"
    echo 1 > "$1/cgroup.freeze"
    for _ in 1 2 3 4 5; do
      grep -q . "$1/cgroup.procs" || break
      while read -r pid; do echo "$pid" > "$2/cgroup.procs" || true; done < "$1/cgroup.procs"
    done
  fi
  rmdir "$1" || true
}}
if [ -d "$runner" ]; then
  for job in "$runner/{jobs}"/*/; do
    if [ -d "$job" ]; then adopt "${{job%/}}" "$tree/{jobs}/$(basename "$job")"; fi
  done
  if [ -d "$runner/{jobs}" ]; then rmdir "$runner/{jobs}" || true; fi
  if [ -d "$runner/daemon" ]; then adopt "$runner/daemon" "$tree/{jobs}/legacy-daemon"; fi
  for controller in io memory pids; do
    echo "-$controller" > "$runner/cgroup.subtree_control" 2>/dev/null || true
  done
fi"#,
        job_tree = job_tree_unit_name(),
        unit = service_unit_name(),
        jobs = homeboy_core::job_cgroup::JOBS_DIR,
    )
}

/// Start the host unit, installing it from `binary` first when this runner
/// host has none yet. The first controller to attach after #18063 creates
/// it; every later controller finds it and only starts it.
fn ensure_host_unit_script(
    binary: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    format!(
        r#"set -eu
if [ ! -f "$HOME/.config/systemd/user/{unit}" ]; then
{install}
fi
systemctl --user start {unit}"#,
        unit = service_unit_name(),
        install = install_script(binary, startup_token, daemon_env),
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

/// Start the host unit (installing it from `homeboy` if the host has none),
/// then poll read-only daemon status until it holds a fresh, reachable,
/// loopback lease. Starting an active unit changes nothing.
fn wait_for_service_daemon(
    target: &ServiceTarget,
    homeboy: &str,
    runner_id: &str,
) -> std::result::Result<RemoteDaemon, String> {
    let client = &target.client;
    run_remote(
        client,
        &ensure_host_unit_script(
            homeboy,
            &uuid::Uuid::new_v4().to_string(),
            &daemon_policy_env(&target.runner.env),
        ),
        "starting the runner service",
    )
    .map_err(|error| with_unit_evidence(client, error.message))?;
    poll_until_ready(
        Instant::now() + SERVICE_READY_TIMEOUT,
        || match remote_daemon_status(client, homeboy, runner_id) {
            Ok(status) => match status.daemon {
                Some(daemon)
                    if status.fresh
                        && status.reachable
                        && daemon.pid.is_some()
                        && daemon.lease_id.as_deref().is_some_and(|id| !id.is_empty())
                        && parse_loopback_daemon_addr(&daemon.address).is_ok() =>
                {
                    Ok(daemon)
                }
                Some(_) => Err(format!(
                    "service daemon lease is not ready (fresh: {}, reachable: {}, stale reason: {})",
                    status.fresh,
                    status.reachable,
                    status.stale_reason.as_deref().unwrap_or("none"),
                )),
                None => Err("service daemon has not published a lease".to_string()),
            },
            Err(error) => Err(error),
        },
        || std::thread::sleep(SERVICE_READY_POLL_INTERVAL),
    )
    .map_err(|last| with_unit_evidence(client, format!("{last}.")))
}

/// Probe until `probe` succeeds or `deadline` passes, pausing with `pause`
/// between failed probes. Returns the first success at once, else the last
/// failure.
fn poll_until_ready<T, E>(
    deadline: Instant,
    mut probe: impl FnMut() -> std::result::Result<T, E>,
    mut pause: impl FnMut(),
) -> std::result::Result<T, E> {
    loop {
        match probe() {
            Ok(ready) => return Ok(ready),
            Err(last) if Instant::now() >= deadline => return Err(last),
            Err(_) => pause(),
        }
    }
}

fn report_for(runner_id: &str, binary: &str, daemon: Option<&RemoteDaemon>) -> RunnerServiceReport {
    RunnerServiceReport {
        runner_id: runner_id.to_string(),
        unit: service_unit_name(),
        binary: binary.to_string(),
        state_dir: format!("~/{}", super::daemon_state_dir()),
        active: daemon.is_some(),
        daemon_pid: daemon.and_then(|daemon| daemon.pid),
        daemon_lease_id: daemon.and_then(|daemon| daemon.lease_id.clone()),
        daemon_address: daemon.map(|daemon| daemon.address.clone()),
        daemon_build_identity: daemon.and_then(|daemon| daemon.build_identity.clone()),
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

    if let Some(lease_id) = status
        .daemon
        .as_ref()
        .and_then(|daemon| daemon.lease_id.as_deref())
    {
        // The controller-started daemon still owns the state directory. It is
        // idle, so stop it and let the unit take the owner lock.
        remote_daemon_force_stop(client, homeboy, lease_id).map_err(Error::internal_unexpected)?;
    }
    run_remote(
        client,
        &install_script(
            homeboy,
            &uuid::Uuid::new_v4().to_string(),
            &daemon_policy_env(&target.runner.env),
        ),
        "starting the runner service",
    )?;
    let daemon =
        wait_for_service_daemon(&target, homeboy, runner_id).map_err(Error::internal_unexpected)?;

    super::super::merge(Some(runner_id), r#"{"service_managed":true}"#, &[])?;
    Ok(report_for(runner_id, homeboy, Some(&daemon)))
}

/// Report the runner service and the daemon lease it currently holds.
pub fn service_status(runner_id: &str) -> Result<RunnerServiceReport> {
    let target = service_target(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&target.runner, "runner service status")?;
    let daemon = remote_daemon_status(&target.client, homeboy, runner_id)
        .ok()
        .filter(|status| status.fresh && status.reachable)
        .and_then(|status| status.daemon);
    Ok(report_for(runner_id, homeboy, daemon.as_ref()))
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
    let startup_token = uuid::Uuid::new_v4().to_string();
    run_remote(
        &target.client,
        &delegated_repoint_script(
            binary,
            &startup_token,
            &daemon_policy_env(&target.runner.env),
        ),
        "restarting the runner service",
    )
    .map_err(|mut error| {
        error.message = with_unit_evidence(&target.client, error.message);
        error
    })?;
    Ok(())
}

/// Append the runner unit's own account of why it is not running: its systemd
/// state and result, and the last journal lines, which carry the start error.
/// A failed restart then explains itself instead of pointing the operator at
/// the runner (#18539).
fn with_unit_evidence(client: &SshClient, message: String) -> String {
    let Ok(unit) = super::probe_unit_state(client) else {
        return message;
    };
    let error = unit
        .last_error
        .as_deref()
        .map(|error| format!("; start error: {error}"))
        .unwrap_or_default();
    format!(
        "{message}\nRunner unit {}: {}{error}",
        service_unit_name(),
        unit.summary()
    )
}

/// Repoint through the target binary, so the version that knows the unit
/// layout it runs under writes that layout and migrates to it (#18539). The
/// controller's own rendering serves only a target that predates
/// `runner service repoint`.
fn delegated_repoint_script(
    binary: &str,
    startup_token: &str,
    daemon_env: &[(String, String)],
) -> String {
    let env: String = daemon_env
        .iter()
        .map(|(key, value)| format!(" --env {}", shell::quote_arg(&format!("{key}={value}"))))
        .collect();
    format!(
        r#"if {binary} runner service repoint --help >/dev/null 2>&1; then
  exec {binary} runner service repoint --binary {binary} --startup-token {token}{env} >/dev/null
fi
{fallback}"#,
        binary = shell::quote_arg(binary),
        token = shell::quote_arg(startup_token),
        fallback = repoint_script(binary, startup_token, daemon_env),
    )
}

/// Run this binary's repoint on the runner host itself: point the unit at
/// `binary`, write this version's units, migrate what an older layout left
/// running, and restart. Policy env is filtered through the same allowlist
/// as a controller-rendered unit.
pub fn repoint_local(
    binary: &str,
    startup_token: &str,
    env: &[String],
) -> Result<RunnerServiceReport> {
    let env = env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    let script = repoint_script(binary, startup_token, &daemon_policy_env(&env));
    let output = std::process::Command::new("sh")
        .args(["-c", &script])
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("runner service repoint".into()))
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "runner service repoint failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(report_for("local", binary, None))
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

/// Read-only shell that reports the runner's host unit, when installed, with
/// the binary its `ExecStart` runs, that binary's version, the version of the
/// executable the main process is running, and the unit's active state. It
/// never starts, stops, reloads, or rewrites anything.
pub(crate) fn service_inventory_script() -> String {
    format!(
        r#"unit_dir="$HOME/.config/systemd/user"
for unit_path in "$unit_dir"/{unit}; do
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
        unit = service_unit_name(),
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
        &service_inventory_script(),
        "inspecting runner service binaries",
    )?;
    Ok(parse_service_inventory(&stdout))
}

/// Observe the runner-service unit hosted on this machine, read-only. A
/// machine that is not a runner host, or cannot run the probe, reports none.
pub(crate) fn observe_local_service_units() -> Vec<ServiceUnitObservation> {
    std::process::Command::new("sh")
        .args(["-c", &service_inventory_script()])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_service_inventory(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
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
/// and write the session. Start this controller's unit if it retired idle.
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
    ensure_machine_not_registered(runner_id, identity.machine_id.as_deref())?;
    let Some(expected_identity) = identity.build_identity.clone() else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            "the runner's homeboy did not report a build identity".to_string(),
        ));
    };
    // Another controller may restart the shared host daemon between choosing
    // its lease and attaching to it. Follow the restart to the successor
    // instead of refusing the connect.
    let deadline = Instant::now() + SERVICE_READY_TIMEOUT;
    let (connection, expected_identity, expected_version) = loop {
        let daemon = match wait_for_service_daemon(&target, homeboy, runner_id) {
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
        let attached_lease = daemon.lease_id.clone();
        // The host daemon serves every controller, so it may run another build
        // than this controller's configured homeboy. Lab contract compatibility
        // admits it (#16577); the session records the build that actually runs.
        let expected_identity = daemon
            .build_identity
            .clone()
            .filter(|identity| !identity.trim().is_empty())
            .unwrap_or_else(|| expected_identity.clone());
        let expected_version = daemon.version.clone().unwrap_or(identity.version.clone());
        match connect_remote_daemon(connection_daemon::RemoteDaemonConnectRequest {
            owner_bound: false,
            server: &target.server,
            homeboy,
            daemon,
            expected_version: &expected_version,
            expected_identity: &expected_identity,
            runner_id,
            session_path: &session_path,
            unresponsive_recovery: None,
        }) {
            Ok(connection) => break (connection, expected_identity, expected_version),
            Err(_)
                if Instant::now() < deadline
                    && host_daemon_was_replaced(
                        attached_lease.as_deref(),
                        remote_daemon_status(client, homeboy, runner_id)
                            .ok()
                            .as_ref(),
                    ) => {}
            Err(report) => return Ok(*report),
        }
    };
    let connection_daemon::RemoteDaemonConnection {
        local_port,
        tunnel_pid,
        tunnel_process_start_identity,
        local_url,
        daemon,
        ..
    } = connection;

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
        machine_id: identity.machine_id.clone(),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
    };
    if let Err(error) = super::replacement_journal::promote_pending_replacement(runner_id, &session)
        .and_then(|_| write_session(&session))
    {
        terminate_tunnel_if_owned(&session);
        return Err(error);
    }

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
            failure_kind: None,
            failure_message: None,
            failure_evidence: None,
            binary_convergence: None,
        },
        0,
    ))
}

/// Whether the host daemon this connect attached to was replaced while it
/// attached: the host now reports another lease, or none that is fresh yet.
fn host_daemon_was_replaced(
    attached_lease: Option<&str>,
    status: Option<&RemoteDaemonStatus>,
) -> bool {
    let Some(status) = status else { return false };
    let current = status
        .daemon
        .as_ref()
        .and_then(|daemon| daemon.lease_id.as_deref());
    !status.fresh || current != attached_lease
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
        let unit = render_service_unit("test-startup-token");
        assert!(unit.contains(
            "ExecStart=%h/.local/share/homeboy/runner-service/host/homeboy daemon serve --addr 127.0.0.1:0"
        ));
        assert!(unit.contains(
            "Environment=HOMEBOY_DAEMON_STATE_DIR=%h/.config/homeboy/runner-daemons/host\n"
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("Environment=HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=21600\n"));
        // A restart stops only the daemon; re-adoptable job children lead
        // their own sessions and must outlive it.
        assert!(unit.contains("KillMode=process"));
        // Jobs live in the job tree unit, so a restart of this unit never
        // meets a cgroup with live jobs (#18493).
        assert!(!unit.contains("Delegate"));
        assert!(unit.contains("\nWants=homeboy-runner-jobs.service\n"));
        assert!(
            unit.contains("\nEnvironment=HOMEBOY_JOB_CGROUP_UNIT=homeboy-runner-jobs.service\n")
        );
        let job_tree = render_job_tree_unit();
        assert!(job_tree.contains("\nDelegate=yes\nDelegateSubgroup=holder\n"));
        assert!(unit.contains("WantedBy=default.target"));
        assert_eq!(service_unit_name(), "homeboy-runner.service");
    }

    #[test]
    fn service_readiness_poll_returns_on_the_first_ready_probe() {
        let mut probes = 0;
        let mut pauses = 0;
        let ready = poll_until_ready(
            Instant::now() + Duration::from_secs(60),
            || {
                probes += 1;
                if probes < 3 {
                    Err("not ready")
                } else {
                    Ok("lease")
                }
            },
            || pauses += 1,
        );
        assert_eq!(ready, Ok("lease"));
        // Ready on the third probe: two pauses, no trailing wait.
        assert_eq!((probes, pauses), (3, 2));

        let mut pauses = 0;
        let expired: std::result::Result<(), &str> =
            poll_until_ready(Instant::now(), || Err("last"), || pauses += 1);
        assert_eq!(expired, Err("last"));
        assert_eq!(pauses, 0);
    }

    #[test]
    fn unit_renders_runner_owned_guard_policy_from_runner_env() {
        let env: std::collections::HashMap<String, String> = [
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT", " 1024 "),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES", "68719476736"),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT", "512"),
            ("HOMEBOY_COMMAND", "/opt/homeboy"),
            ("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS", " 3600 "),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let policy = daemon_policy_env(&env);
        assert_eq!(
            policy,
            vec![
                (
                    "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS".to_string(),
                    "3600".to_string()
                ),
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

        let unit = render_service_unit_with_env("t", &policy);
        assert!(
            unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT=1024\n")
        );
        assert!(
            unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES=68719476736\n")
        );
        assert!(unit.contains("\nEnvironment=HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT=512\n"));
        assert!(!unit.contains("HOMEBOY_COMMAND"));
        assert!(unit.contains("Environment=HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=3600\n"));
        assert_eq!(
            unit.matches("Environment=HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=")
                .count(),
            1
        );
        assert!(unit.contains(
            "Environment=HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES=68719476736\nExecStart="
        ));
    }

    #[test]
    fn daemon_policy_env_rejects_values_that_could_inject_into_the_unit() {
        let env: std::collections::HashMap<String, String> = [
            (
                "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT",
                "1024\nExecStartPre=/bin/evil",
            ),
            ("HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES", "-1"),
            (
                "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS",
                "1\nExecStartPre=/bin/evil",
            ),
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
            install_script("/opt/homeboy", "t", &policy),
            repoint_script("/opt/homeboy", "t", &policy),
        ] {
            assert!(
                script.contains("Environment=HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT=1024")
            );
        }
    }

    /// The service-owned daemon must publish an attributable startup token in
    /// its own lease. Without this, `daemon status`/`ensure-running` can never
    /// tell this process apart from an unrelated foreground `daemon serve`,
    /// and every controller operation that inspects ownership fails closed
    /// (#15087).
    #[test]
    fn unit_carries_the_daemon_startup_token_env_var() {
        let unit = render_service_unit("a-fresh-token");
        assert!(unit.contains(&format!(
            "Environment={}=a-fresh-token",
            paths::DAEMON_STARTUP_TOKEN_ENV
        )));
    }

    #[test]
    fn each_install_and_repoint_gets_its_own_fresh_startup_token() {
        let first = render_service_unit("token-a");
        let second = render_service_unit("token-b");
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
                    point_binary_script(&binary.display().to_string())
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
                .join(".local/share/homeboy/runner-service/host/homeboy");
            assert_eq!(std::fs::read_link(&link).unwrap(), *binary);
        }
    }

    /// Run a service script under a real shell with a recording `systemctl`.
    fn run_in_runner_home(home: &std::path::Path, script: &str) -> String {
        let bin_dir = home.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        homeboy_test_support::write_executable(
            bin_dir.join("systemctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/systemctl.log\"\n",
            0o755,
        )
        .unwrap();
        let script = if cfg!(target_os = "macos") {
            script.replace("sed -i ", "sed -i '' ")
        } else {
            script.to_string()
        };
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("HOME", home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// #18539: a repoint runs the target binary's own repoint, so a newer
    /// target migrates to its layout even when an older controller drives the
    /// upgrade; only a target without `runner service repoint` gets the
    /// controller's rendering.
    #[test]
    fn repoint_is_performed_by_the_target_binary_when_it_can() {
        let policy = vec![(
            "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS".to_string(),
            "60".to_string(),
        )];
        for (supports_repoint, delegated) in [(true, true), (false, false)] {
            let home = tempfile::tempdir().unwrap();
            let target = home.path().join("homeboy-target");
            homeboy_test_support::write_executable(
                &target,
                &format!(
                    "#!/bin/sh\ncase \"$*\" in *--help*) exit {};; esac\nprintf '%s\\n' \"$*\" >> \"$HOME/target.log\"\n",
                    if supports_repoint { 0 } else { 2 }
                ),
                0o755,
            )
            .unwrap();
            let target = target.display().to_string();
            run_in_runner_home(
                home.path(),
                &delegated_repoint_script(&target, "tok", &policy),
            );
            let read =
                |name: &str| std::fs::read_to_string(home.path().join(name)).unwrap_or_default();
            if delegated {
                assert_eq!(
                    read("target.log"),
                    format!(
                        "runner service repoint --binary {target} --startup-token tok --env HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=60\n"
                    )
                );
                assert_eq!(read("systemctl.log"), "");
            } else {
                assert_eq!(read("target.log"), "");
                assert!(read("systemctl.log").contains("start homeboy-runner.service"));
            }
        }
    }

    /// A connect follows another controller's in-place restart of the host
    /// daemon, and gives up only when the daemon it attached to still holds
    /// the host or the host cannot be observed.
    #[test]
    fn connect_retries_only_after_the_attached_daemon_was_replaced() {
        let status = |fresh, lease| {
            super::super::tests::remote_daemon_status_for_test(fresh, true, 0, lease, 7)
        };
        for (status, replaced) in [
            (Some(status(true, "restarted")), true),
            (Some(status(false, "attached")), true),
            (Some(status(true, "attached")), false),
            (None, false),
        ] {
            assert_eq!(
                host_daemon_was_replaced(Some("attached"), status.as_ref()),
                replaced,
                "{status:?}"
            );
        }
    }

    /// Two controllers on different builds attach to one host unit: the first
    /// installs it from its binary, the second only starts it, so it never
    /// repoints a daemon another controller's jobs run on (#18063).
    #[test]
    fn controllers_on_different_builds_share_one_host_unit() {
        let home = tempfile::tempdir().expect("runner home");
        let first = home.path().join("homeboy-a");
        let second = home.path().join("homeboy-b");
        let unit = home
            .path()
            .join(".config/systemd/user/homeboy-runner.service");
        let link = home
            .path()
            .join(".local/share/homeboy/runner-service/host/homeboy");

        for binary in [&first, &second] {
            run_in_runner_home(
                home.path(),
                &ensure_host_unit_script(&binary.display().to_string(), "t", &[]),
            );
        }

        assert_eq!(std::fs::read_link(&link).unwrap(), first);
        assert!(std::fs::read_to_string(&unit)
            .unwrap()
            .contains("HOMEBOY_DAEMON_STATE_DIR=%h/.config/homeboy/runner-daemons/host\n"));
        let commands = std::fs::read_to_string(home.path().join("systemctl.log")).unwrap();
        let unit_name = "homeboy-runner.service";
        assert_eq!(
            commands
                .matches(&format!("enable --now {unit_name}"))
                .count(),
            1
        );
        assert_eq!(commands.matches(&format!("start {unit_name}")).count(), 2);

        // A refresh is the one path that repoints the shared unit.
        run_in_runner_home(
            home.path(),
            &repoint_script(&second.display().to_string(), "t2", &[]),
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), second);
    }

    #[test]
    fn unit_path_is_resolved_from_the_runner_login_shell() {
        let unit = render_service_unit("t");
        assert!(unit.contains(&format!("Environment=PATH={SERVICE_PATH_PLACEHOLDER}")));
        let write_unit = write_unit_script("t", &[]);
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
    fn adopt_script_is_valid_sh() {
        let status = std::process::Command::new("sh")
            .args(["-n", "-c", &adopt_in_unit_jobs_script()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn install_script_writes_the_rendered_unit_verbatim() {
        let script = install_script("/opt/homeboy", "install-token", &[]);
        let marker = "<<'HOMEBOY_UNIT'\n";
        let job_tree = script.find(marker).unwrap() + marker.len();
        let start = script.rfind(marker).unwrap() + marker.len();
        let end = script.find("HOMEBOY_UNIT\nlogin_path=").unwrap();
        assert!(script[job_tree..].starts_with(&render_job_tree_unit()));
        assert_eq!(&script[start..end], render_service_unit("install-token"));
        assert!(script.contains("systemctl --user enable --now homeboy-runner-jobs.service"));
        assert!(script.contains(&format!(
            "systemctl --user enable --now {}",
            service_unit_name()
        )));
    }

    /// Repoint-and-restart must rewrite the unit (not just the binary link),
    /// so a unit installed before the startup token existed self-heals to
    /// carry one the next time refresh restarts it (#15087).
    #[test]
    fn repoint_script_rewrites_the_unit_with_a_fresh_token() {
        let write_unit = write_unit_script("repoint-token", &[]);
        assert!(write_unit.contains("repoint-token"));
        assert!(write_unit.contains("systemctl --user daemon-reload"));
        assert!(write_unit.contains(&format!("cat > \"$unit_dir/{}.tmp\"", service_unit_name())));
    }

    /// #15733: the inventory sees the host unit, resolves `%h` in
    /// `ExecStart`, reads its binary's version, and never invokes a mutating
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
        let own_link = home.path().join(service_binary_link());
        std::fs::create_dir_all(own_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&own_slot, &own_link).unwrap();

        let unscoped_unit = "homeboy-runner-homeboy-lab.service";
        std::fs::write(
            unit_dir.join(unscoped_unit),
            "[Service]\nExecStart=%h/.local/share/homeboy/runner-service/homeboy-lab/homeboy daemon serve --addr 127.0.0.1:0\n",
        )
        .unwrap();
        let own_unit = service_unit_name();
        std::fs::write(unit_dir.join(&own_unit), render_service_unit("token")).unwrap();
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
                service_inventory_script()
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
}
