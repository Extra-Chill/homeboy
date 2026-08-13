use crate::engine::shell;

use super::{ManagedSshSession, Server, SshClient};

#[derive(Clone, Copy)]
pub enum SshPortFlag {
    Lowercase,
    Uppercase,
}

#[derive(Clone, Copy, Default)]
pub struct SshArgOptions<'a> {
    pub interactive: bool,
    pub strict_host_key_checking_no: bool,
    pub batch_mode: bool,
    pub connect_timeout: bool,
    pub keepalive: bool,
    pub exit_on_forward_failure: bool,
    /// Persistent forwards must be owned by their spawned SSH child rather
    /// than absorbed by a configured ControlMaster.
    pub disable_multiplexing: bool,
    pub legacy_scp: bool,
    pub port_flag: Option<SshPortFlag>,
    pub command: Option<&'a str>,
}

pub(crate) fn client_ssh_args(client: &SshClient, options: SshArgOptions<'_>) -> Vec<String> {
    let mut args = client_connection_args(
        &client.user,
        &client.host,
        client.port,
        client.identity_file.as_deref(),
        client.auth.as_ref(),
        options,
    );
    args.push(format!("{}@{}", client.user, client.host));
    if let Some(command) = options.command {
        args.push(command.to_string());
    }
    args
}

pub fn client_option_args(client: &SshClient, options: SshArgOptions<'_>) -> Vec<String> {
    client_connection_args(
        &client.user,
        &client.host,
        client.port,
        client.identity_file.as_deref(),
        client.auth.as_ref(),
        options,
    )
}

pub fn server_option_args(server: &Server, options: SshArgOptions<'_>) -> Vec<String> {
    let session = server.auth.as_ref().map(ManagedSshSession::from_auth);
    client_connection_args(
        &server.user,
        &server.host,
        server.port,
        server
            .identity_file
            .as_deref()
            .filter(|path| !path.is_empty()),
        session.as_ref(),
        options,
    )
}

pub fn shell_join_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell::quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn client_connection_args(
    _user: &str,
    _host: &str,
    port: u16,
    identity_file: Option<&str>,
    session: Option<&ManagedSshSession>,
    options: SshArgOptions<'_>,
) -> Vec<String> {
    let mut args = Vec::new();

    if options.legacy_scp {
        args.push("-O".to_string());
    }

    if let Some(identity_file) = identity_file {
        args.push("-i".to_string());
        args.push(shellexpand::tilde(identity_file).to_string());
    }

    if let Some(flag) = options.port_flag {
        if port != 22 {
            args.push(match flag {
                SshPortFlag::Lowercase => "-p".to_string(),
                SshPortFlag::Uppercase => "-P".to_string(),
            });
            args.push(port.to_string());
        }
    }

    if options.strict_host_key_checking_no {
        push_option(&mut args, "StrictHostKeyChecking=no");
    }

    if options.disable_multiplexing {
        // `ControlMaster=no` still attaches to an existing socket when
        // ControlPath is set. `none` makes this child own its forward.
        push_option(&mut args, "ControlMaster=no");
        push_option(&mut args, "ControlPath=none");
    } else if let Some(session) = session {
        // A managed session is established explicitly by `server connect`. Command
        // clients attach to that socket rather than starting a competing master.
        // `-o ControlPath` is understood by both ssh and scp; scp's `-S` means
        // the SSH executable path rather than the control socket.
        push_option(&mut args, format!("ControlPath={}", session.control_path));
        push_option(&mut args, "ControlMaster=no");
    }

    if options.batch_mode && !options.interactive {
        push_option(&mut args, "BatchMode=yes");
    }
    if options.exit_on_forward_failure {
        push_option(&mut args, "ExitOnForwardFailure=yes");
    }
    if options.connect_timeout && !options.interactive {
        push_option(&mut args, "ConnectTimeout=10");
    }
    if options.keepalive && !options.interactive {
        push_option(&mut args, "ServerAliveInterval=15");
        push_option(&mut args, "ServerAliveCountMax=3");
    }

    // ssh allocates a TTY by default only when no remote command is given
    // (#10839). An interactive session that carries one -- `cd <base_path> &&
    // exec $SHELL` -- therefore gets no TTY, and the shell it execs would come
    // up non-interactive, with no prompt and no job control. Ask for the
    // terminal explicitly.
    //
    // Scoped to the interactive path: `interactive` is set by exactly one
    // caller, `SshClient::execute_interactive`. Every non-interactive caller
    // captures output and must not be handed a pty.
    if options.interactive && options.command.is_some() {
        args.push("-t".to_string());
    }

    args
}

fn push_option(args: &mut Vec<String>, option: impl Into<String>) {
    args.push("-o".to_string());
    args.push(option.into());
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::process::Command;

    use super::*;

    #[test]
    fn shell_join_quotes_ssh_option_values_with_spaces() {
        let client = SshClient {
            host: "example.test".to_string(),
            user: "deploy".to_string(),
            port: 2222,
            identity_file: Some("/tmp/key with spaces".to_string()),
            auth: Some(ManagedSshSession {
                control_path: "/tmp/control path".to_string(),
                persist: "4h".to_string(),
                persist_source: crate::server::ManagedSshSessionPersistSource::Configured,
            }),
            is_local: false,
            env: HashMap::new(),
        };

        let rendered = shell_join_args(&client_option_args(
            &client,
            SshArgOptions {
                batch_mode: true,
                port_flag: Some(SshPortFlag::Lowercase),
                ..SshArgOptions::default()
            },
        ));

        assert!(rendered.contains("-i '/tmp/key with spaces'"));
        assert!(rendered.contains("-o 'ControlPath=/tmp/control path'"));
        assert!(rendered.contains("-o ControlMaster=no"));
        assert!(rendered.contains("-p 2222"));
    }

    #[test]
    fn installed_openssh_expands_managed_controlpath_for_a_host_alias() {
        // `ssh -G` parses configuration without opening a network connection. It
        // proves that the actual OpenSSH client expands Homeboy's `%h/%p/%r`
        // control path from an alias's configured HostName, port, and user.
        let fixture = tempfile::tempdir().expect("SSH config fixture");
        let config = fixture.path().join("ssh_config");
        let control_path = fixture.path().join("control-%h-%p-%r");
        std::fs::write(
            &config,
            "Host sandbox-alias\n  HostName resolved.example.test\n  Port 2222\n",
        )
        .expect("write SSH config");
        let client = SshClient {
            host: "sandbox-alias".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: None,
            auth: Some(ManagedSshSession {
                control_path: control_path.to_string_lossy().to_string(),
                persist: "4h".to_string(),
                persist_source: crate::server::ManagedSshSessionPersistSource::Configured,
            }),
            is_local: false,
            env: HashMap::new(),
        };
        let options = client_option_args(
            &client,
            SshArgOptions {
                batch_mode: true,
                ..SshArgOptions::default()
            },
        );

        let output = Command::new("ssh")
            .args(["-G", "-F"])
            .arg(&config)
            .args(options)
            .arg("deploy@sandbox-alias")
            .output()
            .expect("installed OpenSSH client");
        assert!(
            output.status.success(),
            "ssh -G failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let effective = String::from_utf8(output.stdout).expect("OpenSSH config output");
        assert!(effective.contains("hostname resolved.example.test\n"));
        assert!(effective.contains("port 2222\n"));
        assert!(effective.contains("user deploy\n"));
        assert!(effective.contains("controlmaster false\n"));
        assert!(effective.contains(&format!(
            "controlpath {}\n",
            fixture
                .path()
                .join("control-resolved.example.test-2222-deploy")
                .display()
        )));
    }

    #[test]
    fn persistent_forward_options_bypass_a_managed_controlmaster() {
        let server: Server = serde_json::from_value(serde_json::json!({
            "id": "runner",
            "host": "example.test",
            "user": "deploy",
            "auth": {
                "mode": "key_plus_password_controlmaster",
                "control_path": "/tmp/homeboy-control",
                "persist": "1h"
            }
        }))
        .expect("server");

        let args = server_option_args(
            &server,
            SshArgOptions {
                disable_multiplexing: true,
                ..SshArgOptions::default()
            },
        );

        assert!(args.contains(&"ControlPath=none".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("/tmp/homeboy-control")));
    }
}
