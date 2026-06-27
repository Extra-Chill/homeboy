use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::runner::{quote_runner_env_value, remote_shell_path_preamble};

use super::super::session::ensure_control_path_parent;
use super::super::{
    ManagedSshSession, ManagedSshSessionOutput, Server, ServerAuthMode, ServerSessionConfig,
};
use super::host::{is_local_host, is_transient_ssh_error};
use super::local_exec::{
    execute_local_command, execute_local_command_interactive, execute_local_command_with_stdin,
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

    pub(crate) fn build_ssh_args(&self, command: Option<&str>, interactive: bool) -> Vec<String> {
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
        let effective = self.prepend_env(command);
        self.execute_with_stdin(&effective, SshStdin::None)
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
