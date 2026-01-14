use crate::error::{Error, RemoteCommandFailedDetails, Result, TargetDetails};
use crate::server::Server;
use crate::shell;
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

impl CommandOutput {
    pub fn into_remote_result(self, command: &str, target: TargetDetails) -> Result<Self> {
        if self.success {
            return Ok(self);
        }

        Err(Error::remote_command_failed(RemoteCommandFailedDetails {
            command: command.to_string(),
            exit_code: self.exit_code,
            stdout: self.stdout,
            stderr: self.stderr,
            target,
        }))
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

        Ok(Self {
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            identity_file,
        })
    }

    pub fn execute(&self, command: &str) -> CommandOutput {
        self.execute_with_stdin(command, None)
    }

    pub fn upload_file(&self, local_path: &str, remote_path: &str) -> CommandOutput {
        let remote_command = format!("cat > {}", shell::quote_path(remote_path));
        self.execute_with_stdin(&remote_command, Some(local_path))
    }

    fn execute_with_stdin(&self, command: &str, stdin_file: Option<&str>) -> CommandOutput {
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
    execute_local_command_in_dir(command, None)
}

pub fn execute_local_command_in_dir(command: &str, current_dir: Option<&str>) -> CommandOutput {
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
