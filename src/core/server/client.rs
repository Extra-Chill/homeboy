use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crate::core::engine::invocation;
use crate::core::engine::resource::{ChildProcessIdentity, ExtensionChildResourceSummary};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::runner::{quote_runner_env_value, remote_shell_path_preamble};
use chrono::Utc;

use super::{
    session::ensure_control_path_parent, ManagedSshSession, ManagedSshSessionOutput, Server,
    ServerAuthMode, ServerSessionConfig,
};
use std::process::{Command, Stdio};

#[cfg(unix)]
static ACTIVE_CLEANUP_PGID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
static CLEANUP_SIGNALS_INSTALLED: std::sync::Once = std::sync::Once::new();

pub struct SshClient {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub auth: Option<ManagedSshSession>,
    /// When true, all commands run locally instead of over SSH.
    /// Set automatically when the server host is localhost/127.0.0.1/::1.
    pub is_local: bool,
    /// Environment variables to inject before remote commands.
    /// Values are passed through the shell, so `$PATH`-style expansion works.
    pub env: HashMap<String, String>,
}

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: i32,
    pub child_resource: Option<ExtensionChildResourceSummary>,
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

    fn build_ssh_args(&self, command: Option<&str>, interactive: bool) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(identity_file) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        if let Some(session) = &self.auth {
            args.extend([
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                format!("ControlPath={}", session.control_path),
                "-o".to_string(),
                format!("ControlPersist={}", session.persist),
            ]);
        }

        // For non-interactive commands, add timeout and keepalive options
        // to prevent hangs on stalled connections or unexpected prompts.
        if !interactive {
            args.extend([
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "-o".to_string(),
                "ServerAliveInterval=15".to_string(),
                "-o".to_string(),
                "ServerAliveCountMax=3".to_string(),
            ]);
        }

        args.push(format!("{}@{}", self.user, self.host));

        if let Some(cmd) = command {
            args.push(cmd.to_string());
        }

        args
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

    fn build_session_connect_args(&self) -> Result<Vec<String>> {
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

    fn with_per_command_connect_fallback(
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

    fn output_session_config(&self) -> ServerSessionConfig {
        ServerSessionConfig {
            control_path: self.auth.as_ref().map(|auth| auth.control_path.clone()),
            persist: self.auth.as_ref().map(|auth| auth.persist.clone()),
        }
    }

    pub fn execute(&self, command: &str) -> CommandOutput {
        let effective = self.prepend_env(command);
        self.execute_with_stdin(&effective, None)
    }

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
        self.execute_with_stdin(&remote_command, Some(local_path))
    }

    fn execute_with_stdin(&self, command: &str, stdin_file: Option<&str>) -> CommandOutput {
        self.execute_with_retry(command, stdin_file, 3)
    }

    fn execute_with_retry(
        &self,
        command: &str,
        stdin_file: Option<&str>,
        max_attempts: u32,
    ) -> CommandOutput {
        let backoff_secs = [0, 2, 5]; // delays before retry 1, 2, 3

        for attempt in 0..max_attempts {
            let result = self.execute_once(command, stdin_file);

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
            child_resource: None,
        }
    }

    fn execute_once(&self, command: &str, stdin_file: Option<&str>) -> CommandOutput {
        // Local execution: run command directly instead of over SSH
        if self.is_local {
            if let Some(stdin_file_path) = stdin_file {
                // For stdin piping (used by upload_file), use shell redirection
                let local_cmd = format!("cat {} | {}", shell::quote_path(stdin_file_path), command);
                return execute_local_command(&local_cmd);
            }
            return execute_local_command(command);
        }

        let args = self.build_ssh_args(Some(command), false);

        let mut cmd = Command::new("ssh");
        cmd.args(&args);

        if let Some(stdin_file_path) = stdin_file {
            match std::fs::File::open(stdin_file_path) {
                Ok(file) => {
                    cmd.stdin(file);
                }
                Err(err) => {
                    return CommandOutput {
                        stdout: String::new(),
                        stderr: format!("Failed to open stdin file: {}", err),
                        success: false,
                        exit_code: -1,
                        child_resource: None,
                    };
                }
            }
        }

        let output = cmd.output();

        match output {
            Ok(out) => CommandOutput {
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                success: out.status.success(),
                exit_code: out.status.code().unwrap_or(-1),
                child_resource: None,
            },
            Err(e) => CommandOutput {
                stdout: String::new(),
                stderr: format!("SSH error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            },
        }
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

pub fn execute_local_command(command: &str) -> CommandOutput {
    execute_local_command_in_dir(command, None, None)
}

/// Run a local command, capturing stdout/stderr.
///
/// All locally-spawned commands run in their own process group with guaranteed
/// descendant teardown on exit, panic, or signal. Verbs that genuinely need a
/// background process use `std::process::Command` directly and manage the pid.
pub fn execute_local_command_in_dir(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    execute_local_command_in_dir_impl(command, current_dir, env)
}

fn execute_local_command_in_dir_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    configure_process_group_cleanup(&mut cmd);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let cleanup_guard = ProcessGroupCleanupGuard::new(child.id());
    let _invocation_child_guard =
        invocation_child_guard(env, child.id(), cleanup_guard.pgid(), command);
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    let output = match child.wait_with_output() {
        Ok(out) => CommandOutput {
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            success: out.status.success(),
            exit_code: out.status.code().unwrap_or(-1),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout: String::new(),
            stderr: format!("Command error: {}", e),
            success: false,
            exit_code: -1,
            child_resource: Some(monitor.finish()),
        },
    };
    cleanup_guard.cleanup();
    output
}

pub fn execute_local_command_interactive(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> i32 {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

/// Execute local command with stdout/stderr tee'd to terminal *and* captured.
///
/// Originally this function just inherited stdout/stderr and returned empty
/// strings — which meant callers like the test runner had no way to surface
/// PHPUnit output when tests failed (#1143). We now pipe both streams, copy
/// each chunk to the parent's stdout/stderr as it arrives (so the user still
/// sees live progress), and retain the full text in `CommandOutput` for
/// downstream processing.
pub fn execute_local_command_passthrough(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    execute_local_command_passthrough_impl(command, current_dir, env)
}

fn execute_local_command_passthrough_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    use std::io::{Read, Write};
    use std::thread;

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let cleanup_guard = ProcessGroupCleanupGuard::new(child.id());
    let _invocation_child_guard =
        invocation_child_guard(env, child.id(), cleanup_guard.pgid(), command);
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    // Tee each stream: copy every chunk to the parent's stdout/stderr as it
    // arrives (preserving the live-progress UX) while buffering it for the
    // caller. Using 4 KiB reads keeps latency low without excess syscalls.
    fn tee_to<R, W>(mut src: R, mut sink: W) -> String
    where
        R: Read,
        W: Write,
    {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = sink.write_all(&buf[..n]);
                    let _ = sink.flush();
                    captured.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || tee_to(pipe, std::io::stdout())));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || tee_to(pipe, std::io::stderr())));

    let status = child.wait();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr,
            success: status.success(),
            exit_code: status.code().unwrap_or(-1),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: format!("{stderr}\nCommand error: {}", e),
            success: false,
            exit_code: -1,
            child_resource: Some(monitor.finish()),
        },
    };
    cleanup_guard.cleanup();
    output
}

pub(crate) fn execute_local_command_stderr_passthrough(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    use std::io::{Read, Write};
    use std::thread;

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let cleanup_guard = ProcessGroupCleanupGuard::new(child.id());
    let _invocation_child_guard =
        invocation_child_guard(env, child.id(), cleanup_guard.pgid(), command);
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    fn read_all<R: Read>(mut src: R) -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => captured.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    fn tee_to_stderr<R: Read>(mut src: R) -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        let mut sink = std::io::stderr();
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = sink.write_all(&buf[..n]);
                    let _ = sink.flush();
                    captured.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_all(pipe)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || tee_to_stderr(pipe)));

    let status = child.wait();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr,
            success: status.success(),
            exit_code: status.code().unwrap_or(-1),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: format!("{stderr}\nCommand error: {}", e),
            success: false,
            exit_code: -1,
            child_resource: Some(monitor.finish()),
        },
    };
    cleanup_guard.cleanup();
    output
}

#[cfg(unix)]
fn configure_process_group_cleanup(cmd: &mut Command) {
    install_process_cleanup_signal_handlers();
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group_cleanup(_cmd: &mut Command) {}

struct ProcessGroupCleanupGuard {
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
}

impl ProcessGroupCleanupGuard {
    fn new(root_pid: u32) -> Self {
        #[cfg(unix)]
        {
            let pgid = Some(root_pid as libc::pid_t);
            if let Some(pgid) = pgid {
                ACTIVE_CLEANUP_PGID.store(pgid, Ordering::SeqCst);
            }
            Self { pgid }
        }

        #[cfg(not(unix))]
        {
            let _ = root_pid;
            Self {}
        }
    }

    fn cleanup(mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            cleanup_process_group(pgid);
            ACTIVE_CLEANUP_PGID
                .compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
            self.pgid = None;
        }
    }

    #[cfg(unix)]
    fn pgid(&self) -> Option<i32> {
        self.pgid.map(|pgid| pgid)
    }

    #[cfg(not(unix))]
    fn pgid(&self) -> Option<i32> {
        None
    }
}

fn invocation_child_guard(
    env: Option<&[(&str, &str)]>,
    root_pid: u32,
    pgid: Option<i32>,
    command_label: &str,
) -> Option<invocation::InvocationChildGuard> {
    let invocation_id = env.and_then(|pairs| {
        pairs
            .iter()
            .find_map(|(key, value)| (*key == "HOMEBOY_INVOCATION_ID").then_some(*value))
    })?;

    invocation::register_child_process(invocation_id, root_pid, pgid, command_label.to_string())
        .ok()
}

impl Drop for ProcessGroupCleanupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            cleanup_process_group(pgid);
            ACTIVE_CLEANUP_PGID
                .compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
        }
    }
}

#[cfg(unix)]
fn install_process_cleanup_signal_handlers() {
    CLEANUP_SIGNALS_INSTALLED.call_once(|| unsafe {
        libc::signal(
            libc::SIGINT,
            cleanup_signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            cleanup_signal_handler as *const () as libc::sighandler_t,
        );
    });
}

#[cfg(unix)]
extern "C" fn cleanup_signal_handler(signal: libc::c_int) {
    let pgid = ACTIVE_CLEANUP_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
    }
    unsafe {
        libc::_exit(128 + signal);
    }
}

#[cfg(unix)]
fn cleanup_process_group(pgid: libc::pid_t) {
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(200));
    if crate::core::process::process_group_is_running(pgid) {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

struct ChildResourceMonitor {
    child: ChildProcessIdentity,
    started_at: String,
    started_instant: Instant,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<ChildProbeState>>,
}

#[derive(Default)]
struct ChildProbeState {
    peak_rss_bytes: Option<u64>,
    peak_cpu_percent: Option<f64>,
    warnings: Vec<String>,
}

impl ChildResourceMonitor {
    fn start(root_pid: u32, command_label: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle =
            std::thread::spawn(move || sample_child_until_stopped(root_pid, stop_for_thread));

        Self {
            child: ChildProcessIdentity {
                root_pid,
                command_label,
            },
            started_at: Utc::now().to_rfc3339(),
            started_instant: Instant::now(),
            stop,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> ExtensionChildResourceSummary {
        self.stop.store(true, Ordering::Relaxed);
        let mut state = self
            .handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        state.warnings.sort();
        state.warnings.dedup();

        ExtensionChildResourceSummary {
            child: self.child,
            started_at: self.started_at,
            finished_at: Utc::now().to_rfc3339(),
            duration_ms: self.started_instant.elapsed().as_millis(),
            sampled_peak_rss_bytes: state.peak_rss_bytes,
            sampled_peak_cpu_percent: state.peak_cpu_percent,
            warnings: state.warnings,
        }
    }
}

fn sample_child_until_stopped(root_pid: u32, stop: Arc<AtomicBool>) -> ChildProbeState {
    let mut state = ChildProbeState::default();

    loop {
        match probe_child_resources(root_pid) {
            Ok(Some((rss_bytes, cpu_percent))) => {
                state.peak_rss_bytes = Some(
                    state
                        .peak_rss_bytes
                        .map_or(rss_bytes, |peak| peak.max(rss_bytes)),
                );
                state.peak_cpu_percent = Some(
                    state
                        .peak_cpu_percent
                        .map_or(cpu_percent, |peak| peak.max(cpu_percent)),
                );
            }
            Ok(None) => {}
            Err(warning) => state.warnings.push(warning),
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    state
}

fn probe_child_resources(root_pid: u32) -> std::result::Result<Option<(u64, f64)>, String> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-o", "%cpu=", "-p", &root_pid.to_string()])
        .output()
        .map_err(|_| "extension_child_probe_unsupported".to_string())?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let Some(rss_kb) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return Err("extension_child_rss_probe_unsupported".to_string());
    };
    let Some(cpu_percent) = parts.next().and_then(|value| value.parse::<f64>().ok()) else {
        return Err("extension_child_cpu_probe_unsupported".to_string());
    };

    Ok(Some((rss_kb.saturating_mul(1024), cpu_percent)))
}

/// Check if a host address refers to the local machine.
///
/// Matches localhost aliases (localhost, 127.0.0.1, ::1) and also checks
/// whether the host matches any IP address assigned to this machine's
/// network interfaces. This handles the case where a server config uses
/// the machine's public IP (e.g. a Hetzner VPS IP) — the agent running
/// on that same machine should deploy locally instead of SSH-ing to itself.
pub fn is_local_host(host: &str) -> bool {
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }

    // Check if host matches any local network interface address.
    // Parse the host as an IP first; if it's a hostname we skip this check
    // (DNS resolution would be slow and unreliable).
    let target_ip: std::net::IpAddr = match host.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };

    match get_local_ips() {
        Some(ips) => ips.contains(&target_ip),
        None => false,
    }
}

fn get_local_ips() -> Option<Vec<std::net::IpAddr>> {
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ip")
            .args(["-o", "addr", "show"])
            .output()
            .ok()?;
        let stdout = successful_command_stdout(output)?;
        let ips: Vec<std::net::IpAddr> = stdout
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 4 {
                    return None;
                }
                let addr_prefix = parts[3];
                let addr_str = addr_prefix.split('/').next()?;
                addr_str.parse().ok()
            })
            .collect();

        Some(ips)
    }

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ifconfig").output().ok()?;
        let stdout = successful_command_stdout(output)?;
        let ips: Vec<std::net::IpAddr> = stdout
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("inet ") {
                    rest.split_whitespace().next()?.parse().ok()
                } else if let Some(rest) = line.strip_prefix("inet6 ") {
                    let addr_str = rest.split_whitespace().next()?;
                    let addr_str = addr_str.split('%').next()?;
                    addr_str.parse().ok()
                } else {
                    None
                }
            })
            .collect();

        Some(ips)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn successful_command_stdout(output: std::process::Output) -> Option<String> {
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if an SSH failure is a transient connection error worth retrying.
fn is_transient_ssh_error(output: &CommandOutput) -> bool {
    let stderr = output.stderr.to_lowercase();
    // SSH exit code 255 = connection error (not a remote command failure)
    let is_connection_exit = output.exit_code == 255;

    let transient_patterns = [
        "connection refused",
        "connection reset",
        "connection timed out",
        "no route to host",
        "network is unreachable",
        "temporary failure in name resolution",
        "could not resolve hostname",
        "broken pipe",
        "ssh_exchange_identification",
        "connection closed by remote host",
    ];

    is_connection_exit || transient_patterns.iter().any(|p| stderr.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_local_hosts() {
        assert!(!is_local_host("example.com"));
        assert!(!is_local_host("192.168.1.1")); // private but not this machine (unless it is)
        assert!(!is_local_host("8.8.8.8"));
    }

    #[test]
    fn test_own_ip_detected_as_local() {
        // Get this machine's IPs and verify they're detected as local
        if let Some(ips) = get_local_ips() {
            for ip in &ips {
                let ip_str = ip.to_string();
                assert!(
                    is_local_host(&ip_str),
                    "Machine's own IP {} should be detected as local",
                    ip_str
                );
            }
        }
    }

    #[test]
    fn test_execute_local_command_in_dir() {
        let output = execute_local_command_in_dir("sleep 0.2", None, None);

        assert!(output.success);
        let child = output.child_resource.expect("child resource summary");
        assert!(child.child.root_pid > 0);
        assert_eq!(child.child.command_label, "sleep 0.2");
        assert!(child.duration_ms > 0);
        assert!(
            child.sampled_peak_rss_bytes.is_some() || !child.warnings.is_empty(),
            "resource probes should either sample RSS or explain why they could not"
        );
    }

    #[test]
    fn test_upload_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target with spaces.txt");
        std::fs::write(&source, "uploaded through stdin\n").expect("write source");
        let client = SshClient {
            host: "localhost".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        };

        let output = client.upload_file(&source.to_string_lossy(), &target.to_string_lossy());

        assert!(output.success, "upload failed: {}", output.stderr);
        assert_eq!(
            std::fs::read_to_string(target).expect("read target"),
            "uploaded through stdin\n"
        );
    }

    #[test]
    fn managed_session_config_adds_controlmaster_args() {
        let server = Server {
            id: "bastion".to_string(),
            aliases: Vec::new(),
            host: "bastion.example.test".to_string(),
            user: "deploy".to_string(),
            port: 2222,
            identity_file: None,
            kind: Some("password-gated".to_string()),
            auth: Some(super::super::ServerAuth {
                mode: ServerAuthMode::KeyPlusPasswordControlmaster,
                session: ServerSessionConfig {
                    control_path: Some("/tmp/homeboy-test-%h-%p-%r".to_string()),
                    persist: Some("4h".to_string()),
                },
            }),
            env: HashMap::new(),
            runner: None,
        };

        let client = SshClient::from_server(&server, "bastion").expect("client");
        let args = client.build_ssh_args(Some("uptime"), false);

        assert!(args.contains(&"ControlMaster=auto".to_string()));
        assert!(args.contains(&"ControlPath=/tmp/homeboy-test-%h-%p-%r".to_string()));
        assert!(args.contains(&"ControlPersist=4h".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"2222".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("uptime"));
    }

    #[test]
    fn managed_session_connect_builds_master_command() {
        let client = SshClient {
            host: "bastion.example.test".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: Some("/tmp/key".to_string()),
            auth: Some(ManagedSshSession {
                control_path: "/tmp/homeboy-test-control".to_string(),
                persist: "10m".to_string(),
            }),
            is_local: false,
            env: HashMap::new(),
        };

        let args = client.build_session_connect_args().expect("args");

        assert!(args.contains(&"-M".to_string()));
        assert!(args.contains(&"-N".to_string()));
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"ControlMaster=yes".to_string()));
        assert!(args.contains(&"ControlPath=/tmp/homeboy-test-control".to_string()));
        assert!(args.contains(&"ControlPersist=10m".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("deploy@bastion.example.test")
        );
    }

    #[test]
    fn test_from_server() {
        let server = Server {
            id: "local".to_string(),
            aliases: Vec::new(),
            host: "localhost".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: Some("local".to_string()),
            auth: Some(super::super::ServerAuth {
                mode: ServerAuthMode::KeyPlusPasswordControlmaster,
                session: ServerSessionConfig {
                    control_path: Some("/tmp/homeboy-local-%h-%p-%r".to_string()),
                    persist: Some("5m".to_string()),
                },
            }),
            env: HashMap::new(),
            runner: None,
        };

        let client = SshClient::from_server(&server, "local").expect("client");

        assert!(client.is_local);
        assert_eq!(client.host, "localhost");
        assert_eq!(client.user, "tester");
        assert_eq!(
            client.auth.as_ref().map(|auth| auth.persist.as_str()),
            Some("5m")
        );
    }

    #[test]
    fn test_connect_managed_session() {
        let client = local_managed_session_client();

        let output = client.connect_managed_session().expect("connect");

        assert!(output.live);
        assert_eq!(output.session.persist.as_deref(), Some("10m"));
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn test_check_managed_session() {
        let client = local_managed_session_client();

        let output = client.check_managed_session().expect("check");

        assert!(output.live);
        assert_eq!(
            output.session.control_path.as_deref(),
            Some("/tmp/homeboy-local-control")
        );
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn test_disconnect_managed_session() {
        let client = local_managed_session_client();

        let output = client.disconnect_managed_session().expect("disconnect");

        assert!(!output.live);
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn managed_session_connect_reports_per_command_fallback() {
        let client = local_managed_session_client();
        let output = client.with_per_command_connect_fallback(
            ManagedSshSessionOutput {
                session: client.output_session_config(),
                live: false,
                stdout: String::new(),
                stderr: "Connection closed by 192.0.96.181 port 22".to_string(),
                exit_code: 255,
            },
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
                exit_code: 0,
                child_resource: None,
            },
        );

        assert!(!output.live);
        assert_eq!(output.exit_code, 0);
        assert!(output.stderr.contains("Connection closed"));
        assert!(output.stderr.contains("per-command SSH succeeded"));
    }

    #[test]
    fn test_execute_interactive() {
        let client = SshClient {
            host: "localhost".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        };

        assert_eq!(client.execute_interactive(Some("true")), 0);
    }

    #[test]
    fn test_execute_local_command_interactive() {
        assert_eq!(execute_local_command_interactive("true", None, None), 0);
    }

    #[test]
    fn test_execute_local_command_passthrough() {
        let output = execute_local_command_passthrough("printf 'passthrough\\n'", None, None);

        assert!(output.success);
        assert_eq!(output.stdout, "passthrough\n");
    }

    fn local_managed_session_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            auth: Some(ManagedSshSession {
                control_path: "/tmp/homeboy-local-control".to_string(),
                persist: "10m".to_string(),
            }),
            is_local: true,
            env: HashMap::new(),
        }
    }

    #[test]
    fn test_execute_local_command_stderr_passthrough() {
        let output = execute_local_command_stderr_passthrough(
            "printf '{\"ok\":true}\n'; printf 'progress turn=1\n' >&2",
            None,
            None,
        );

        assert!(output.success);
        assert_eq!(output.stdout, "{\"ok\":true}\n");
        assert_eq!(output.stderr, "progress turn=1\n");
    }

    #[cfg(unix)]
    #[test]
    fn process_cleanup_kills_lingering_background_children() {
        let pid_file = std::env::temp_dir().join(format!(
            "homeboy-process-cleanup-{}.pid",
            uuid::Uuid::new_v4()
        ));
        let command = format!(
            "sh -c 'sleep 30 >/dev/null 2>&1 < /dev/null & echo $! > {}'",
            crate::core::engine::shell::quote_path(&pid_file.to_string_lossy())
        );

        let output = execute_local_command_in_dir(&command, None, None);

        assert!(output.success, "command failed: {}", output.stderr);
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        let _ = std::fs::remove_file(&pid_file);

        for _ in 0..20 {
            if !pid_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            !pid_is_alive(pid),
            "background child {pid} should be cleaned up"
        );
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: libc::pid_t) -> bool {
        u32::try_from(pid)
            .map(crate::core::process::pid_is_running)
            .unwrap_or(false)
    }
}
