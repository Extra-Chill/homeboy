use super::CommandOutput;

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

/// Check if an SSH failure is a transient connection error worth retrying.
pub(crate) fn is_transient_ssh_error(output: &CommandOutput) -> bool {
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
