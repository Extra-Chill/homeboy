use crate::engine::shell;

use super::{ManagedSshSession, Server, ServerAuthMode, SshClient};

#[derive(Clone, Copy)]
pub(crate) enum SshPortFlag {
    Lowercase,
    Uppercase,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SshArgOptions<'a> {
    pub(crate) interactive: bool,
    pub(crate) strict_host_key_checking_no: bool,
    pub(crate) batch_mode: bool,
    pub(crate) connect_timeout: bool,
    pub(crate) keepalive: bool,
    pub(crate) exit_on_forward_failure: bool,
    pub(crate) legacy_scp: bool,
    pub(crate) port_flag: Option<SshPortFlag>,
    pub(crate) command: Option<&'a str>,
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

pub(crate) fn client_option_args(client: &SshClient, options: SshArgOptions<'_>) -> Vec<String> {
    client_connection_args(
        &client.user,
        &client.host,
        client.port,
        client.identity_file.as_deref(),
        client.auth.as_ref(),
        options,
    )
}

pub(crate) fn server_option_args(server: &Server, options: SshArgOptions<'_>) -> Vec<String> {
    let auth = server
        .auth
        .as_ref()
        .filter(|auth| auth.mode == ServerAuthMode::KeyPlusPasswordControlmaster)
        .map(ManagedSshSession::from_auth);
    client_connection_args(
        &server.user,
        &server.host,
        server.port,
        server
            .identity_file
            .as_deref()
            .filter(|path| !path.is_empty()),
        auth.as_ref(),
        options,
    )
}

pub(crate) fn shell_join_args(args: &[String]) -> String {
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
    auth: Option<&ManagedSshSession>,
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

    if let Some(session) = auth {
        push_option(&mut args, "ControlMaster=auto");
        push_option(&mut args, format!("ControlPath={}", session.control_path));
        push_option(&mut args, format!("ControlPersist={}", session.persist));
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

    args
}

fn push_option(args: &mut Vec<String>, option: impl Into<String>) {
    args.push("-o".to_string());
    args.push(option.into());
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
        assert!(rendered.contains("-p 2222"));
    }
}
