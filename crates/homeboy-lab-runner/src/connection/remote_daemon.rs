use super::*;
use crate::daemon_repair;
use crate::session::RunnerConnectFailureEvidenceRef;
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonRecoveryEvidence};
use homeboy_lab_runner_contract::LabCapabilityVersion;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(super) const REMOTE_DAEMON_STATUS_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn resolve_ssh_runner(runner: &Runner) -> Result<Option<(String, Server, SshClient)>> {
    if runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let server_id = runner.server_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(&server_id)?;
    let mut client = SshClient::from_server(&server, &server_id)?;
    client.env.extend(runner.env.clone());
    Ok(Some((server_id, server, client)))
}

/// Read the remote Homeboy version on a lifecycle (write) path.
///
/// Retains the retrying, unbounded execution used by `connect`, where a
/// transient SSH failure should be retried rather than surfaced. Read-only
/// inspection must use [`bounded_remote_homeboy_version`] instead.
pub(super) fn remote_homeboy_version(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<String, String> {
    parse_remote_homeboy_version(&client.execute_with_timeout(
        &remote_homeboy_version_command(homeboy),
        REMOTE_DAEMON_STATUS_TIMEOUT,
    ))
}

fn remote_homeboy_version_command(homeboy: &str) -> String {
    format!("{} --version", shell::quote_arg(homeboy))
}

fn parse_remote_homeboy_version(
    output: &homeboy_core::server::CommandOutput,
) -> std::result::Result<String, String> {
    if !output.success {
        return Err(command_failure_message(
            "remote Homeboy version check failed",
            output,
        ));
    }
    let version = output.stdout.trim().to_string();
    if version.is_empty() {
        return Err("remote Homeboy version check returned empty output".to_string());
    }
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteHomeboyIdentity {
    pub(super) version: String,
    pub(super) build_identity: Option<String>,
    /// The daemon-recovery capabilities the remote advertised in its typed
    /// self-identity report. `None` means the remote is an older binary that
    /// never advertised the field; controllers must fall back to scraping the
    /// long-option help text for those recovery contracts.
    pub(super) daemon_recovery_capabilities: Option<Vec<LabCapabilityVersion>>,
}

/// Read the immutable identity of the remote Homeboy executable on a lifecycle
/// (write) path. Retains the retrying, unbounded execution used by `connect`.
pub(super) fn remote_homeboy_identity(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteHomeboyIdentity, String> {
    let output = client.execute_with_timeout(
        &remote_homeboy_identity_command(homeboy),
        REMOTE_DAEMON_STATUS_TIMEOUT,
    );
    if output.success {
        if let Some(identity) = parse_self_identity_output(&output.stdout) {
            return Ok(identity);
        }
    }
    let version = remote_homeboy_version(client, homeboy)?;
    Ok(RemoteHomeboyIdentity {
        version: normalize_homeboy_version_owned(&version),
        build_identity: None,
        daemon_recovery_capabilities: None,
    })
}

/// Read the remote Homeboy identity under a hard wall-clock bound (#10418).
///
/// This is the probe `runner status` was blocked on while a prune/cook occupied
/// the Lab path. A timeout degrades to an unverifiable identity — which callers
/// already model as `IdentityComparison::Unverifiable` — and is recorded in the
/// read-only probe ledger so the partial answer names its own gap.
pub(super) fn bounded_remote_homeboy_identity(
    client: &SshClient,
    homeboy: &str,
    runner_id: Option<&str>,
) -> std::result::Result<RemoteHomeboyIdentity, String> {
    bounded_remote_homeboy_identity_until(
        client,
        homeboy,
        runner_id,
        std::time::Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    )
}

pub(super) fn bounded_remote_homeboy_identity_until(
    client: &SshClient,
    homeboy: &str,
    runner_id: Option<&str>,
    deadline: std::time::Instant,
) -> std::result::Result<RemoteHomeboyIdentity, String> {
    let command = remote_homeboy_identity_command(homeboy);
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if timeout.is_zero() {
        return Err(
            "runner admission observation deadline exhausted before remote Homeboy identity"
                .to_string(),
        );
    }
    let started = std::time::Instant::now();
    let output = client.execute_with_timeout(&command, timeout);
    let degraded = crate::readonly_probe::record_probe_outcome(
        "runner_homeboy_identity",
        runner_id,
        started,
        timeout,
        &output,
    );
    if output.success {
        if let Some(identity) = parse_self_identity_output(&output.stdout) {
            return Ok(identity);
        }
    }
    // A probe that already exhausted its deadline must not be followed by a
    // second probe against the same wedged endpoint: two bounded probes in
    // series is a doubled bound, which is what this fix exists to prevent.
    if degraded {
        return Err(command_failure_message(
            "remote Homeboy identity probe did not complete within its read-only bound",
            &output,
        ));
    }
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if timeout.is_zero() {
        return Err(
            "runner admission observation deadline exhausted before remote Homeboy version"
                .to_string(),
        );
    }
    let command = remote_homeboy_version_command(homeboy);
    let started = std::time::Instant::now();
    let output = client.execute_with_timeout(&command, timeout);
    crate::readonly_probe::record_probe_outcome(
        "runner_homeboy_version",
        runner_id,
        started,
        timeout,
        &output,
    );
    let version = parse_remote_homeboy_version(&output)?;
    Ok(RemoteHomeboyIdentity {
        version: normalize_homeboy_version_owned(&version),
        build_identity: None,
        daemon_recovery_capabilities: None,
    })
}

fn remote_homeboy_identity_command(homeboy: &str) -> String {
    format!("{} self identity", shell::quote_arg(homeboy))
}

pub(super) fn parse_self_identity_output(output: &str) -> Option<RemoteHomeboyIdentity> {
    let body: Value = parse_json_from_mixed_stdout(output).ok()?;
    let data = body.get("data").unwrap_or(&body);
    let version = data.get("version")?.as_str()?.trim();
    let display = data
        .get("display")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| immutable_build_identity(value));
    if version.is_empty() {
        return None;
    }
    let build_identity = display.map(str::to_string).or_else(|| {
        let commit = data
            .get("git_commit")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let dirty = data
            .get("git_dirty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(format!(
            "homeboy {}+{}{}",
            normalize_homeboy_version_owned(version),
            commit,
            if dirty { "-dirty" } else { "" }
        ))
    });
    let daemon_recovery_capabilities = data
        .get("daemon_recovery_capabilities")
        .and_then(parse_daemon_recovery_capabilities);
    Some(RemoteHomeboyIdentity {
        version: version.to_string(),
        build_identity,
        daemon_recovery_capabilities,
    })
}

/// Tolerantly parse the typed daemon-recovery capability list.
///
/// `None` is returned for an absent field, a JSON `null`, a non-array value,
/// or an array that does not deserialize as [`LabCapabilityVersion`] entries;
/// the caller then falls back to the help scrape for those recovery contracts
/// (older binary). Unknown ids and future versions are preserved verbatim:
/// matching is by id, and a typed list — even an explicitly empty one — is
/// authoritative whenever it parses.
fn parse_daemon_recovery_capabilities(value: &Value) -> Option<Vec<LabCapabilityVersion>> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

fn immutable_build_identity(identity: &str) -> bool {
    normalize_homeboy_version_owned(identity)
        .split_once('+')
        .is_some_and(|(version, commit)| !version.is_empty() && !commit.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdentityComparison {
    Match,
    Mismatch,
    Unverifiable,
}

pub(super) fn compare_identities(left: Option<&str>, right: Option<&str>) -> IdentityComparison {
    match (left, right) {
        (Some(left), Some(right))
            if immutable_build_identity(left) && immutable_build_identity(right) =>
        {
            if left.trim() == right.trim() {
                IdentityComparison::Match
            } else {
                IdentityComparison::Mismatch
            }
        }
        _ => IdentityComparison::Unverifiable,
    }
}

pub(super) fn compare_build_commits(left: Option<&str>, right: Option<&str>) -> IdentityComparison {
    match (left.and_then(build_commit), right.and_then(build_commit)) {
        (Some(left), Some(right)) if left == right => IdentityComparison::Match,
        (Some(_), Some(_)) => IdentityComparison::Mismatch,
        _ => IdentityComparison::Unverifiable,
    }
}

fn build_commit(identity: &str) -> Option<String> {
    immutable_build_identity(identity)
        .then(|| normalize_homeboy_version_owned(identity))
        .and_then(|identity| {
            identity
                .split_once('+')
                .map(|(_, commit)| commit.to_string())
        })
        .map(|commit| commit.strip_suffix("-dirty").unwrap_or(&commit).to_string())
}

pub(super) struct SshTunnelOutput {
    pub(super) pid: Option<u32>,
    pub(super) process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
    pub(super) stderr: String,
    pub(super) success: bool,
    child: Option<std::process::Child>,
}

impl SshTunnelOutput {
    pub(super) fn release_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    pub(super) fn contain_child(&mut self) {
        if let (Some(mut child), Some(identity)) =
            (self.child.take(), self.process_start_identity.as_ref())
        {
            let _ = homeboy_core::process::terminate_verified_isolated_process_group_and_reap(
                &mut child,
                &into_process_start_identity(identity),
                Duration::from_millis(200),
            );
        }
    }
}

fn into_process_start_identity(
    identity: &RunnerTunnelProcessStartIdentity,
) -> homeboy_core::process::ProcessStartIdentity {
    match identity {
        RunnerTunnelProcessStartIdentity::Linux { starttime_ticks } => {
            homeboy_core::process::ProcessStartIdentity::Linux {
                starttime_ticks: *starttime_ticks,
            }
        }
        RunnerTunnelProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        } => homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds: *start_seconds,
            start_microseconds: *start_microseconds,
        },
    }
}

fn capture_tunnel_identity(
    child: &mut std::process::Child,
) -> std::result::Result<RunnerTunnelProcessStartIdentity, String> {
    let pid = child.id();
    match homeboy_core::process::process_start_identity(pid) {
        Ok(Some(homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks })) => {
            Ok(RunnerTunnelProcessStartIdentity::Linux { starttime_ticks })
        }
        Ok(Some(homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        })) => Ok(RunnerTunnelProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        }),
        Ok(None) => Err("new SSH forward exited before identity capture".to_string()),
        Err(error) => Err(format!("capture SSH forward identity: {error}")),
    }
}

fn contain_tunnel_child(child: &mut std::process::Child) {
    if let Ok(Some(identity)) = homeboy_core::process::process_start_identity(child.id()) {
        let _ = homeboy_core::process::terminate_verified_isolated_process_group_and_reap(
            child,
            &identity,
            Duration::from_millis(200),
        );
    }
}

pub(super) fn open_loopback_tunnel(
    server: &Server,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
    loopback_transport: bool,
) -> SshTunnelOutput {
    if loopback_transport {
        return SshTunnelOutput {
            pid: None,
            process_start_identity: None,
            stderr: String::new(),
            success: true,
            child: None,
        };
    }

    let mut args = homeboy_core::server::ssh_args::server_option_args(
        server,
        homeboy_core::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            exit_on_forward_failure: true,
            disable_multiplexing: true,
            port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Lowercase),
            ..homeboy_core::server::ssh_args::SshArgOptions::default()
        },
    );
    args.extend([
        "-N".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{}:{}:{}", local_port, remote_host, remote_port),
        format!("{}@{}", server.user, server.host),
    ]);

    let mut command = std::process::Command::new("ssh");
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = spawn_tunnel_process(&mut command);
    match child {
        Ok(mut child) => match capture_tunnel_identity(&mut child) {
            Ok(identity) => SshTunnelOutput {
                pid: Some(child.id()),
                process_start_identity: Some(identity),
                stderr: String::new(),
                success: true,
                child: Some(child),
            },
            Err(error) => {
                contain_tunnel_child(&mut child);
                SshTunnelOutput {
                    pid: None,
                    process_start_identity: None,
                    stderr: error,
                    success: false,
                    child: None,
                }
            }
        },
        Err(err) => SshTunnelOutput {
            pid: None,
            process_start_identity: None,
            stderr: format!("SSH tunnel error: {}", err),
            success: false,
            child: None,
        },
    }
}

/// Open a controller-owned reverse forward. sshd allocates the runner-loopback
/// port, so parallel runner sessions cannot collide on a fixed proxy port.
pub(super) fn open_reverse_proxy_tunnel(
    server: &Server,
    proxy_host: &str,
    proxy_port: u16,
) -> SshTunnelOutput {
    let mut args = homeboy_core::server::ssh_args::server_option_args(
        server,
        homeboy_core::server::ssh_args::SshArgOptions {
            batch_mode: true,
            connect_timeout: true,
            exit_on_forward_failure: true,
            disable_multiplexing: true,
            port_flag: Some(homeboy_core::server::ssh_args::SshPortFlag::Lowercase),
            ..homeboy_core::server::ssh_args::SshArgOptions::default()
        },
    );
    args.extend([
        "-v".to_string(),
        "-N".to_string(),
        "-R".to_string(),
        format!("127.0.0.1:0:{proxy_host}:{proxy_port}"),
        format!("{}@{}", server.user, server.host),
    ]);
    let mut command = std::process::Command::new("ssh");
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let Ok(mut child) = spawn_tunnel_process(&mut command) else {
        return SshTunnelOutput {
            pid: None,
            process_start_identity: None,
            stderr: "SSH proxy forward could not start".to_string(),
            success: false,
            child: None,
        };
    };
    let pid = child.id();
    let identity = match capture_tunnel_identity(&mut child) {
        Ok(identity) => identity,
        Err(error) => {
            contain_tunnel_child(&mut child);
            return SshTunnelOutput {
                pid: None,
                process_start_identity: None,
                stderr: error,
                success: false,
                child: None,
            };
        }
    };
    let Some(stderr) = child.stderr.take() else {
        contain_tunnel_child(&mut child);
        return SshTunnelOutput {
            pid: None,
            process_start_identity: None,
            stderr: "SSH proxy forward did not expose stderr".to_string(),
            success: false,
            child: None,
        };
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = String::new();
        for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
            output.push_str(&line);
            output.push('\n');
            if let Some(port) = allocated_remote_port(&line) {
                let _ = sender.send(Ok(port));
                return;
            }
        }
        let _ = sender.send(Err(output));
    });
    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(port)) => SshTunnelOutput {
            pid: Some(pid),
            process_start_identity: Some(identity),
            stderr: port.to_string(),
            success: true,
            child: Some(child),
        },
        Ok(Err(stderr)) => {
            contain_tunnel_child(&mut child);
            SshTunnelOutput {
                pid: None,
                process_start_identity: None,
                stderr,
                success: false,
                child: None,
            }
        }
        Err(_) => {
            contain_tunnel_child(&mut child);
            SshTunnelOutput {
                pid: None,
                process_start_identity: None,
                stderr: "SSH proxy forward did not report an allocated remote port".to_string(),
                success: false,
                child: None,
            }
        }
    }
}

pub(super) fn allocated_remote_port(line: &str) -> Option<u16> {
    let port = line
        .split_once("Allocated port ")?
        .1
        .split_whitespace()
        .next()?;
    port.parse().ok()
}

#[cfg(test)]
mod proxy_forward_tests {
    use super::{allocated_remote_port, contain_tunnel_child, spawn_tunnel_process};
    use std::time::Duration;

    #[test]
    fn reads_the_port_allocated_by_openssh() {
        assert_eq!(
            allocated_remote_port("Allocated port 42731 for remote forward to 127.0.0.1:8080"),
            Some(42731)
        );
        assert_eq!(
            allocated_remote_port("debug1: forwarding established"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_proxy_forward_setup_reaps_its_dedicated_group() {
        let descendant = tempfile::NamedTempFile::new().expect("descendant pid file");
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            &format!("sleep 60 & echo $! > {}; wait", descendant.path().display()),
        ]);
        let mut child = spawn_tunnel_process(&mut command).expect("spawn child");
        let pid = child.id();
        assert!(homeboy_core::process::pid_is_running(pid));

        let descendant_pid = (0..20)
            .find_map(|_| {
                let pid = std::fs::read_to_string(descendant.path())
                    .ok()
                    .and_then(|value| value.trim().parse().ok());
                std::thread::sleep(Duration::from_millis(10));
                pid
            })
            .expect("descendant PID");

        contain_tunnel_child(&mut child);

        assert!(!homeboy_core::process::pid_is_running(pid));
        assert!(!homeboy_core::process::pid_is_running(descendant_pid));
    }
}

pub(super) fn spawn_tunnel_process(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    command.spawn()
}

#[derive(Debug, Clone)]
pub(super) struct RemoteDaemon {
    pub(super) address: String,
    pub(super) pid: Option<u32>,
    pub(super) lease_id: Option<String>,
    pub(super) version: Option<String>,
    pub(super) build_identity: Option<String>,
    pub(super) inspected_freshness: Option<DaemonFreshnessReport>,
}

#[derive(Debug, Clone)]
pub(super) struct RemoteDaemonStatus {
    pub(super) daemon: Option<RemoteDaemon>,
    pub(super) stale_reason: Option<String>,
    pub(super) stale_reason_code: Option<DaemonStaleReasonCode>,
    pub(super) fresh: bool,
    pub(super) reachable: bool,
    pub(super) active_jobs: usize,
    /// The typed `/jobs` view is independently required before replacing a
    /// reachable stale daemon. Missing or malformed evidence fails closed.
    pub(super) work_evidence: RemoteDaemonWorkEvidence,
    pub(super) endpoint_probe_error: Option<String>,
    pub(super) termination_evidence: Option<homeboy_core::daemon::DaemonTerminationEvidence>,
    /// The daemon's own recovery authorization survives SSH status parsing.
    /// Callers must not recreate a destructive action from stale reason alone.
    pub(super) daemon_freshness: Option<DaemonFreshnessReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteDaemonWorkEvidence {
    Unknown,
    ActiveOrUnresolved(usize),
    AuthoritativelyIdle,
}

/// The remote daemon is the authority for lifecycle decisions. Consumers must
/// derive zero-job and rotation decisions from this snapshot rather than
/// interpreting a persisted controller session as current daemon state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteDaemonAuthoritySnapshot {
    pub(super) source: &'static str,
    pub(super) active_jobs: usize,
    pub(super) active_lease_id: Option<String>,
    pub(super) proven_idle: bool,
    pub(super) safe_to_rotate: bool,
}

pub(super) fn authority_snapshot(status: &RemoteDaemonStatus) -> RemoteDaemonAuthoritySnapshot {
    let proven_idle = status.reachable
        && status.endpoint_probe_error.is_none()
        && status.active_jobs == 0
        && status.work_evidence.is_authoritatively_idle();
    let daemon = status.daemon.as_ref();
    let active_lease_id = daemon.and_then(|daemon| daemon.lease_id.clone());
    let safe_to_rotate = proven_idle
        && daemon.is_some_and(|daemon| daemon.pid.is_some())
        && active_lease_id
            .as_deref()
            .is_some_and(|lease| !lease.is_empty());
    RemoteDaemonAuthoritySnapshot {
        source: "remote daemon SSH status and typed /jobs probe",
        active_jobs: status.active_jobs,
        active_lease_id,
        proven_idle,
        safe_to_rotate,
    }
}

/// Return the one live lease that may replace a ledger containing only stale
/// leases. The remote daemon status and typed jobs endpoint are authoritative;
/// a persisted lease is never used to stop a different daemon.
pub(super) fn authoritative_idle_lease_for_stale_generations(
    status: &RemoteDaemonStatus,
    persisted_leases: &[String],
) -> std::result::Result<Option<String>, String> {
    if persisted_leases.is_empty() {
        return Ok(None);
    }
    let snapshot = authority_snapshot(status);
    if status.daemon.is_none() {
        return Err(
            "authoritative daemon reconciliation cannot prove a live daemon lease and PID"
                .to_string(),
        );
    }
    let lease_id = snapshot.active_lease_id.as_deref().ok_or_else(|| {
        "authoritative daemon reconciliation cannot prove the live daemon lease".to_string()
    })?;
    if persisted_leases
        .iter()
        .any(|persisted| persisted == lease_id)
    {
        return Ok(None);
    }
    if !snapshot.safe_to_rotate {
        return Err(format!(
            "authoritative daemon reconciliation from {} requires a reachable lease/PID, successful endpoint probes, and zero typed active jobs",
            snapshot.source
        ));
    }
    Ok(Some(lease_id.to_string()))
}

/// A stale generation registry may be retired only after the remote authority
/// proves there is no process left that could own its recorded work. A missing
/// lease is sufficient only when the authoritative jobs view is also zero. A
/// PID-dead result is specific to one lease, so it cannot retire an inventory
/// containing any other lease.
pub(super) fn authoritative_stale_generations_are_dead(
    status: &RemoteDaemonStatus,
    persisted_leases: &[String],
) -> bool {
    if persisted_leases.is_empty() || status.reachable || status.active_jobs != 0 {
        return false;
    }
    match status.daemon.as_ref() {
        Some(daemon) => {
            status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
                && daemon.lease_id.as_deref().is_some_and(|lease| {
                    persisted_leases.iter().all(|persisted| persisted == lease)
                })
                && daemon.pid.is_some()
        }
        None => status.stale_reason_code == Some(DaemonStaleReasonCode::LeaseMissing),
    }
}

impl RemoteDaemonWorkEvidence {
    fn from_unresolved_count(count: usize) -> Self {
        if count == 0 {
            Self::AuthoritativelyIdle
        } else {
            Self::ActiveOrUnresolved(count)
        }
    }

    fn is_authoritatively_idle(self) -> bool {
        self == Self::AuthoritativelyIdle
    }

    #[cfg(test)]
    pub(in crate::connection) fn idle() -> Self {
        Self::AuthoritativelyIdle
    }
}

pub(super) fn remote_daemon_recovery_freshness_from_status(
    runner_id: &str,
    status: &RemoteDaemonStatus,
) -> DaemonFreshnessReport {
    let daemon = status.daemon.as_ref();
    let lease_id = daemon.and_then(|daemon| daemon.lease_id.clone());
    let pid = daemon.and_then(|daemon| daemon.pid);
    let adoption_eligible = status
        .daemon_freshness
        .as_ref()
        .is_some_and(|freshness| freshness.adoption_command.is_some());
    let proven_dead = status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
        && lease_id.is_some()
        && pid.is_some();
    let leaseless_reconciliation_available = status.active_jobs > 0
        && matches!(
            status.stale_reason_code,
            Some(
                DaemonStaleReasonCode::LeaseMissing
                    | DaemonStaleReasonCode::LeaseCorrupt
                    | DaemonStaleReasonCode::VersionMismatch
            )
        );
    // A remote daemon that self-reports fresh and authoritatively idle (zero
    // active jobs proven via its typed `/jobs` view) is not a recovery hazard:
    // the controller simply lost its session to a healthy daemon. Reconnecting
    // is the safe, deterministic recovery. Without this case such a daemon fell
    // into the generic "lease evidence unavailable; active jobs are protected"
    // branch below, which produced no adoption command and left Lab placement
    // waiting for controller generation admission (#8694). "Protected active
    // jobs" is also nonsensical when there are provably zero.
    let authority = authority_snapshot(status);
    let recoverable_fresh_idle = !proven_dead
        && !leaseless_reconciliation_available
        && status.fresh
        && authority.proven_idle;
    let mut ownership_evidence = if proven_dead {
        Some(format!(
            "remote daemon status over SSH proved PID {} is dead for lease `{}`",
            pid.expect("proven dead PID"),
            lease_id.as_deref().expect("proven dead lease")
        ))
    } else if leaseless_reconciliation_available {
        Some("active durable jobs require explicit reconciliation; it will verify the owner lock, process list, and configured listener before terminalizing them".to_string())
    } else if recoverable_fresh_idle {
        Some(format!(
            "{} proved the remote daemon is fresh with zero authoritatively idle jobs; the controller session can be safely reconnected",
            authority.source
        ))
    } else {
        Some("remote daemon lease evidence is unavailable; active jobs are protected from implicit replacement".to_string())
    };
    if let Some(error) = &status.endpoint_probe_error {
        ownership_evidence = Some(format!(
            "{}; reachable endpoint identity probe failed: {error}",
            ownership_evidence.unwrap_or_default()
        ));
    }
    if let Some(stale_reason) = &status.stale_reason {
        ownership_evidence = Some(format!(
            "{}; inspected stale reason: {stale_reason}",
            ownership_evidence.unwrap_or_default()
        ));
    }
    // The repair plan and the adoption command are the same action in two
    // shapes, so they are built from one command and one code. A remote runner
    // is exactly the daemon an operator cannot look at, so leaving this empty
    // and falling back to generic reconnect prose discards the evidence the SSH
    // probe just paid for (#10302).
    let repair_step = if proven_dead && adoption_eligible {
        Some(daemon_repair::action_step(
            daemon_repair::RUNNER_ADOPT_ORPHAN_LEASE,
            daemon_repair::adopt_orphan_lease_action(
                runner_id,
                lease_id.as_deref().expect("proven dead lease"),
            ),
        ))
    } else if leaseless_reconciliation_available {
        Some(daemon_repair::action_step(
            daemon_repair::RUNNER_RECONCILE_LEASELESS_ORPHANS,
            daemon_repair::reconcile_leaseless_orphans_action(runner_id),
        ))
    } else if status.active_jobs == 0
        && status
            .stale_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("foreground daemon candidates"))
    {
        daemon_repair::reconcile_unleased_candidates_action(runner_id).map(|action| {
            daemon_repair::action_step(daemon_repair::RUNNER_RECONCILE_UNLEASED_CANDIDATES, action)
        })
    } else if recoverable_fresh_idle {
        Some(daemon_repair::action_step(
            daemon_repair::RUNNER_CONNECT,
            daemon_repair::connect_action(runner_id),
        ))
    } else {
        None
    };
    let adoption_command = repair_step.as_ref().map(|step| step.command.clone());
    let repair_plan: Vec<_> = repair_step.into_iter().collect();
    DaemonFreshnessReport {
        fresh: status.fresh,
        stale_reason_code: status.stale_reason_code,
        restartable: false,
        lease_id,
        pid,
        recovery_evidence: Some(if proven_dead {
            DaemonRecoveryEvidence::ProvenDead
        } else if recoverable_fresh_idle {
            DaemonRecoveryEvidence::Recoverable
        } else {
            DaemonRecoveryEvidence::Unavailable
        }),
        ownership_evidence,
        adoption_command,
        binary_hash: None,
        daemon_version: daemon.and_then(|daemon| daemon.version.clone()),
        daemon_build_identity: daemon.and_then(|daemon| daemon.build_identity.clone()),
        runtime_paths: None,
        active_jobs: status.active_jobs,
        termination_evidence: status.termination_evidence.clone(),
        repair_plan,
    }
}

pub(super) fn unavailable_recovery_freshness(
    runner_id: &str,
    error: impl Into<String>,
) -> DaemonFreshnessReport {
    DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(DaemonStaleReasonCode::TransportUnreachable),
        restartable: false,
        lease_id: None,
        pid: None,
        recovery_evidence: Some(DaemonRecoveryEvidence::Unavailable),
        ownership_evidence: Some(format!(
            "remote daemon recovery evidence unavailable: {}",
            error.into()
        )),
        // No lease, PID, or job count survived the transport failure, so no
        // lease-specific action can be named. Rebuilding the controller session
        // is the only honest step, and it is emitted as typed steps rather than
        // left to a downstream prose fallback.
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: daemon_repair::reconnect_plan(runner_id),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RemoteDaemonConnectAction {
    Reattach,
    Start,
    ReplaceIdleStale,
    ReplaceUnhealthyExactOwner,
}

pub(super) struct RemoteDaemonEnsureRequest<'a> {
    pub(super) client: &'a SshClient,
    pub(super) homeboy: &'a str,
    pub(super) runner_id: &'a str,
    pub(super) previous_session: Option<&'a RunnerSession>,
    pub(super) configured_identity: &'a str,
    pub(super) orphan_lease_id: Option<&'a str>,
    pub(super) confirmed_no_pid_job_ids: &'a [uuid::Uuid],
    pub(super) live_lease_expectation: Option<(&'a str, u32)>,
    pub(super) replacement_operation_id: Option<&'a str>,
    pub(super) admission_fence: Option<&'a crate::generation_store::AdmissionFence>,
    pub(super) registry_lock_held: bool,
    /// The typed daemon-recovery capabilities advertised in the remote's
    /// self-identity report. `None` (older binary) keeps the help scrape
    /// fallback for `daemon ensure-running --replacement-operation-id`.
    pub(super) daemon_recovery_capabilities: Option<&'a [LabCapabilityVersion]>,
}

pub(super) fn ensure_remote_daemon(
    request: RemoteDaemonEnsureRequest<'_>,
) -> homeboy_core::Result<RemoteDaemon> {
    ensure_remote_daemon_inner(request).map_err(|error| match error {
        RemoteDaemonEnsureError::Other(message) => {
            homeboy_core::Error::internal_unexpected(message)
        }
        RemoteDaemonEnsureError::EnsureRunning(failure) => {
            let mut error = homeboy_core::Error::internal_unexpected(failure.message);
            error.details = serde_json::to_value(&failure.evidence_ref)
                .map(|reference| json!({ "failure_evidence_ref": reference }))
                .unwrap_or(Value::Null);
            error
        }
    })
}

fn ensure_remote_daemon_inner(
    request: RemoteDaemonEnsureRequest<'_>,
) -> std::result::Result<RemoteDaemon, RemoteDaemonEnsureError> {
    let RemoteDaemonEnsureRequest {
        client,
        homeboy,
        runner_id,
        previous_session,
        configured_identity,
        orphan_lease_id,
        confirmed_no_pid_job_ids,
        live_lease_expectation,
        replacement_operation_id,
        admission_fence,
        registry_lock_held,
        daemon_recovery_capabilities,
    } = request;
    let mut status = remote_daemon_status(client, homeboy)?;
    probe_remote_daemon_endpoint(client, &mut status, Some(runner_id));
    if let Some(lease_id) = orphan_lease_id {
        if let Some(fence) = admission_fence {
            return Err(RemoteDaemonEnsureError::Other(format!(
                "runner `{runner_id}` generation `{}` has {} unresolved active job(s); refusing orphan adoption before terminal job evidence is available",
                fence.generation, fence.active_job_count,
            )));
        }
        if status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead)
            && status
                .daemon
                .as_ref()
                .and_then(|daemon| daemon.lease_id.as_deref())
                == Some(lease_id)
        {
            return Ok(remote_daemon_adopt_orphan(
                client,
                homeboy,
                lease_id,
                confirmed_no_pid_job_ids,
            )?);
        }
    }
    if !confirmed_no_pid_job_ids.is_empty() {
        return Err(RemoteDaemonEnsureError::Other(
            "--confirm-untracked-child-dead applies only when the remote daemon reports the exact requested lease as PID-dead"
                .to_string(),
        ));
    }
    // The real runner id, not a `<runner-id>` placeholder: this report now
    // carries an executable repair plan, and a plan naming a command that does
    // not exist is worse than no plan at all (#10302).
    let inspected_freshness = remote_daemon_recovery_freshness_from_status(runner_id, &status);
    let action = remote_daemon_connect_action_for_runner(
        previous_session,
        &status,
        configured_identity,
        runner_id,
        live_lease_expectation,
    )?;
    let action = fence_generation_admission(action, admission_fence, runner_id)?;
    match action {
        RemoteDaemonConnectAction::Reattach => {
            let mut daemon = status.daemon.ok_or_else(|| {
                "remote daemon reattach selected without a daemon lease".to_string()
            })?;
            daemon.inspected_freshness = Some(inspected_freshness);
            Ok(daemon)
        }
        RemoteDaemonConnectAction::Start => {
            negotiate_ensure_running_operation_id(
                client,
                homeboy,
                replacement_operation_id,
                daemon_recovery_capabilities,
            )?;
            journal_ensure_running_replay(
                runner_id,
                homeboy,
                replacement_operation_id,
                registry_lock_held,
            )?;
            remote_daemon_ensure_running(client, homeboy, runner_id, replacement_operation_id)
        }
        RemoteDaemonConnectAction::ReplaceIdleStale
        | RemoteDaemonConnectAction::ReplaceUnhealthyExactOwner => {
            // Prove idempotent replacement support before stopping A. Otherwise
            // a controller response loss could leave no recoverable owner.
            negotiate_ensure_running_operation_id(
                client,
                homeboy,
                replacement_operation_id,
                daemon_recovery_capabilities,
            )?;
            // Persist B's idempotent receipt key before removing A. A retry then
            // replays this command before inspecting or replacing any lease.
            journal_ensure_running_replay(
                runner_id,
                homeboy,
                replacement_operation_id,
                registry_lock_held,
            )?;
            let daemon = status.daemon.as_ref().expect("replacement requires daemon");
            let lease_id = daemon
                .lease_id
                .as_deref()
                .expect("replacement requires lease");
            remote_daemon_force_stop(client, homeboy, lease_id)?;
            let replacement =
                remote_daemon_ensure_running(client, homeboy, runner_id, replacement_operation_id)?;
            Ok(verify_remote_daemon_replacement(
                client,
                homeboy,
                &replacement,
                configured_identity,
            )?)
        }
    }
}

pub(super) fn fence_generation_admission(
    action: RemoteDaemonConnectAction,
    fence: Option<&crate::generation_store::AdmissionFence>,
    runner_id: &str,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    let Some(fence) = fence else {
        return Ok(action);
    };
    if action == RemoteDaemonConnectAction::Reattach {
        return Ok(action);
    }
    Err(format!(
        "runner `{runner_id}` generation `{}` has {} unresolved active job(s); refusing to create or replace an admission daemon. Reattach the live generation when available, or run `homeboy runner reconcile {runner_id}` after terminal job evidence is available",
        fence.generation, fence.active_job_count,
    ))
}

fn journal_ensure_running_replay(
    runner_id: &str,
    homeboy: &str,
    replacement_operation_id: Option<&str>,
    registry_lock_held: bool,
) -> std::result::Result<(), String> {
    if replacement_operation_id.is_none() {
        return Ok(());
    }
    let command = remote_daemon_ensure_running_command(homeboy, replacement_operation_id);
    let write = if registry_lock_held {
        crate::generation_store::record_replacement_operation_replay_locked(
            runner_id,
            "ensure-running",
            &command,
        )
    } else {
        crate::generation_store::record_replacement_operation_replay(
            runner_id,
            "ensure-running",
            &command,
        )
    };
    write.map_err(|error| error.to_string())
}

pub(super) fn negotiate_ensure_running_operation_id(
    client: &SshClient,
    homeboy: &str,
    replacement_operation_id: Option<&str>,
    daemon_recovery_capabilities: Option<&[LabCapabilityVersion]>,
) -> std::result::Result<(), String> {
    let Some(_) = replacement_operation_id else {
        return Ok(());
    };
    // A runner that advertises the typed `--replacement-operation-id`
    // capability skips the help scrape entirely; an older runner still
    // negotiates from `daemon ensure-running --help`.
    let advertised = homeboy_lab_runner_contract::daemon_recovery_capability_negotiated(
        daemon_recovery_capabilities,
        homeboy_lab_runner_contract::DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY,
        || {
            let command = format!("{} daemon ensure-running --help", shell::quote_arg(homeboy));
            let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
            if !output.success {
                return Err("remote Homeboy must be upgraded: unable to negotiate daemon ensure-running --replacement-operation-id before mutation".to_string());
            }
            if !declared_long_options(&output.stdout).contains("--replacement-operation-id") {
                return Err("remote Homeboy must be upgraded: daemon ensure-running does not support --replacement-operation-id".to_string());
            }
            Ok(true)
        },
    )?;
    if !advertised {
        return Err("remote Homeboy must be upgraded: daemon ensure-running does not advertise the --replacement-operation-id capability".to_string());
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn remote_daemon_connect_action(
    previous_session: Option<&RunnerSession>,
    status: &RemoteDaemonStatus,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    remote_daemon_connect_action_for_runner(
        previous_session,
        status,
        &homeboy_product_identity::build_identity().display,
        "<runner-id>",
        None,
    )
}

pub(super) fn remote_daemon_connect_action_for_runner(
    previous_session: Option<&RunnerSession>,
    status: &RemoteDaemonStatus,
    expected_identity: &str,
    runner_id: &str,
    live_lease_expectation: Option<(&str, u32)>,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    let Some(daemon) = status.daemon.as_ref() else {
        if status.active_jobs > 0 {
            return Err(format!(
                "remote daemon status has {} active job(s) but no daemon state; refusing ensure-running or implicit replacement. Inspect `homeboy daemon status` and use the explicit active-job recovery guidance before retrying.",
                status.active_jobs
            ));
        }
        return Ok(RemoteDaemonConnectAction::Start);
    };

    // `daemon status` proves this exact recorded PID is no longer running.
    // With no durable work left to reconcile, ensure-running can safely replace
    // the stale lease without requiring another orphan-adoption cycle.
    if status.stale_reason_code == Some(DaemonStaleReasonCode::PidDead) && status.active_jobs == 0 {
        return Ok(RemoteDaemonConnectAction::Start);
    }

    if !status.reachable {
        return Err(format!(
            "remote daemon is unreachable; refusing to replace or persist a session{}",
            active_job_recovery_guidance(status.active_jobs)
        ));
    }
    if daemon.lease_id.as_deref().is_none_or(str::is_empty) || daemon.pid.is_none() {
        return Err(format!(
            "reachable remote daemon did not report both lease and PID; refusing to replace or persist a session{}",
            active_job_recovery_guidance(status.active_jobs)
        ));
    }
    if parse_loopback_daemon_addr(&daemon.address).is_err() {
        return Err(format!(
            "reachable remote daemon did not report a loopback address; refusing to replace or persist a session{}",
            active_job_recovery_guidance(status.active_jobs)
        ));
    }
    if let Some(endpoint_error) = status.endpoint_probe_error.as_deref() {
        let exact_owner = previous_session.is_some_and(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
                && session.remote_daemon_lease_id.as_deref() == daemon.lease_id.as_deref()
                && session.remote_daemon_pid == daemon.pid
                && session.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
                && session.homeboy_build_identity.as_deref().map(str::trim)
                    == Some(expected_identity.trim())
        });
        if exact_owner && status.fresh && status.active_jobs == 0 {
            return Ok(RemoteDaemonConnectAction::ReplaceUnhealthyExactOwner);
        }
        if live_lease_expectation == daemon.lease_id.as_deref().zip(daemon.pid) {
            return Err(lease_reconciliation_failure(
                previous_session
                    .and_then(|session| session.remote_daemon_lease_id.as_deref())
                    .unwrap_or("none or corrupt"),
                daemon.lease_id.as_deref().expect("checked above"),
                daemon,
                status,
                expected_identity,
                runner_id,
                live_lease_expectation,
            ));
        }
        return Err(format!(
            "remote daemon listener is unhealthy ({endpoint_error}); refusing replacement because exact lease/PID/address/build ownership with authoritatively zero active jobs was not proven{}",
            active_job_recovery_guidance(status.active_jobs)
        ));
    }
    // A lease-less freshness report ordinarily prevents replacement. The one
    // bounded exception is an idle daemon whose identity differs from the
    // configured executable and whose typed `/jobs` endpoint independently
    // proves no active or unresolved work. The stop itself remains lease-bound
    // through Homeboy's daemon lifecycle command.
    if !status.fresh {
        if live_lease_expectation
            == Some((
                daemon.lease_id.as_deref().expect("checked above"),
                daemon.pid.expect("checked above"),
            ))
            && daemon
                .build_identity
                .as_deref()
                .is_some_and(|identity| !identity.trim().is_empty())
            && status.endpoint_probe_error.is_none()
        {
            return Ok(RemoteDaemonConnectAction::Reattach);
        }
        if status.active_jobs == 0
            && status.work_evidence.is_authoritatively_idle()
            && status.endpoint_probe_error.is_none()
            && daemon
                .build_identity
                .as_deref()
                .is_some_and(|identity| identity.trim() != expected_identity.trim())
        {
            return Ok(RemoteDaemonConnectAction::ReplaceIdleStale);
        }
        return reattach_only_if_same_lease(
            previous_session,
            daemon,
            runner_id,
            status,
            live_lease_expectation,
        );
    }

    let healthy = status.fresh && status.reachable;

    if healthy {
        if previous_session.is_none() && status.active_jobs > 0 {
            let daemon = status.daemon.as_ref().expect("healthy daemon exists");
            let daemon_identity = daemon.build_identity.as_deref().ok_or_else(|| format!(
                "remote daemon has {} active job(s) but its reachable endpoint did not provide a build identity; refusing reattachment or replacement",
                status.active_jobs
            ))?;
            let daemon_version = daemon.version.as_deref().ok_or_else(|| format!(
                "remote daemon has {} active job(s) but its reachable endpoint did not provide a version; refusing reattachment or replacement",
                status.active_jobs
            ))?;
            if daemon_identity.trim() != expected_identity.trim() {
                return Err(format!(
                    "remote daemon has {} active job(s) under reachable lease `{}` (PID {}) but build identity `{daemon_identity}` / version `{daemon_version}` does not match this configured runner binary `{expected_identity}`; refusing replacement. Run a controller pinned to `{daemon_identity}` and retry `homeboy runner connect <runner-id>` to reattach this exact lease.",
                    status.active_jobs,
                    daemon.lease_id.as_deref().unwrap_or("unavailable"),
                    daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
                ));
            }
        }
        if let Some(session) = previous_session.filter(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
        }) {
            if session.remote_daemon_lease_id.is_none() {
                if session.remote_daemon_pid == daemon.pid
                    && session.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
                {
                    return Ok(RemoteDaemonConnectAction::Reattach);
                }
                return Err("persisted direct-SSH runner session has no daemon lease and does not match the live daemon PID/address; refusing replacement".to_string());
            }
            let expected_lease = session.remote_daemon_lease_id.as_deref().expect("checked");
            let actual_lease = daemon.lease_id.as_deref().ok_or_else(|| {
                "live remote daemon did not report a lease; refusing to replace it".to_string()
            })?;
            if expected_lease != actual_lease {
                if live_lease_expectation
                    == Some((actual_lease, daemon.pid.expect("checked above")))
                    && daemon.build_identity.as_deref().map(str::trim)
                        == Some(expected_identity.trim())
                    && status.endpoint_probe_error.is_none()
                {
                    return Ok(RemoteDaemonConnectAction::Reattach);
                }
                return Err(lease_reconciliation_failure(
                    expected_lease,
                    actual_lease,
                    daemon,
                    status,
                    expected_identity,
                    runner_id,
                    live_lease_expectation,
                ));
            }
        } else if live_lease_expectation
            != Some((
                daemon.lease_id.as_deref().expect("checked above"),
                daemon.pid.expect("checked above"),
            ))
            || daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim())
            || status.endpoint_probe_error.is_some()
        {
            return Err(lease_reconciliation_failure(
                "none or corrupt",
                daemon.lease_id.as_deref().expect("checked above"),
                daemon,
                status,
                expected_identity,
                runner_id,
                live_lease_expectation,
            ));
        }
        return Ok(RemoteDaemonConnectAction::Reattach);
    }

    Ok(RemoteDaemonConnectAction::Start)
}

fn reattach_only_if_same_lease(
    previous_session: Option<&RunnerSession>,
    daemon: &RemoteDaemon,
    runner_id: &str,
    status: &RemoteDaemonStatus,
    live_lease_expectation: Option<(&str, u32)>,
) -> std::result::Result<RemoteDaemonConnectAction, String> {
    let persisted_lease = previous_session
        .filter(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
        })
        .and_then(|session| session.remote_daemon_lease_id.as_deref());
    if persisted_lease == daemon.lease_id.as_deref() {
        Ok(RemoteDaemonConnectAction::Reattach)
    } else {
        Err(lease_reconciliation_failure(
            persisted_lease.unwrap_or("none or corrupt"),
            daemon.lease_id.as_deref().unwrap_or("unavailable"),
            daemon,
            status,
            "not evaluated for stale daemon",
            runner_id,
            live_lease_expectation,
        ))
    }
}

fn lease_reconciliation_failure(
    expected_lease: &str,
    actual_lease: &str,
    daemon: &RemoteDaemon,
    status: &RemoteDaemonStatus,
    expected_identity: &str,
    runner_id: &str,
    live_lease_expectation: Option<(&str, u32)>,
) -> String {
    if live_lease_expectation == daemon.lease_id.as_deref().zip(daemon.pid) {
        let (blocker, action) = if let Some(error) = status.endpoint_probe_error.as_deref() {
            (
                format!("the endpoint identity/jobs probe failed: {error}"),
                "Restore endpoint identity/jobs availability",
            )
        } else if daemon
            .build_identity
            .as_deref()
            .is_none_or(|identity| identity.trim().is_empty())
        {
            (
                "the reachable endpoint did not provide a build identity".to_string(),
                "Restore endpoint build identity reporting",
            )
        } else if daemon.build_identity.as_deref().map(str::trim) != Some(expected_identity.trim())
        {
            (
                format!(
                    "live build identity `{}` does not match configured identity `{expected_identity}`",
                    daemon.build_identity.as_deref().expect("checked above")
                ),
                "Use a controller configured for the live identity or restore the endpoint to the configured identity",
            )
        } else {
            (
                "the required endpoint identity proof is unavailable".to_string(),
                "Restore the required endpoint identity proof",
            )
        };
        return format!(
            "explicit live lease adoption matched lease `{actual_lease}` and PID {}, but {blocker}; refusing to persist a session. No session state was changed. {action}, then verify it with `homeboy runner status {} --json` before retrying adoption.",
            daemon.pid.expect("matching expectation requires PID"),
            shell::quote_arg(runner_id),
        );
    }
    format!(
        "live remote daemon lease `{actual_lease}` differs from persisted session lease `{expected_lease}`; refusing to adopt or replace it because runner ownership is not proven (fresh={}, reachable={}, live identity `{}`, configured identity `{}`, endpoint probe `{}`). No session state was changed. Run `homeboy runner connect {} --adopt-live-lease {} --expected-live-pid {}` to explicitly adopt this observed lease/PID/build after revalidation. This is operator-confirmed recovery within the trusted remote SSH UID boundary; it never stops or replaces a daemon, and later lease drift fails closed. Run `homeboy runner status {} --json` to inspect it.",
        status.fresh,
        status.reachable,
        daemon.build_identity.as_deref().unwrap_or("unavailable"),
        expected_identity,
        status.endpoint_probe_error.as_deref().unwrap_or("verified"),
        shell::quote_arg(runner_id),
        shell::quote_arg(actual_lease),
        daemon.pid.map(|pid| pid.to_string()).as_deref().unwrap_or("unavailable"),
        shell::quote_arg(runner_id),
    )
}

fn active_job_recovery_guidance(active_jobs: usize) -> String {
    if active_jobs > 0 {
        format!(
            "; {active_jobs} active job(s) were not replaced. Inspect `homeboy daemon status` and use explicit active-job recovery guidance before retrying"
        )
    } else {
        String::new()
    }
}

pub(super) fn remote_daemon_status(
    client: &SshClient,
    homeboy: &str,
) -> std::result::Result<RemoteDaemonStatus, String> {
    remote_daemon_status_with_timeout(client, homeboy, REMOTE_DAEMON_STATUS_TIMEOUT, None)
}

pub(super) fn bounded_remote_daemon_status_with_timeout(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
    timeout: Duration,
) -> std::result::Result<RemoteDaemonStatus, String> {
    remote_daemon_status_with_timeout(client, homeboy, timeout, Some(runner_id))
}

fn remote_daemon_status_with_timeout(
    client: &SshClient,
    homeboy: &str,
    timeout: Duration,
    runner_id: Option<&str>,
) -> std::result::Result<RemoteDaemonStatus, String> {
    let command = format!("{} daemon status", shell::quote_arg(homeboy));
    let started = std::time::Instant::now();
    let output = client.execute_with_timeout(&command, timeout);
    if let Some(runner_id) = runner_id {
        crate::readonly_probe::record_probe_outcome(
            "runner_remote_daemon_status",
            Some(runner_id),
            started,
            timeout,
            &output,
        );
    }
    if !output.success {
        return Err(command_failure_message(
            "remote daemon status failed",
            &output,
        ));
    }
    let envelope = parse_envelope(&output.stdout)
        .map_err(|err| format!("remote daemon status returned invalid JSON: {}", err))?;
    if !envelope.success {
        return Err(format!(
            "remote daemon status returned an error: {}",
            envelope.error.unwrap_or(Value::Null)
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| "remote daemon status returned no data".to_string())?;
    let stale_reason = data
        .get("stale_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stale_reason_code = data
        .pointer("/freshness/stale_reason_code")
        .cloned()
        .and_then(|code| serde_json::from_value(code).ok());
    let termination_evidence = data
        .get("termination_evidence")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let daemon_freshness = data
        .pointer("/freshness")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    if !data
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(RemoteDaemonStatus {
            daemon: data.get("state").map(remote_daemon_from_state),
            stale_reason,
            stale_reason_code,
            fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reachable: data
                .get("reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active_jobs: remote_daemon_active_jobs(&data),
            work_evidence: RemoteDaemonWorkEvidence::Unknown,
            endpoint_probe_error: None,
            termination_evidence,
            daemon_freshness,
        });
    }
    let Some(state) = data.get("state") else {
        return Ok(RemoteDaemonStatus {
            daemon: None,
            stale_reason: Some(
                stale_reason
                    .unwrap_or_else(|| "remote daemon status has no lease state".to_string()),
            ),
            stale_reason_code,
            fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reachable: data
                .get("reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active_jobs: remote_daemon_active_jobs(&data),
            work_evidence: RemoteDaemonWorkEvidence::Unknown,
            endpoint_probe_error: None,
            termination_evidence,
            daemon_freshness,
        });
    };
    Ok(RemoteDaemonStatus {
        daemon: Some(remote_daemon_from_state(state)),
        stale_reason,
        stale_reason_code,
        fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
        reachable: data
            .get("reachable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        active_jobs: remote_daemon_active_jobs(&data),
        work_evidence: RemoteDaemonWorkEvidence::Unknown,
        endpoint_probe_error: None,
        termination_evidence,
        daemon_freshness,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use homeboy_core::{server, test_support};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    #[test]
    fn bounded_status_records_a_timed_out_remote_daemon_probe() {
        test_support::with_isolated_home(|home| {
            let daemon = home.path().join("slow-homeboy");
            std::fs::write(&daemon, "#!/bin/sh\nsleep 1\n")
                .expect("write slow remote Homeboy shim");
            let mut permissions = std::fs::metadata(&daemon)
                .expect("read remote Homeboy shim metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&daemon, permissions)
                .expect("make remote Homeboy shim executable");
            server::create(
                &serde_json::json!({ "id": "slow-runner", "host": "localhost", "user": "test" })
                    .to_string(),
                false,
            )
            .expect("create local server");
            let server_config = server::load("slow-runner").expect("load local server");
            let client = SshClient::from_server(&server_config, "slow-runner").expect("SSH client");

            crate::readonly_probe::clear_degradations();
            let started = Instant::now();
            let result = remote_daemon_status_with_timeout(
                &client,
                daemon.to_str().expect("daemon path"),
                Duration::from_millis(100),
                Some("slow-runner"),
            );

            assert!(result.is_err());
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "bounded status probe exceeded its deadline"
            );
            let degradations = crate::readonly_probe::take_degradations();
            assert_eq!(degradations.len(), 1);
            assert_eq!(degradations[0].probe, "runner_remote_daemon_status");
            assert_eq!(degradations[0].runner_id.as_deref(), Some("slow-runner"));
        });
    }
}

pub(super) fn probe_remote_daemon_endpoint(
    client: &SshClient,
    status: &mut RemoteDaemonStatus,
    runner_id: Option<&str>,
) {
    probe_remote_daemon_endpoint_until(
        client,
        status,
        runner_id,
        std::time::Instant::now() + crate::readonly_probe::readonly_probe_timeout(),
    );
}

pub(super) fn probe_remote_daemon_endpoint_until(
    client: &SshClient,
    status: &mut RemoteDaemonStatus,
    runner_id: Option<&str>,
    deadline: std::time::Instant,
) {
    if !status.reachable {
        return;
    }
    let Some(daemon) = status.daemon.as_mut() else {
        return;
    };
    if parse_loopback_daemon_addr(&daemon.address).is_err() {
        status.endpoint_probe_error = Some(
            "remote daemon status reported a non-loopback endpoint; refusing identity probe"
                .to_string(),
        );
        return;
    }
    let command = format!(
        "curl --fail --silent --show-error --max-time 2 {}/version",
        shell::quote_arg(&format!("http://{}", daemon.address))
    );
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if timeout.is_zero() {
        status.endpoint_probe_error = Some(
            "runner admission observation deadline exhausted before endpoint identity probe"
                .to_string(),
        );
        return;
    }
    let started = std::time::Instant::now();
    let output = client.execute_with_timeout(&command, timeout);
    crate::readonly_probe::record_probe_outcome(
        "runner_remote_endpoint_identity",
        runner_id,
        started,
        timeout,
        &output,
    );
    if !output.success {
        status.endpoint_probe_error = Some(command_failure_message(
            "remote daemon endpoint identity probe failed",
            &output,
        ));
        return;
    }
    let body: Value = match parse_json_from_mixed_stdout(&output.stdout) {
        Ok(body) => body,
        Err(error) => {
            status.endpoint_probe_error = Some(format!(
                "remote daemon endpoint identity probe returned invalid JSON: {error}"
            ));
            return;
        }
    };
    daemon.version = body
        .get("version")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/data/version").and_then(Value::as_str))
        .map(str::to_string);
    daemon.build_identity = body
        .pointer("/build_identity/display")
        .and_then(Value::as_str)
        .or_else(|| {
            body.pointer("/data/build_identity/display")
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    if daemon.version.is_none() || daemon.build_identity.is_none() {
        status.endpoint_probe_error = Some(
            "remote daemon endpoint identity probe did not return both version and build identity"
                .to_string(),
        );
    }
    let command = format!(
        "curl --fail --silent --show-error --max-time 2 {}/jobs",
        shell::quote_arg(&format!("http://{}", daemon.address))
    );
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if timeout.is_zero() {
        status.endpoint_probe_error = Some(
            "runner admission observation deadline exhausted before endpoint job probe".to_string(),
        );
        return;
    }
    let started = std::time::Instant::now();
    let output = client.execute_with_timeout(&command, timeout);
    crate::readonly_probe::record_probe_outcome(
        "runner_remote_typed_jobs",
        runner_id,
        started,
        timeout,
        &output,
    );
    if !output.success {
        status.endpoint_probe_error = Some(command_failure_message(
            "remote daemon typed job probe failed",
            &output,
        ));
        return;
    }
    let body: Value = match parse_json_from_mixed_stdout(&output.stdout) {
        Ok(body) => body,
        Err(error) => {
            status.endpoint_probe_error = Some(format!(
                "remote daemon typed job probe returned invalid JSON: {error}"
            ));
            return;
        }
    };
    let jobs = body
        .pointer("/data/body")
        .or_else(|| body.get("body"))
        .unwrap_or(&body);
    let Some(active) = jobs.get("active_runner_jobs").and_then(Value::as_array) else {
        status.endpoint_probe_error =
            Some("remote daemon typed job probe did not return active_runner_jobs".to_string());
        return;
    };
    let Some(stale) = jobs.get("stale_runner_jobs").and_then(Value::as_array) else {
        status.endpoint_probe_error =
            Some("remote daemon typed job probe did not return stale_runner_jobs".to_string());
        return;
    };
    status.work_evidence =
        RemoteDaemonWorkEvidence::from_unresolved_count(active.len().saturating_add(stale.len()));
}

fn remote_daemon_from_state(state: &Value) -> RemoteDaemon {
    RemoteDaemon {
        address: state
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: state
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        lease_id: state
            .get("lease_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        version: None,
        build_identity: None,
        inspected_freshness: None,
    }
}

pub(super) fn remote_daemon_active_jobs(data: &Value) -> usize {
    data.pointer("/freshness/active_jobs")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

fn remote_daemon_ensure_running(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
    replacement_operation_id: Option<&str>,
) -> std::result::Result<RemoteDaemon, RemoteDaemonEnsureError> {
    let command = remote_daemon_ensure_running_command(homeboy, replacement_operation_id);
    let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
    if !output.success {
        return Err(RemoteDaemonEnsureError::Other(command_failure_message(
            "remote daemon ensure-running failed",
            &output,
        )));
    }
    let envelope = parse_envelope(&output.stdout).map_err(|err| {
        format!(
            "remote daemon ensure-running returned invalid JSON: {}",
            err
        )
    })?;
    if !envelope.success {
        return Err(RemoteDaemonEnsureError::EnsureRunning(
            summarize_ensure_running_failure(
                runner_id,
                &command,
                envelope.error.as_ref().unwrap_or(&Value::Null),
                &output.stderr,
                None,
            ),
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| "remote daemon ensure-running returned no data".to_string())?;
    Ok(RemoteDaemon {
        address: data
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: data
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        lease_id: data
            .get("lease_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        version: None,
        build_identity: None,
        inspected_freshness: None,
    })
}

const ENSURE_RUNNING_FAILURE_SCHEMA_VERSION: u8 = 1;
pub(super) const MAX_CANDIDATE_EXEMPLAR_BYTES: usize = 256;
pub(super) const MAX_CANDIDATE_EXEMPLARS: usize = 3;
const MAX_BLOCKER_BYTES: usize = 256;
const MAX_NEXT_ACTION_BYTES: usize = 256;
// Includes every bounded field and the durable evidence URI, so truncation
// cannot remove the reference from a rendered failure envelope.
pub(super) const MAX_ENSURE_RUNNING_FAILURE_MESSAGE_BYTES: usize = 1400;

#[derive(Debug, Clone)]
pub(super) struct EnsureRunningFailure {
    pub(super) message: String,
    pub(super) evidence_ref: Option<RunnerConnectFailureEvidenceRef>,
}

enum RemoteDaemonEnsureError {
    Other(String),
    EnsureRunning(EnsureRunningFailure),
}

impl From<String> for RemoteDaemonEnsureError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

/// Keep control-plane output small while preserving the complete redacted
/// remote envelope in the configured durable artifact root.
pub(super) fn summarize_ensure_running_failure(
    runner_id: &str,
    command: &str,
    error: &Value,
    stderr: &str,
    store: Option<&homeboy_core::observation::ObservationStore>,
) -> EnsureRunningFailure {
    let policy = homeboy_core::redaction::RedactionPolicy::default();
    let error = policy.redact_json(error);
    let details = error.get("details").unwrap_or(&Value::Null);
    let classification = details
        .get("classification")
        .and_then(Value::as_str)
        .or_else(|| error.get("code").and_then(Value::as_str))
        .unwrap_or("remote_daemon_ensure_running_failure");
    let candidates = details
        .get("candidates")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let candidate_count = details
        .get("candidate_count")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(candidates.len());
    let blocker = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("remote daemon ensure-running returned an error");
    let next_action = details
        .get("safe_next_action")
        .and_then(Value::as_str)
        .or_else(|| error.get("hint").and_then(Value::as_str))
        .unwrap_or("Retry the exact runner connect command after inspecting daemon status.");
    let artifact_ref = store
        .map(|store| {
            persist_ensure_running_failure_evidence_in(store, runner_id, command, &error, stderr)
        })
        .unwrap_or_else(|| {
            persist_ensure_running_failure_evidence(runner_id, command, &error, stderr)
        })
        .ok();
    let candidates = bounded_candidate_exemplars(candidates);
    let evidence = artifact_ref
        .as_ref()
        .map(|reference| reference.uri.as_str())
        .unwrap_or("unavailable");
    let message = format!(
        "remote daemon ensure-running failed summary_v{} classification={} candidate_count={} blocker={} next_action={} candidates={} evidence_ref={}",
        ENSURE_RUNNING_FAILURE_SCHEMA_VERSION,
        truncate_utf8(classification, 96),
        candidate_count,
        truncate_utf8(blocker, MAX_BLOCKER_BYTES),
        truncate_utf8(next_action, MAX_NEXT_ACTION_BYTES),
        candidates, truncate_utf8(evidence, 256),
    );
    EnsureRunningFailure {
        message: truncate_utf8(&message, MAX_ENSURE_RUNNING_FAILURE_MESSAGE_BYTES),
        evidence_ref: artifact_ref,
    }
}

pub(super) fn bounded_candidate_exemplars(candidates: &[Value]) -> String {
    let mut seen = BTreeSet::new();
    let mut exemplars = Vec::new();
    let mut bytes = 0;
    // An ambiguous owner blocks safe recovery; show it before unrelated
    // candidates even when a remote daemon reports it last.
    let prioritized = candidates.iter().filter(|candidate| {
        candidate.get("ownership").and_then(Value::as_str) == Some("ambiguous")
    });
    let remaining = candidates.iter().filter(|candidate| {
        candidate.get("ownership").and_then(Value::as_str) != Some("ambiguous")
    });
    for candidate in prioritized.chain(remaining) {
        let rendered =
            serde_json::to_string(candidate).unwrap_or_else(|_| "<unserializable>".to_string());
        if !seen.insert(rendered.clone()) {
            continue;
        }
        let separator = usize::from(!exemplars.is_empty());
        if exemplars.len() == MAX_CANDIDATE_EXEMPLARS
            || bytes + separator >= MAX_CANDIDATE_EXEMPLAR_BYTES - 2
        {
            break;
        }
        let available = MAX_CANDIDATE_EXEMPLAR_BYTES - 2 - bytes - separator;
        let rendered = truncate_utf8(&rendered, available);
        bytes += separator + rendered.len();
        exemplars.push(rendered);
    }
    format!("[{}]", exemplars.join(","))
}

fn persist_ensure_running_failure_evidence(
    runner_id: &str,
    command: &str,
    error: &Value,
    stderr: &str,
) -> homeboy_core::Result<RunnerConnectFailureEvidenceRef> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    persist_ensure_running_failure_evidence_in(&store, runner_id, command, error, stderr)
}

pub(super) fn persist_ensure_running_failure_evidence_in(
    store: &homeboy_core::observation::ObservationStore,
    runner_id: &str,
    command: &str,
    error: &Value,
    stderr: &str,
) -> homeboy_core::Result<RunnerConnectFailureEvidenceRef> {
    let command = homeboy_core::redaction::redact_string(command);
    let stderr = homeboy_core::redaction::redact_string(stderr);
    let evidence = json!({
        "schema_version": ENSURE_RUNNING_FAILURE_SCHEMA_VERSION,
        "kind": "remote_daemon_ensure_running_failure",
        "runner_id": runner_id,
        "remote_command": command,
        "remote_envelope": error,
        "remote_stderr": stderr,
    });
    let file = tempfile::NamedTempFile::new().map_err(homeboy_core::Error::from)?;
    serde_json::to_writer(file.as_file(), &evidence)?;
    let run = store.start_run(
        homeboy_core::observation::NewRunRecord::builder("runner_connect_failure")
            .metadata(json!({
                "runner_id": runner_id,
                "controller_id": crate::connection::controller_id(),
                "failure_kind": "remote_daemon_ensure_running",
            }))
            .build(),
    )?;
    let artifact = match store.record_artifact_with_metadata(
        &run.id,
        "remote_daemon_ensure_running_failure",
        file.path(),
        json!({ "failure_diagnostic": true, "failure_diagnostic_rank": 1 }),
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            terminalize_or_rollback_runner_connect_failure(store, &run)?;
            return Err(error);
        }
    };
    if let Err(error) = store.finish_run(
        &run.id,
        homeboy_core::observation::RunStatus::Fail,
        Some(run.metadata_json.clone()),
    ) {
        rollback_runner_connect_failure(store, &run.id)?;
        return Err(error);
    }
    Ok(RunnerConnectFailureEvidenceRef {
        schema_version: ENSURE_RUNNING_FAILURE_SCHEMA_VERSION,
        run_id: run.id.clone(),
        artifact_id: artifact.id.clone(),
        uri: homeboy_artifact_ref_contract::artifact_uri(&run.id, &artifact.id),
    })
}

fn terminalize_or_rollback_runner_connect_failure(
    store: &homeboy_core::observation::ObservationStore,
    run: &homeboy_core::observation::RunRecord,
) -> homeboy_core::Result<()> {
    if store
        .finish_run(
            &run.id,
            homeboy_core::observation::RunStatus::Fail,
            Some(run.metadata_json.clone()),
        )
        .is_ok()
    {
        return Ok(());
    }
    rollback_runner_connect_failure(store, &run.id)
}

fn rollback_runner_connect_failure(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
) -> homeboy_core::Result<()> {
    rollback_runner_connect_failure_with(
        store,
        run_id,
        |store, run_id| store.discard_running_run(run_id),
        |path| std::fs::remove_file(path),
    )
}

pub(super) fn rollback_runner_connect_failure_with<Rollback, Delete>(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    rollback: Rollback,
    mut delete: Delete,
) -> homeboy_core::Result<()>
where
    Rollback:
        FnOnce(&homeboy_core::observation::ObservationStore, &str) -> homeboy_core::Result<bool>,
    Delete: FnMut(&Path) -> std::io::Result<()>,
{
    let artifacts = store.list_artifacts(run_id)?;
    if !rollback(store, run_id)? {
        return Err(homeboy_core::Error::internal_unexpected(format!(
            "runner connect failure observation {run_id} was retained; preserving its artifact bytes"
        )));
    }
    for artifact in artifacts {
        if artifact.artifact_type == "file" {
            // Move the now-unreferenced byte under the cleanup service's owned
            // staging name before attempting removal. A failed delete remains
            // discoverable by `orphaned-artifact-bytes` rather than leaking.
            let path = Path::new(&artifact.path);
            let staged_path =
                path.with_file_name(format!(".artifact-{}.staging", uuid::Uuid::new_v4()));
            std::fs::rename(path, &staged_path).map_err(|error| {
                homeboy_core::Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "stage rolled-back runner connect artifact {} for owned cleanup",
                        artifact.path
                    )),
                )
            })?;
            delete(&staged_path).map_err(|error| {
                homeboy_core::Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "remove rolled-back runner connect artifact {}; retained at {} for orphaned-artifact-bytes cleanup",
                        artifact.path,
                        staged_path.display(),
                    )),
                )
            })?;
        }
    }
    Ok(())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

pub(super) fn remote_daemon_ensure_running_command(
    homeboy: &str,
    replacement_operation_id: Option<&str>,
) -> String {
    format!(
        "{} daemon ensure-running {} --addr 127.0.0.1:0",
        shell::quote_arg(homeboy),
        replacement_operation_id
            .map(|id| format!("--replacement-operation-id {}", shell::quote_arg(id)))
            .unwrap_or_default(),
    )
}

pub(super) fn remote_daemon_force_stop(
    client: &SshClient,
    homeboy: &str,
    lease_id: &str,
) -> std::result::Result<(), String> {
    let command = format!(
        "{} daemon stop --lease-id {}",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
    );
    let output = client.execute_with_timeout(&command, REMOTE_DAEMON_STATUS_TIMEOUT);
    if !output.success {
        return Err(command_failure_message(
            "remote bounded stale-daemon replacement stop failed",
            &output,
        ));
    }
    let envelope = parse_envelope(&output.stdout).map_err(|error| {
        format!("remote bounded stale-daemon replacement stop returned invalid JSON: {error}")
    })?;
    if !envelope.success {
        return Err(
            "remote bounded stale-daemon replacement stop returned an error envelope".to_string(),
        );
    }
    if envelope
        .data
        .as_ref()
        .and_then(|data| data.get("action"))
        .and_then(Value::as_str)
        != Some("stop")
    {
        return Err(
            "remote bounded stale-daemon replacement stop returned an unexpected response"
                .to_string(),
        );
    }
    Ok(())
}

fn verify_remote_daemon_replacement(
    client: &SshClient,
    homeboy: &str,
    replacement: &RemoteDaemon,
    configured_identity: &str,
) -> std::result::Result<RemoteDaemon, String> {
    let mut status = remote_daemon_status(client, homeboy)?;
    probe_remote_daemon_endpoint(client, &mut status, None);
    let daemon = status.daemon.ok_or_else(|| {
        "remote stale-daemon replacement re-probe returned no daemon state".to_string()
    })?;
    if !status.fresh || !status.reachable {
        return Err(
            "remote stale-daemon replacement re-probe did not prove a fresh reachable daemon"
                .to_string(),
        );
    }
    if status.endpoint_probe_error.is_some() {
        return Err(format!(
            "remote stale-daemon replacement endpoint re-probe failed: {}",
            status.endpoint_probe_error.unwrap_or_default()
        ));
    }
    if daemon.lease_id != replacement.lease_id
        || daemon.pid != replacement.pid
        || daemon.address != replacement.address
    {
        return Err(
            "remote stale-daemon replacement ownership changed before re-probe; refusing to persist a different daemon"
                .to_string(),
        );
    }
    if daemon.build_identity.as_deref().map(str::trim) != Some(configured_identity.trim()) {
        return Err(format!(
            "remote stale-daemon replacement identity `{}` does not match configured runner binary `{}`",
            daemon.build_identity.as_deref().unwrap_or("unavailable"),
            configured_identity,
        ));
    }
    Ok(daemon)
}

fn remote_daemon_adopt_orphan(
    client: &SshClient,
    homeboy: &str,
    lease_id: &str,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
) -> std::result::Result<RemoteDaemon, String> {
    let command = remote_daemon_adopt_orphan_command(homeboy, lease_id, confirmed_no_pid_job_ids);
    let output = client.execute(&command);
    if !output.success {
        return Err(command_failure_message(
            "remote daemon orphan adoption failed",
            &output,
        ));
    }
    let envelope = parse_envelope(&output.stdout)
        .map_err(|err| format!("remote daemon orphan adoption returned invalid JSON: {err}"))?;
    if !envelope.success {
        return Err(format!(
            "remote daemon orphan adoption failed: {}",
            envelope.error.unwrap_or(Value::Null)
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| "remote daemon orphan adoption returned no data".to_string())?;
    let replacement = data
        .get("replacement")
        .ok_or_else(|| "remote daemon orphan adoption returned no replacement lease".to_string())?;
    Ok(RemoteDaemon {
        address: replacement
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: replacement
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        lease_id: replacement
            .get("lease_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        version: None,
        build_identity: None,
        inspected_freshness: None,
    })
}

pub(super) fn remote_daemon_adopt_orphan_command(
    homeboy: &str,
    lease_id: &str,
    confirmed_no_pid_job_ids: &[uuid::Uuid],
) -> String {
    let confirmations = confirmed_no_pid_job_ids
        .iter()
        .map(|job_id| format!(" --confirm-untracked-child-dead {job_id}"))
        .collect::<String>();
    format!(
        "{} daemon adopt-orphan --lease-id {} --confirm-pid-dead{} --addr 127.0.0.1:0",
        shell::quote_arg(homeboy),
        shell::quote_arg(lease_id),
        confirmations,
    )
}

pub(super) fn parse_envelope(stdout: &str) -> serde_json::Result<CliEnvelope> {
    parse_json_from_mixed_stdout(stdout)
}

pub(crate) fn parse_json_from_mixed_stdout<T>(stdout: &str) -> serde_json::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    match serde_json::from_str(stdout.trim()) {
        Ok(value) => Ok(value),
        Err(original) => {
            for (index, ch) in stdout.char_indices() {
                if ch != '{' {
                    continue;
                }
                let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
                if let Some(Ok(value)) = stream.next() {
                    return Ok(value);
                }
            }
            Err(original)
        }
    }
}

pub(super) fn parse_loopback_daemon_addr(address: &str) -> std::result::Result<SocketAddr, ()> {
    let addr: SocketAddr = address.parse().map_err(|_| ())?;
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err(())
    }
}

pub(super) fn reserve_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("reserve local tunnel port".to_string()),
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read local tunnel port".to_string()))
        })?
        .port();
    drop(listener);
    Ok(port)
}

pub(super) fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}
