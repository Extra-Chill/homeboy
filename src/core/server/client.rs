use std::collections::HashMap;

use crate::engine::shell;
use crate::error::{Error, Result};

use super::Server;
use std::process::{Command, Stdio};

pub struct SshClient {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
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

        Ok(Self {
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            identity_file,
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

    pub fn execute(&self, command: &str) -> CommandOutput {
        let effective = self.prepend_env(command);
        self.execute_with_stdin(&effective, None)
    }

    /// Build an env preamble that sets configured variables via `export`.
    /// Values are quoted but allow shell expansion (e.g. `$PATH`).
    fn prepend_env(&self, command: &str) -> String {
        if self.env.is_empty() {
            return command.to_string();
        }
        let exports: Vec<String> = self
            .env
            .iter()
            .map(|(k, v)| format!("export {}={}", k, shell::quote_arg(v)))
            .collect();
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
            },
            Err(e) => CommandOutput {
                stdout: String::new(),
                stderr: format!("SSH error: {}", e),
                success: false,
                exit_code: -1,
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

pub fn execute_local_command_in_dir(
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

    match cmd.output() {
        Ok(out) => CommandOutput {
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            success: out.status.success(),
            exit_code: out.status.code().unwrap_or(-1),
        },
        Err(e) => CommandOutput {
            stdout: String::new(),
            stderr: format!("Command error: {}", e),
            success: false,
            exit_code: -1,
        },
    }
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

/// Execute local command with stdout/stderr passed through to terminal.
/// Returns only exit status, not captured output.
pub fn execute_local_command_passthrough(
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

    // Passthrough to terminal instead of capturing
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: status.success(),
            exit_code: status.code().unwrap_or(-1),
        },
        Err(e) => CommandOutput {
            stdout: String::new(),
            stderr: format!("Command error: {}", e),
            success: false,
            exit_code: -1,
        },
    }
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

/// Collect all IP addresses assigned to local network interfaces.
///
/// Uses `ip -o addr show` on Linux and `ifconfig` on macOS.
/// Returns None if the command fails (graceful degradation — falls back
/// to localhost-only matching).
fn get_local_ips() -> Option<Vec<std::net::IpAddr>> {
    #[cfg(target_os = "linux")]
    {
        // `ip -o addr show` outputs one line per address, e.g.:
        // 2: eth0    inet 178.156.237.104/24 brd 178.156.237.255 scope global eth0
        let output = std::process::Command::new("ip")
            .args(["-o", "addr", "show"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ips: Vec<std::net::IpAddr> = stdout
            .lines()
            .filter_map(|line| {
                // Fields: index, iface, family, addr/prefix, ...
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 4 {
                    return None;
                }
                // The address is in field 3, formatted as "addr/prefix"
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

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ips: Vec<std::net::IpAddr> = stdout
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("inet ") {
                    // "inet 192.168.1.5 netmask ..."
                    rest.split_whitespace().next()?.parse().ok()
                } else if let Some(rest) = line.strip_prefix("inet6 ") {
                    // "inet6 fe80::1%lo0 prefixlen ..."
                    let addr_str = rest.split_whitespace().next()?;
                    // Strip zone ID (e.g. %lo0)
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
}
