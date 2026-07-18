use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::shell;
use crate::engine::shell::{quote_runner_env_value, remote_shell_path_preamble};
use crate::error::{Error, Result};

use super::super::session::ensure_control_path_parent;
use super::super::ssh_args::{client_ssh_args, SshArgOptions, SshPortFlag};
use super::super::{
    ManagedSshSession, ManagedSshSessionOutput, Server, ServerAuthMode, ServerSessionConfig,
};
use super::host::{is_local_host, is_transient_ssh_error};
use super::local_exec::{
    execute_local_command, execute_local_command_in_dir_with_timeout,
    execute_local_command_interactive, execute_local_command_with_stdin,
    execute_local_command_with_stdin_and_timeout,
};
use super::{CommandOutput, SshClient};

/// Sentinel terminating the secret-env block streamed over the SSH channel's
/// stdin. Chosen to never collide with an env var name or a `NAME=VALUE` line.
pub(crate) const SECRET_ENV_STDIN_SENTINEL: &str = "__HOMEBOY_SECRET_ENV_END__";

/// Where a command's stdin bytes come from when it is dispatched over SSH (or
/// the localhost fast path).
#[derive(Clone, Copy)]
enum SshStdin<'a> {
    /// No stdin payload — the remote command inherits an empty stdin.
    None,
    /// Stream a local file as the command's stdin (used by `upload_file`).
    File(&'a str),
    /// Stream in-memory bytes as the command's stdin (used to deliver the
    /// secret-env block without placing secrets in the command argv).
    Inline(&'a [u8]),
}

/// POSIX-sh prologue that imports a secret-env block streamed over stdin.
///
/// Each line before the sentinel is a literal `NAME=VALUE` pair. Names and
/// values arrive on the SSH channel's stdin, so neither the controller-local
/// `ssh` argv nor the remote login-shell argv (both visible in `ps`) ever carry
/// the secret. Values are applied with the `export` builtin — not `eval` — so
/// shell metacharacters in a value are inert (no command injection). A value
/// containing a literal newline is the one unsupported shape; OAuth/API tokens
/// never contain one.
fn secret_env_stdin_read_loop() -> String {
    format!(
        "while IFS= read -r __homeboy_env_line; do \
[ \"$__homeboy_env_line\" = \"{sentinel}\" ] && break; \
__homeboy_env_key=${{__homeboy_env_line%%=*}}; \
export \"$__homeboy_env_key=${{__homeboy_env_line#*=}}\"; \
done",
        sentinel = SECRET_ENV_STDIN_SENTINEL,
    )
}

/// Prepend the secret-env read loop to an already-composed command line so the
/// secrets are imported before the command runs.
pub(crate) fn wrap_command_with_secret_env_read_loop(command_line: &str) -> String {
    format!("{} && {}", secret_env_stdin_read_loop(), command_line)
}

/// Serialize the secret env map into the stdin block consumed by
/// [`secret_env_stdin_read_loop`]. Sorted for deterministic output.
pub(crate) fn build_secret_env_stdin_block(secret_env: &BTreeMap<String, String>) -> Vec<u8> {
    let mut block = String::new();
    for (key, value) in secret_env {
        block.push_str(key);
        block.push('=');
        block.push_str(value);
        block.push('\n');
    }
    block.push_str(SECRET_ENV_STDIN_SENTINEL);
    block.push('\n');
    block.into_bytes()
}

/// Map a finished `ssh` invocation's captured output into a [`CommandOutput`].
fn map_ssh_output(output: std::io::Result<std::process::Output>) -> CommandOutput {
    match output {
        Ok(out) => CommandOutput {
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            success: out.status.success(),
            exit_code: out.status.code().unwrap_or(-1),
            timed_out: false,
            child_resource: None,
        },
        Err(err) => CommandOutput {
            stdout: String::new(),
            stderr: format!("SSH error: {}", err),
            success: false,
            exit_code: -1,
            timed_out: false,
            child_resource: None,
        },
    }
}

impl SshClient {
    pub fn from_server(server: &Server, server_id: &str) -> Result<Self> {
        let identity_file = match &server.identity_file {
            Some(path) if !path.is_empty() => {
                let expanded = shellexpand::tilde(path).to_string();
                if !std::path::Path::new(&expanded).exists() {
                    return Err(Error::ssh_identity_file_not_found(
                        server_id.to_string(),
                        expanded,
                    ));
                }
                Some(expanded)
            }
            _ => None,
        };

        let is_local = is_local_host(&server.host);
        if is_local {
            log_status!(
                "ssh",
                "Server '{}' is localhost — using local execution",
                server_id
            );
        }

        let auth = match &server.auth {
            Some(auth) if auth.mode == ServerAuthMode::KeyPlusPasswordControlmaster => {
                Some(ManagedSshSession::from_auth(auth))
            }
            _ => None,
        };

        Ok(Self {
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            identity_file,
            auth,
            is_local,
            env: server.env.clone(),
        })
    }

    /// Apply bounded execution only to a sequence of short diagnostic probes.
    /// Ordinary runner commands retain their existing execution semantics.
    pub fn scoped_probe_limits(
        &self,
        per_probe: Duration,
        overall: Duration,
        progress_prefix: impl Into<String>,
    ) -> ProbeLimitsGuard {
        ACTIVE_PROBE_LIMITS.with(|limits| {
            limits.borrow_mut().push(ProbeLimits {
                per_probe,
                overall,
                started: Instant::now(),
                progress_prefix: progress_prefix.into(),
                timed_out: Arc::new(Mutex::new(Vec::new())),
            });
        });
        ProbeLimitsGuard {}
    }

    pub fn timed_out_probes(&self) -> Vec<BoundedProbeTimeout> {
        ACTIVE_PROBE_LIMITS.with(|limits| {
            limits
                .borrow()
                .last()
                .map(ProbeLimits::timed_out_probes)
                .unwrap_or_default()
        })
    }

    pub fn remaining_probe_budget(&self) -> Option<Duration> {
        ACTIVE_PROBE_LIMITS.with(|limits| {
            limits
                .borrow()
                .last()
                .map(|limits| limits.overall.saturating_sub(limits.started.elapsed()))
        })
    }

    pub(crate) fn build_ssh_args(&self, command: Option<&str>, interactive: bool) -> Vec<String> {
        client_ssh_args(
            self,
            SshArgOptions {
                interactive,
                batch_mode: true,
                connect_timeout: true,
                keepalive: true,
                port_flag: Some(SshPortFlag::Lowercase),
                command,
                ..SshArgOptions::default()
            },
        )
    }

    fn build_session_control_args(&self, operation: &str) -> Result<Vec<String>> {
        let session = self.auth.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "auth.mode",
                "Server is not configured for managed SSH sessions",
                None,
                Some(vec![
                    "Run: homeboy server set <server> --json '{\"auth\":{\"mode\":\"key_plus_password_controlmaster\"}}'".to_string(),
                    "Then run: homeboy server connect <server>".to_string(),
                ]),
            )
        })?;

        let mut args = Vec::new();

        if let Some(identity_file) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        args.extend([
            "-S".to_string(),
            session.control_path.clone(),
            "-O".to_string(),
            operation.to_string(),
            format!("{}@{}", self.user, self.host),
        ]);

        Ok(args)
    }

    pub(crate) fn build_session_connect_args(&self) -> Result<Vec<String>> {
        let session = self.auth.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "auth.mode",
                "Server is not configured for managed SSH sessions",
                None,
                Some(vec![
                    "Run: homeboy server set <server> --json '{\"auth\":{\"mode\":\"key_plus_password_controlmaster\"}}'".to_string(),
                    "Then run: homeboy server connect <server>".to_string(),
                ]),
            )
        })?;

        ensure_control_path_parent(&session.control_path)?;

        let mut args = Vec::new();

        if let Some(identity_file) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        args.extend([
            "-M".to_string(),
            "-N".to_string(),
            "-f".to_string(),
            "-o".to_string(),
            "ControlMaster=yes".to_string(),
            "-o".to_string(),
            format!("ControlPath={}", session.control_path),
            "-o".to_string(),
            format!("ControlPersist={}", session.persist),
            format!("{}@{}", self.user, self.host),
        ]);

        Ok(args)
    }

    pub fn connect_managed_session(&self) -> Result<ManagedSshSessionOutput> {
        if self.is_local {
            return Ok(self.local_managed_session_output(true));
        }

        let args = self.build_session_connect_args()?;
        let output = self.run_managed_session_command(args, true);
        if output.exit_code == 0 {
            return Ok(output);
        }

        Ok(self.with_per_command_connect_fallback(output, self.execute("true")))
    }

    pub(crate) fn with_per_command_connect_fallback(
        &self,
        mut output: ManagedSshSessionOutput,
        probe: CommandOutput,
    ) -> ManagedSshSessionOutput {
        if probe.success {
            output.live = false;
            output.exit_code = 0;
            if !output.stderr.is_empty() && !output.stderr.ends_with('\n') {
                output.stderr.push('\n');
            }
            output.stderr.push_str("Persistent SSH control-master setup failed, but per-command SSH succeeded. This server may close master sessions after authentication; Homeboy will continue without a live control socket.");
        }
        output
    }

    pub fn check_managed_session(&self) -> Result<ManagedSshSessionOutput> {
        if self.is_local {
            return Ok(self.local_managed_session_output(true));
        }

        let args = self.build_session_control_args("check")?;
        Ok(self.run_managed_session_command(args, true))
    }

    pub fn disconnect_managed_session(&self) -> Result<ManagedSshSessionOutput> {
        if self.is_local {
            return Ok(self.local_managed_session_output(false));
        }

        let args = self.build_session_control_args("exit")?;
        Ok(self.run_managed_session_command(args, false))
    }

    fn run_managed_session_command(
        &self,
        args: Vec<String>,
        expect_live_on_success: bool,
    ) -> ManagedSshSessionOutput {
        let output = Command::new("ssh").args(&args).output();
        let (stdout, stderr, exit_code) = match output {
            Ok(out) => (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            ),
            Err(err) => (String::new(), format!("SSH error: {}", err), -1),
        };
        let success = exit_code == 0;

        ManagedSshSessionOutput {
            session: self.output_session_config(),
            live: success && expect_live_on_success,
            stdout,
            stderr,
            exit_code,
        }
    }

    fn local_managed_session_output(&self, live: bool) -> ManagedSshSessionOutput {
        ManagedSshSessionOutput {
            session: self.output_session_config(),
            live,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    pub(crate) fn output_session_config(&self) -> ServerSessionConfig {
        ServerSessionConfig {
            control_path: self.auth.as_ref().map(|auth| auth.control_path.clone()),
            persist: self.auth.as_ref().map(|auth| auth.persist.clone()),
        }
    }

    pub fn execute(&self, command: &str) -> CommandOutput {
        if let Some(limits) = ACTIVE_PROBE_LIMITS.with(|limits| limits.borrow().last().cloned()) {
            return limits.execute(self, command);
        }
        let effective = self.prepend_env(command);
        self.execute_with_stdin(&effective, SshStdin::None)
    }

    /// Execute a short, read-only probe with a hard wall-clock deadline.
    ///
    /// Status/version checks must return partial diagnostics instead of allowing
    /// an unavailable remote command to block the entire dashboard.
    pub fn execute_with_timeout(&self, command: &str, timeout: Duration) -> CommandOutput {
        let effective = self.prepend_env(command);
        if self.is_local {
            return execute_local_command_in_dir_with_timeout(&effective, None, None, timeout);
        }
        self.execute_ssh_with_timeout(&effective, None, timeout)
    }

    /// Execute `command` with secret env vars delivered over stdin instead of
    /// interpolated into the SSH command argv.
    ///
    /// Non-secret env (PATH and other public config) is still exported inline by
    /// [`prepend_env`], preserving `$PATH`-style shell expansion. The secret
    /// values are streamed as a `NAME=VALUE` block (terminated by a sentinel)
    /// over the SSH channel's stdin and imported by a read loop, so OAuth/API
    /// tokens never appear in the controller-local `ssh` argv or the remote
    /// login-shell argv (both visible in `ps`). When `secret_env` is empty this
    /// is identical to [`execute`].
    pub fn execute_with_secret_env(
        &self,
        command: &str,
        secret_env: &BTreeMap<String, String>,
    ) -> CommandOutput {
        let effective = self.prepend_env(command);
        if secret_env.is_empty() {
            return self.execute_with_stdin(&effective, SshStdin::None);
        }
        let wrapped = wrap_command_with_secret_env_read_loop(&effective);
        let block = build_secret_env_stdin_block(secret_env);
        self.execute_with_stdin(&wrapped, SshStdin::Inline(&block))
    }

    /// Execute stdin-delivered secrets under a hard deadline. The stdin path
    /// preserves secret isolation while the SSH child process group is killed
    /// if the remote command fails to return in time.
    pub fn execute_with_secret_env_and_timeout(
        &self,
        command: &str,
        secret_env: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> CommandOutput {
        let effective = self.prepend_env(command);
        let (command, stdin) = if secret_env.is_empty() {
            (effective, None)
        } else {
            let stdin = build_secret_env_stdin_block(secret_env);
            (
                wrap_command_with_secret_env_read_loop(&effective),
                Some(stdin),
            )
        };
        if self.is_local {
            return match stdin {
                Some(stdin) => {
                    execute_local_command_with_stdin_and_timeout(&command, &stdin, timeout)
                }
                None => execute_local_command_in_dir_with_timeout(&command, None, None, timeout),
            };
        }
        self.execute_ssh_with_timeout(&command, stdin.as_deref(), timeout)
    }

    fn execute_ssh_with_timeout(
        &self,
        command: &str,
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> CommandOutput {
        let args = self.build_ssh_args(Some(command), false);
        let mut cmd = Command::new("ssh");
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        crate::server::process_cleanup::configure_process_group_cleanup(&mut cmd);
        execute_command_with_stdin_timeout(cmd, stdin, timeout)
    }
}

#[derive(Clone)]
pub(crate) struct ProbeLimits {
    per_probe: Duration,
    overall: Duration,
    started: Instant,
    progress_prefix: String,
    timed_out: Arc<Mutex<Vec<BoundedProbeTimeout>>>,
}

thread_local! {
    static ACTIVE_PROBE_LIMITS: RefCell<Vec<ProbeLimits>> = const { RefCell::new(Vec::new()) };
}

pub struct ProbeLimitsGuard {}

impl Drop for ProbeLimitsGuard {
    fn drop(&mut self) {
        ACTIVE_PROBE_LIMITS.with(|limits| {
            limits.borrow_mut().pop();
        });
    }
}

#[derive(Debug, Clone)]
pub struct BoundedProbeTimeout {
    pub command: String,
    pub reason_code: &'static str,
}

impl ProbeLimits {
    fn execute(&self, client: &SshClient, command: &str) -> CommandOutput {
        let elapsed = self.started.elapsed();
        let remaining = self.overall.saturating_sub(elapsed);
        let (timeout, reason_code) = if remaining.is_zero() {
            (Duration::ZERO, "runner_doctor.overall_timeout")
        } else if remaining < self.per_probe {
            (remaining, "runner_doctor.overall_timeout")
        } else {
            (self.per_probe, "runner_doctor.probe_timeout")
        };
        eprintln!("[{}] probing: {}", self.progress_prefix, command);
        if timeout.is_zero() {
            self.record_timeout(command, reason_code);
            return CommandOutput {
                stdout: String::new(),
                stderr: "Homeboy remote diagnostic overall deadline was exhausted before this probe started.".to_string(),
                success: false,
                exit_code: 124,
                timed_out: true,
                child_resource: None,
            };
        }
        let output = client.execute_with_timeout(command, timeout);
        if output.timed_out {
            self.record_timeout(command, reason_code);
        }
        output
    }

    fn timed_out_probes(&self) -> Vec<BoundedProbeTimeout> {
        self.timed_out
            .lock()
            .map(|probes| probes.clone())
            .unwrap_or_default()
    }

    fn record_timeout(&self, command: &str, reason_code: &'static str) {
        if let Ok(mut probes) = self.timed_out.lock() {
            probes.push(BoundedProbeTimeout {
                command: command.to_string(),
                reason_code,
            });
        }
    }
}

pub(super) fn execute_command_with_stdin_timeout(
    cmd: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> CommandOutput {
    execute_command_with_writer_factory(cmd, stdin, timeout, |pipe, bytes| {
        let (writer_tx, writer_rx) = std::sync::mpsc::channel();
        let writer = bytes.zip(pipe).map(|(bytes, mut pipe)| {
            let bytes = bytes.to_vec();
            thread::spawn(move || {
                let result = pipe.write_all(&bytes);
                let _ = writer_tx.send(result.map_err(|error| error.kind()));
            })
        });
        (writer_rx, writer)
    })
}

#[cfg(test)]
mod bounded_probe_tests {
    use super::*;

    fn localhost_client() -> SshClient {
        SshClient::from_server(
            &Server {
                id: "local".to_string(),
                aliases: Vec::new(),
                host: "localhost".to_string(),
                user: "tester".to_string(),
                port: 22,
                identity_file: None,
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            },
            "local",
        )
        .expect("localhost client")
    }

    #[test]
    fn bounded_probes_preserve_healthy_output_and_record_stalled_command() {
        let _limits = localhost_client().scoped_probe_limits(
            Duration::from_millis(50),
            Duration::from_secs(1),
            "test doctor",
        );
        let client = localhost_client();

        let healthy = client.execute("printf healthy");
        let stalled = client.execute("sleep 1");

        assert!(healthy.success);
        assert_eq!(healthy.stdout, "healthy");
        assert!(stalled.timed_out);
        let timed_out = client.timed_out_probes();
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].command, "sleep 1");
        assert_eq!(timed_out[0].reason_code, "runner_doctor.probe_timeout");
    }

    #[test]
    fn timed_out_local_probe_terminates_descendants() {
        let marker =
            std::env::temp_dir().join(format!("homeboy-probe-leak-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let client = localhost_client();
        let _limits = client.scoped_probe_limits(
            Duration::from_millis(50),
            Duration::from_secs(1),
            "test doctor",
        );

        let output = client.execute(&format!(
            "(sleep 1; touch {}) & wait",
            crate::engine::shell::quote_path(&marker.to_string_lossy())
        ));

        assert!(output.timed_out);
        thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "timed-out probe child leaked");
    }

    #[test]
    fn unreachable_ssh_probe_returns_a_result_within_its_budget() {
        let mut client = localhost_client();
        client.is_local = false;
        client.port = 1;
        let _limits = client.scoped_probe_limits(
            Duration::from_millis(100),
            Duration::from_millis(200),
            "test doctor",
        );

        let output = client.execute("printf unreachable");

        assert!(!output.success);
    }
}

pub(super) fn execute_command_with_writer_factory<Factory>(
    mut cmd: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
    writer_factory: Factory,
) -> CommandOutput
where
    Factory: FnOnce(
        Option<std::process::ChildStdin>,
        Option<&[u8]>,
    ) -> (
        std::sync::mpsc::Receiver<std::result::Result<(), std::io::ErrorKind>>,
        Option<thread::JoinHandle<()>>,
    ),
{
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("SSH error: {err}"),
                success: false,
                exit_code: -1,
                timed_out: false,
                child_resource: None,
            }
        }
    };
    let pid = child.id();
    let (writer_rx, writer) = writer_factory(child.stdin.take(), stdin);
    let stdout = child.stdout.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).to_string()
        })
    });
    let stderr = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).to_string()
        })
    });
    let mut timed_out = false;
    let mut stdin_failed = false;
    let mut status = None;
    loop {
        if matches!(writer_rx.try_recv(), Ok(Err(_))) {
            stdin_failed = true;
            status = terminate_process_group_with_deadline(&mut child, pid);
            break;
        }
        match child.try_wait() {
            Ok(Some(observed)) => {
                status = Some(observed);
                break;
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                timed_out = true;
                status = terminate_process_group_with_deadline(&mut child, pid);
                break;
            }
            Err(_) => break,
        }
    }
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let mut stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "Homeboy remote probe timed out after {}ms; terminated child process group.",
            timeout.as_millis()
        ));
    }
    if stdin_failed {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str("Homeboy SSH stdin delivery failed before command completion.");
    }
    CommandOutput {
        stdout,
        stderr,
        success: !timed_out && !stdin_failed && status.is_some_and(|status| status.success()),
        exit_code: if timed_out {
            124
        } else {
            status.and_then(|status| status.code()).unwrap_or(-1)
        },
        timed_out,
        child_resource: None,
    }
}

const PROCESS_TERMINATION_GRACE: Duration = Duration::from_millis(100);
const PROCESS_REAP_DEADLINE: Duration = Duration::from_millis(250);

fn terminate_process_group_with_deadline(
    child: &mut std::process::Child,
    pid: u32,
) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    if let Some(status) = poll_child_until(child, PROCESS_TERMINATION_GRACE) {
        return Some(status);
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    poll_child_until(child, PROCESS_REAP_DEADLINE)
}

fn poll_child_until(
    child: &mut std::process::Child,
    deadline: Duration,
) -> Option<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if started.elapsed() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => return None,
        }
    }
}

impl SshClient {
    /// Build an env preamble that normalizes runner command lookup and sets
    /// configured variables via `export`. PATH values allow shell expansion
    /// so configs can append/prepend `$PATH`.
    fn prepend_env(&self, command: &str) -> String {
        let mut exports = vec![remote_shell_path_preamble().to_string()];
        exports.extend(
            self.env
                .iter()
                .map(|(k, v)| format!("export {}={}", k, quote_runner_env_value(k, v))),
        );
        format!("{} && {}", exports.join(" && "), command)
    }

    pub fn upload_file(&self, local_path: &str, remote_path: &str) -> CommandOutput {
        let remote_command = format!("cat > {}", shell::quote_path(remote_path));
        self.execute_with_stdin(&remote_command, SshStdin::File(local_path))
    }

    pub fn download_file(&self, remote_path: &str, local_path: &str) -> CommandOutput {
        if self.is_local {
            return match std::fs::copy(remote_path, local_path) {
                Ok(_) => CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                    exit_code: 0,
                    timed_out: false,
                    child_resource: None,
                },
                Err(err) => CommandOutput {
                    stdout: String::new(),
                    stderr: format!(
                        "failed to copy local file '{}' to '{}': {}",
                        remote_path, local_path, err
                    ),
                    success: false,
                    exit_code: -1,
                    timed_out: false,
                    child_resource: None,
                },
            };
        }

        let local_file = match std::fs::File::create(local_path) {
            Ok(file) => file,
            Err(err) => {
                return CommandOutput {
                    stdout: String::new(),
                    stderr: format!("failed to create download target '{}': {}", local_path, err),
                    success: false,
                    exit_code: -1,
                    timed_out: false,
                    child_resource: None,
                };
            }
        };
        let remote_command = self.prepend_env(&format!("cat {}", shell::quote_path(remote_path)));
        let args = self.build_ssh_args(Some(&remote_command), false);
        let output = Command::new("ssh")
            .args(&args)
            .stdout(Stdio::from(local_file))
            .stderr(Stdio::piped())
            .output();
        match output {
            Ok(out) => CommandOutput {
                stdout: String::new(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                success: out.status.success(),
                exit_code: out.status.code().unwrap_or(-1),
                timed_out: false,
                child_resource: None,
            },
            Err(err) => CommandOutput {
                stdout: String::new(),
                stderr: format!("SSH download error: {}", err),
                success: false,
                exit_code: -1,
                timed_out: false,
                child_resource: None,
            },
        }
    }

    fn execute_with_stdin(&self, command: &str, stdin: SshStdin<'_>) -> CommandOutput {
        self.execute_with_retry(command, stdin, 3)
    }

    fn execute_with_retry(
        &self,
        command: &str,
        stdin: SshStdin<'_>,
        max_attempts: u32,
    ) -> CommandOutput {
        let backoff_secs = [0, 2, 5]; // delays before retry 1, 2, 3

        for attempt in 0..max_attempts {
            let result = self.execute_once(command, stdin);

            // Only retry on transient connection errors, not command failures
            if result.success || attempt + 1 >= max_attempts || !is_transient_ssh_error(&result) {
                return result;
            }

            let delay = backoff_secs.get(attempt as usize + 1).copied().unwrap_or(5);
            log_status!(
                "ssh",
                "Connection failed (attempt {}/{}), retrying in {}s...",
                attempt + 1,
                max_attempts,
                delay
            );
            std::thread::sleep(std::time::Duration::from_secs(delay));
        }

        // Unreachable, but satisfy the compiler
        CommandOutput {
            stdout: String::new(),
            stderr: "SSH retry exhausted".to_string(),
            success: false,
            exit_code: -1,
            timed_out: false,
            child_resource: None,
        }
    }

    fn execute_once(&self, command: &str, stdin: SshStdin<'_>) -> CommandOutput {
        // Local execution: run command directly instead of over SSH
        if self.is_local {
            return match stdin {
                SshStdin::File(stdin_file_path) => {
                    // For stdin piping (used by upload_file), use shell redirection
                    let local_cmd =
                        format!("cat {} | {}", shell::quote_path(stdin_file_path), command);
                    execute_local_command(&local_cmd)
                }
                SshStdin::Inline(bytes) => execute_local_command_with_stdin(command, bytes),
                SshStdin::None => execute_local_command(command),
            };
        }

        let args = self.build_ssh_args(Some(command), false);

        let mut cmd = Command::new("ssh");
        cmd.args(&args);

        match stdin {
            SshStdin::File(stdin_file_path) => match std::fs::File::open(stdin_file_path) {
                Ok(file) => {
                    cmd.stdin(file);
                    map_ssh_output(cmd.output())
                }
                Err(err) => CommandOutput {
                    stdout: String::new(),
                    stderr: format!("Failed to open stdin file: {}", err),
                    success: false,
                    exit_code: -1,
                    timed_out: false,
                    child_resource: None,
                },
            },
            SshStdin::Inline(bytes) => self.run_ssh_with_inline_stdin(cmd, bytes),
            SshStdin::None => map_ssh_output(cmd.output()),
        }
    }

    /// Spawn `ssh`, stream `stdin` bytes to the channel, and collect output.
    ///
    /// Used for the secret-env block: the bytes carry `NAME=VALUE` secret pairs
    /// the remote read loop imports, so they stay off the `ssh` argv. The block
    /// is small (a handful of tokens) and fits the OS pipe buffer, so writing it
    /// before `wait_with_output` never deadlocks against captured stdout/stderr.
    fn run_ssh_with_inline_stdin(&self, mut cmd: Command, stdin: &[u8]) -> CommandOutput {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                return CommandOutput {
                    stdout: String::new(),
                    stderr: format!("SSH error: {}", err),
                    success: false,
                    exit_code: -1,
                    timed_out: false,
                    child_resource: None,
                };
            }
        };
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(stdin);
            // Drop closes stdin (EOF) so the remote read loop terminates.
        }
        map_ssh_output(child.wait_with_output())
    }

    pub fn execute_interactive(&self, command: Option<&str>) -> i32 {
        let effective = command.map(|c| self.prepend_env(c));
        let effective_ref = effective.as_deref();

        // Local execution: run command directly instead of opening SSH session
        if self.is_local {
            return match effective_ref {
                Some(cmd) => execute_local_command_interactive(cmd, None, None),
                None => {
                    // Interactive shell on localhost — just open a shell
                    execute_local_command_interactive("bash", None, None)
                }
            };
        }

        let args = self.build_ssh_args(effective_ref, true);

        let status = Command::new("ssh")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();

        match status {
            Ok(s) => s.code().unwrap_or(-1),
            Err(_) => -1,
        }
    }
}
