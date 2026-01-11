use crate::config::ServerConfig;
use crate::Result;
use std::process::{Command, Stdio};

pub struct SshClient {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
}

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: i32,
}

impl SshClient {
    pub fn from_server(server: &ServerConfig, server_id: &str) -> Result<Self> {
        let identity_file = match &server.identity_file {
            Some(path) if !path.is_empty() => {
                let expanded = shellexpand::tilde(path).to_string();
                if !std::path::Path::new(&expanded).exists() {
                    return Err(crate::Error::Ssh(format!(
                        "SSH identity file not found for server '{}': {}",
                        server_id, expanded
                    )));
                }
                Some(expanded)
            }
            _ => None,
        };

        Ok(Self {
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            identity_file,
        })
    }

    pub fn execute(&self, command: &str) -> CommandOutput {
        let mut args = Vec::new();

        if let Some(identity_file) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        args.push(format!("{}@{}", self.user, self.host));
        args.push(command.to_string());

        let output = Command::new("/usr/bin/ssh").args(&args).output();

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
        let mut args = Vec::new();

        if let Some(identity_file) = &self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        args.push(format!("{}@{}", self.user, self.host));

        if let Some(cmd) = command {
            args.push(cmd.to_string());
        }

        let status = Command::new("/usr/bin/ssh")
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
    let output = Command::new("/bin/bash").args(["-c", command]).output();

    match output {
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
