//! collection — extracted from health.rs.

use super::super::SshClient;
use crate::project::Project;
use serde::Serialize;
use super::parse_health_output;
use super::shell_quote_service;
use super::ServerHealth;


/// Collect health metrics for a project by resolving its server and SSH-ing in.
/// Returns None if the server can't be reached or isn't configured.
pub fn collect_project_health(proj: &Project) -> Option<ServerHealth> {
    let server_id = proj.server_id.as_deref()?;
    let srv = super::load(server_id).ok()?;
    let client = SshClient::from_server(&srv, server_id).ok()?;
    Some(collect_server_health(&client, &proj.services))
}

/// Collect server health metrics via a single SSH command.
///
/// Runs a compound shell command that outputs structured, delimited sections
/// for uptime, load, disk, memory, and optionally service statuses.
pub(crate) fn collect_server_health(client: &SshClient, services: &[String]) -> ServerHealth {
    // Build a single compound command to minimize SSH round-trips.
    // Each section is delimited by a marker line for reliable parsing.
    let mut cmd_parts = vec![
        "echo '---UPTIME---'".to_string(),
        "uptime -p 2>/dev/null || uptime".to_string(),
        "echo '---LOAD---'".to_string(),
        "cat /proc/loadavg 2>/dev/null".to_string(),
        "echo '---CPUS---'".to_string(),
        "nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo 2>/dev/null || echo 1".to_string(),
        "echo '---DISK---'".to_string(),
        "df -h / 2>/dev/null | tail -1".to_string(),
        "echo '---MEMORY---'".to_string(),
        "free -h 2>/dev/null | grep '^Mem:'".to_string(),
    ];

    if !services.is_empty() {
        cmd_parts.push("echo '---SERVICES---'".to_string());
        for svc in services {
            // Output: service_name:status
            cmd_parts.push(format!(
                "echo '{}:'$(systemctl is-active {} 2>/dev/null || echo 'unknown')",
                svc,
                shell_quote_service(svc)
            ));
        }
    }

    let compound_cmd = cmd_parts.join(" && ");
    let output = client.execute(&compound_cmd);

    if !output.success && output.stdout.is_empty() {
        // Total SSH failure — return empty health
        return ServerHealth::default();
    }

    parse_health_output(&output.stdout, services)
}
