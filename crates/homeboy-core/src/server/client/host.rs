use super::CommandOutput;
use crate::server::{ssh_args, Server};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

const LOOPBACK_HOST_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(1);

/// Resolve a configured server host for direct-SSH transport selection.
///
/// A hostname is loopback only when every resolved address is loopback. This
/// prevents a mixed DNS answer from silently bypassing SSH tunnel ownership.
pub fn server_host_resolves_only_to_loopback(host: &str, port: u16) -> Result<bool, String> {
    server_host_resolves_only_to_loopback_with(host, port, |host, port| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let host = host.to_string();
        let resolve_host = host.clone();
        std::thread::spawn(move || {
            let result = (resolve_host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.map(|address| address.ip()).collect());
            let _ = sender.send(result);
        });
        receiver
            .recv_timeout(LOOPBACK_HOST_RESOLUTION_TIMEOUT)
            .map_err(|_| format!("resolve server host `{host}` timed out"))?
            .map_err(|error| format!("resolve server host `{host}`: {error}"))
    })
}

/// Determine whether a direct SSH transport stays local after OpenSSH expands
/// the configured host alias. ProxyJump and ProxyCommand always make the
/// transport remote, even when the final hostname resolves to loopback.
pub fn server_uses_loopback_transport(server: &Server) -> Result<bool, String> {
    let mut command = std::process::Command::new("ssh");
    command.arg("-G").args(ssh_args::server_option_args(
        server,
        ssh_args::SshArgOptions {
            disable_multiplexing: true,
            port_flag: Some(ssh_args::SshPortFlag::Lowercase),
            ..ssh_args::SshArgOptions::default()
        },
    ));
    command.arg(format!("{}@{}", server.user, server.host));
    let output = command
        .output()
        .map_err(|error| format!("expand SSH configuration: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "expand SSH configuration: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    server_uses_loopback_transport_from_ssh_config(
        &String::from_utf8_lossy(&output.stdout),
        |host, port| server_host_resolves_only_to_loopback(host, port),
    )
}

fn server_uses_loopback_transport_from_ssh_config(
    config: &str,
    resolves_only_to_loopback: impl FnOnce(&str, u16) -> Result<bool, String>,
) -> Result<bool, String> {
    let mut hostname = None;
    let mut port = None;
    let mut proxy = false;
    for line in config.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "hostname" => hostname = Some(value),
            "port" => port = value.parse::<u16>().ok(),
            "proxyjump" | "proxycommand" if !matches!(value, "none" | "") => proxy = true,
            _ => {}
        }
    }
    if proxy {
        return Ok(false);
    }
    let hostname =
        hostname.ok_or_else(|| "expand SSH configuration returned no hostname".to_string())?;
    let port = port.ok_or_else(|| "expand SSH configuration returned no port".to_string())?;
    resolves_only_to_loopback(hostname, port)
}

fn server_host_resolves_only_to_loopback_with(
    host: &str,
    port: u16,
    resolve: impl FnOnce(&str, u16) -> Result<Vec<IpAddr>, String>,
) -> Result<bool, String> {
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Ok(true);
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(address.is_loopback());
    }
    let addresses = resolve(host, port)?;
    if addresses.is_empty() {
        return Err(format!(
            "resolve server host `{host}` returned no addresses"
        ));
    }
    Ok(addresses.iter().all(IpAddr::is_loopback))
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

pub(crate) fn get_local_ips() -> Option<Vec<std::net::IpAddr>> {
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn successful_command_stdout(output: std::process::Output) -> Option<String> {
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

/// SSH connection failures worth retrying, matched case-insensitively against
/// lowercased stderr.
///
/// This is the single source of truth for the pattern list. Callers that work
/// with a different output type (for example `homeboy-lab-runner`, which sees a
/// raw `std::process::Output` from a piped `sh` invocation rather than a
/// [`CommandOutput`]) import this constant instead of restating the patterns.
pub const TRANSIENT_SSH_STDERR_PATTERNS: [&str; 10] = [
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

#[cfg(test)]
mod tests {
    use super::{
        server_host_resolves_only_to_loopback_with, server_uses_loopback_transport_from_ssh_config,
    };
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn loopback_only_hostname_alias_is_local_transport() {
        assert!(
            server_host_resolves_only_to_loopback_with("loopback-alias", 22, |_, _| Ok(vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            ]))
            .expect("resolve alias")
        );
    }

    #[test]
    fn hostname_with_any_non_loopback_address_requires_ssh_transport() {
        assert!(
            !server_host_resolves_only_to_loopback_with("mixed-alias", 22, |_, _| {
                Ok(vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
                ])
            })
            .expect("resolve alias")
        );
    }

    #[test]
    fn hostname_resolution_failure_rejects_local_transport_classification() {
        let error = server_host_resolves_only_to_loopback_with("unresolved", 22, |_, _| {
            Err("name service unavailable".to_string())
        })
        .expect_err("failed resolution is not loopback evidence");

        assert!(error.contains("name service unavailable"));
    }

    #[test]
    fn ssh_config_alias_to_remote_host_requires_a_tunnel() {
        let config = "hostname 192.0.2.1\nport 22\nproxyjump none\nproxycommand none\n";
        assert!(
            !server_uses_loopback_transport_from_ssh_config(config, |_, _| Ok(false))
                .expect("classify")
        );
    }

    #[test]
    fn ssh_config_true_local_alias_bypasses_a_tunnel() {
        let config = "hostname 127.0.0.1\nport 2222\nproxyjump none\nproxycommand none\n";
        assert!(
            server_uses_loopback_transport_from_ssh_config(config, |host, port| {
                Ok(host == "127.0.0.1" && port == 2222)
            })
            .expect("classify")
        );
    }

    #[test]
    fn ssh_proxy_semantics_require_a_tunnel_even_for_loopback_destination() {
        let config = "hostname 127.0.0.1\nport 22\nproxyjump bastion.example\nproxycommand none\n";
        assert!(
            !server_uses_loopback_transport_from_ssh_config(config, |_, _| Ok(true))
                .expect("classify")
        );
    }
}

/// Check if an SSH failure is a transient connection error worth retrying.
pub fn is_transient_ssh_error(output: &CommandOutput) -> bool {
    let stderr = output.stderr.to_lowercase();
    // SSH exit code 255 = connection error (not a remote command failure)
    let is_connection_exit = output.exit_code == 255;

    is_connection_exit
        || TRANSIENT_SSH_STDERR_PATTERNS
            .iter()
            .any(|p| stderr.contains(p))
}
