use std::process::{Command, Stdio};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::runner::{quote_runner_env_value, remote_shell_path_preamble};

use super::super::session::ensure_control_path_parent;
use super::super::{
    ManagedSshSession, ManagedSshSessionOutput, Server, ServerAuthMode, ServerSessionConfig,
};
use super::host::{is_local_host, is_transient_ssh_error};
use super::local_exec::{execute_local_command, execute_local_command_interactive};
use super::{CommandOutput, SshClient};

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

    pub fn download_file(&self, remote_path: &str, local_path: &str) -> CommandOutput {
        if self.is_local {
            return match std::fs::copy(remote_path, local_path) {
                Ok(_) => CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                    exit_code: 0,
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
                child_resource: None,
            },
            Err(err) => CommandOutput {
                stdout: String::new(),
                stderr: format!("SSH download error: {}", err),
                success: false,
                exit_code: -1,
                child_resource: None,
            },
        }
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
