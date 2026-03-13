//! Server health metrics collection via SSH.
//!
//! Core primitive used by both `fleet status` and `project status` commands
//! to collect uptime, load, disk, memory, and service health from remote servers.

use super::SshClient;
use crate::project::Project;
use serde::Serialize;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Default, Serialize, Clone)]
pub struct ServerHealth {
    /// Server uptime as a human-readable string (e.g. "10 days")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
    /// Load averages: 1min, 5min, 15min
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadAverage>,
    /// Disk usage for the primary filesystem
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskUsage>,
    /// Memory usage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryUsage>,
    /// Service statuses (only if project has services configured)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceStatus>,
    /// Warning messages for thresholds exceeded
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    /// Number of CPU cores (for contextualizing load)
    pub cpu_cores: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct DiskUsage {
    /// Used space in human-readable form (e.g. "36G")
    pub used: String,
    /// Total space in human-readable form (e.g. "150G")
    pub total: String,
    /// Usage percentage (0-100)
    pub percent: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct MemoryUsage {
    /// Used memory in human-readable form (e.g. "2.2G")
    pub used: String,
    /// Total memory in human-readable form (e.g. "7.6G")
    pub total: String,
    /// Usage percentage (0-100)
    pub percent: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct ServiceStatus {
    pub name: String,
    pub active: bool,
    /// Raw status string from systemctl (e.g. "active", "inactive", "failed")
    pub status: String,
}

// ============================================================================
// Collection
// ============================================================================

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
fn collect_server_health(client: &SshClient, services: &[String]) -> ServerHealth {
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

// ============================================================================
// Parsing
// ============================================================================

/// Quote a service name for safe use in shell commands.
fn shell_quote_service(name: &str) -> String {
    // Only allow alphanumeric, dash, dot, underscore, @ (systemd instance separator)
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | '@'))
    {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "'\\''"))
    }
}

/// Parse the structured health output from the compound SSH command.
fn parse_health_output(output: &str, services: &[String]) -> ServerHealth {
    let mut health = ServerHealth::default();
    let mut current_section = "";

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            "---UPTIME---" => {
                current_section = "uptime";
                continue;
            }
            "---LOAD---" => {
                current_section = "load";
                continue;
            }
            "---CPUS---" => {
                current_section = "cpus";
                continue;
            }
            "---DISK---" => {
                current_section = "disk";
                continue;
            }
            "---MEMORY---" => {
                current_section = "memory";
                continue;
            }
            "---SERVICES---" => {
                current_section = "services";
                continue;
            }
            _ => {}
        }

        match current_section {
            "uptime" => {
                health.uptime = Some(parse_uptime(trimmed));
            }
            "load" => {
                health.load = parse_load_average(trimmed, health.load.as_ref());
            }
            "cpus" => {
                if let Some(ref mut load) = health.load {
                    if let Ok(cores) = trimmed.parse::<u32>() {
                        load.cpu_cores = cores;
                    }
                }
            }
            "disk" => {
                health.disk = parse_disk_usage(trimmed);
            }
            "memory" => {
                health.memory = parse_memory_usage(trimmed);
            }
            "services" if !services.is_empty() => {
                if let Some(svc) = parse_service_status(trimmed) {
                    health.services.push(svc);
                }
            }
            _ => {}
        }
    }

    // Generate warnings
    health.warnings = generate_warnings(&health);

    health
}

/// Parse `uptime -p` output (e.g. "up 10 days, 3 hours, 22 minutes") or
/// fallback `uptime` output.
fn parse_uptime(line: &str) -> String {
    // `uptime -p` produces "up 10 days, 3 hours, 22 minutes"
    if let Some(rest) = line.strip_prefix("up ") {
        return rest.to_string();
    }
    // Fallback: extract from full uptime line "... up 10 days, ..."
    if let Some(up_pos) = line.find("up ") {
        let after_up = &line[up_pos + 3..];
        // Find the end: typically before "user" or a load indicator
        if let Some(user_pos) = after_up.find("user") {
            let segment = after_up[..user_pos].trim_end_matches([',', ' ']);
            // Remove the trailing number (user count) — e.g. "10 days,  3"
            if let Some(last_comma) = segment.rfind(',') {
                return segment[..last_comma].trim().to_string();
            }
            return segment.to_string();
        }
        return after_up.trim().to_string();
    }
    line.to_string()
}

/// Parse /proc/loadavg (e.g. "0.02 0.08 0.08 1/234 12345").
fn parse_load_average(line: &str, existing: Option<&LoadAverage>) -> Option<LoadAverage> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 3 {
        Some(LoadAverage {
            one: parts[0].parse().unwrap_or(0.0),
            five: parts[1].parse().unwrap_or(0.0),
            fifteen: parts[2].parse().unwrap_or(0.0),
            cpu_cores: existing.map_or(1, |l| l.cpu_cores),
        })
    } else {
        None
    }
}

/// Parse `df -h /` output line (e.g. "/dev/sda1  150G  36G  114G  25% /").
fn parse_disk_usage(line: &str) -> Option<DiskUsage> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    // Standard df -h output: Filesystem Size Used Avail Use% Mounted
    if parts.len() >= 5 {
        let percent_str = parts[4].trim_end_matches('%');
        Some(DiskUsage {
            total: parts[1].to_string(),
            used: parts[2].to_string(),
            percent: percent_str.parse().unwrap_or(0),
        })
    } else {
        None
    }
}

/// Parse `free -h` Mem line (e.g. "Mem:  7.6Gi  2.2Gi  3.1Gi  ...").
fn parse_memory_usage(line: &str) -> Option<MemoryUsage> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    // free -h: Mem: total used free shared buff/cache available
    if parts.len() >= 3 {
        let total = parts[1].to_string();
        let used = parts[2].to_string();
        let total_bytes = parse_human_bytes(&total);
        let used_bytes = parse_human_bytes(&used);
        let percent = if total_bytes > 0 {
            ((used_bytes as f64 / total_bytes as f64) * 100.0) as u32
        } else {
            0
        };
        Some(MemoryUsage {
            total,
            used,
            percent,
        })
    } else {
        None
    }
}

/// Parse human-readable byte strings (e.g. "7.6Gi", "2.2G", "150M") to bytes.
fn parse_human_bytes(s: &str) -> u64 {
    let s = s.trim();
    // Try to split into number and suffix
    let mut num_end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() || c == '.' {
            num_end = i + c.len_utf8();
        } else {
            break;
        }
    }

    if num_end == 0 {
        return 0;
    }

    let num: f64 = match s[..num_end].parse() {
        Ok(n) => n,
        Err(_) => return 0,
    };

    let suffix = s[num_end..].trim().to_uppercase();
    let multiplier: f64 = match suffix.as_str() {
        "K" | "KI" | "KB" | "KIB" => 1024.0,
        "M" | "MI" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GI" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TI" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };

    (num * multiplier) as u64
}

/// Parse "service_name:status" line from systemctl output.
fn parse_service_status(line: &str) -> Option<ServiceStatus> {
    let (name, status) = line.split_once(':')?;
    let status_str = status.trim();
    Some(ServiceStatus {
        name: name.trim().to_string(),
        active: status_str == "active",
        status: status_str.to_string(),
    })
}

// ============================================================================
// Warnings
// ============================================================================

/// Generate warning messages based on health thresholds.
fn generate_warnings(health: &ServerHealth) -> Vec<String> {
    let mut warnings = Vec::new();

    if let Some(ref load) = health.load {
        let threshold = load.cpu_cores.max(1) as f64;
        if load.one > threshold {
            warnings.push(format!(
                "High load: {:.2} ({:.1}x cores)",
                load.one,
                load.one / threshold
            ));
        }
    }

    if let Some(ref disk) = health.disk {
        if disk.percent >= 90 {
            warnings.push(format!("Disk critically full: {}% used", disk.percent));
        } else if disk.percent >= 80 {
            warnings.push(format!("Disk usage high: {}% used", disk.percent));
        }
    }

    if let Some(ref mem) = health.memory {
        if mem.percent >= 90 {
            warnings.push(format!("Memory critically high: {}% used", mem.percent));
        } else if mem.percent >= 80 {
            warnings.push(format!("Memory usage high: {}% used", mem.percent));
        }
    }

    for svc in &health.services {
        if !svc.active {
            warnings.push(format!("Service down: {} ({})", svc.name, svc.status));
        }
    }

    warnings
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uptime_p() {
        assert_eq!(
            parse_uptime("up 10 days, 3 hours, 22 minutes"),
            "10 days, 3 hours, 22 minutes"
        );
    }

    #[test]
    fn test_parse_uptime_p_short() {
        assert_eq!(parse_uptime("up 45 minutes"), "45 minutes");
    }

    #[test]
    fn test_parse_uptime_fallback() {
        let line = " 14:32:01 up 10 days,  3:22,  1 user,  load average: 0.02, 0.08, 0.08";
        let result = parse_uptime(line);
        assert_eq!(result, "10 days,  3:22");
    }

    #[test]
    fn test_parse_load_average() {
        let load = parse_load_average("0.02 0.08 0.08 1/234 12345", None).unwrap();
        assert!((load.one - 0.02).abs() < 0.001);
        assert!((load.five - 0.08).abs() < 0.001);
        assert!((load.fifteen - 0.08).abs() < 0.001);
        assert_eq!(load.cpu_cores, 1);
    }

    #[test]
    fn test_parse_load_average_preserves_cores() {
        let existing = LoadAverage {
            cpu_cores: 4,
            ..Default::default()
        };
        let load = parse_load_average("1.50 0.80 0.40 2/300 999", Some(&existing)).unwrap();
        assert_eq!(load.cpu_cores, 4);
        assert!((load.one - 1.50).abs() < 0.001);
    }

    #[test]
    fn test_parse_disk_usage() {
        let disk = parse_disk_usage("/dev/sda1       150G   36G  114G  25% /").unwrap();
        assert_eq!(disk.total, "150G");
        assert_eq!(disk.used, "36G");
        assert_eq!(disk.percent, 25);
    }

    #[test]
    fn test_parse_disk_usage_high() {
        let disk = parse_disk_usage("/dev/vda1        75G   69G  2.4G  97% /").unwrap();
        assert_eq!(disk.percent, 97);
    }

    #[test]
    fn test_parse_memory_usage() {
        let mem = parse_memory_usage(
            "Mem:          7.6Gi       2.2Gi       3.1Gi       0.0Ki       2.3Gi       5.1Gi",
        )
        .unwrap();
        assert_eq!(mem.total, "7.6Gi");
        assert_eq!(mem.used, "2.2Gi");
        assert!(mem.percent >= 28 && mem.percent <= 30);
    }

    #[test]
    fn test_parse_human_bytes() {
        assert_eq!(parse_human_bytes("7.6Gi"), 8_160_437_862);
        assert_eq!(parse_human_bytes("2.2Gi"), 2_362_232_012);
        assert_eq!(parse_human_bytes("150G"), 161_061_273_600);
        assert_eq!(parse_human_bytes("512M"), 536_870_912);
        assert_eq!(parse_human_bytes("1024K"), 1_048_576);
        assert_eq!(parse_human_bytes("0"), 0);
    }

    #[test]
    fn test_parse_service_status_active() {
        let svc = parse_service_status("kimaki:active").unwrap();
        assert_eq!(svc.name, "kimaki");
        assert!(svc.active);
        assert_eq!(svc.status, "active");
    }

    #[test]
    fn test_parse_service_status_inactive() {
        let svc = parse_service_status("php8.4-fpm:inactive").unwrap();
        assert_eq!(svc.name, "php8.4-fpm");
        assert!(!svc.active);
        assert_eq!(svc.status, "inactive");
    }

    #[test]
    fn test_parse_service_status_failed() {
        let svc = parse_service_status("mysql:failed").unwrap();
        assert_eq!(svc.name, "mysql");
        assert!(!svc.active);
        assert_eq!(svc.status, "failed");
    }

    #[test]
    fn test_parse_health_output_full() {
        let output = "\
---UPTIME---
up 10 days, 3 hours
---LOAD---
0.02 0.08 0.08 1/234 12345
---CPUS---
4
---DISK---
/dev/sda1       150G   36G  114G  25% /
---MEMORY---
Mem:          7.6Gi       2.2Gi       3.1Gi       0.0Ki       2.3Gi       5.1Gi
---SERVICES---
kimaki:active
nginx:active
mysql:failed
";
        let health =
            parse_health_output(output, &["kimaki".into(), "nginx".into(), "mysql".into()]);

        assert_eq!(health.uptime.as_deref(), Some("10 days, 3 hours"));
        let load = health.load.unwrap();
        assert!((load.one - 0.02).abs() < 0.001);
        assert_eq!(load.cpu_cores, 4);
        let disk = health.disk.unwrap();
        assert_eq!(disk.percent, 25);
        let mem = health.memory.unwrap();
        assert_eq!(mem.total, "7.6Gi");
        assert_eq!(health.services.len(), 3);
        assert!(health.services[0].active);
        assert!(!health.services[2].active);
        assert!(health.warnings.iter().any(|w| w.contains("mysql")));
    }

    #[test]
    fn test_parse_health_output_no_services() {
        let output = "\
---UPTIME---
up 2 hours, 15 minutes
---LOAD---
0.50 0.30 0.20 1/100 5678
---CPUS---
2
---DISK---
/dev/vda1        75G   30G   42G  42% /
---MEMORY---
Mem:          3.8Gi       1.5Gi       1.0Gi       0.0Ki       1.3Gi       2.0Gi
";
        let health = parse_health_output(output, &[]);

        assert_eq!(health.uptime.as_deref(), Some("2 hours, 15 minutes"));
        assert!(health.services.is_empty());
        assert!(health.warnings.is_empty());
    }

    #[test]
    fn test_warnings_high_load() {
        let health = ServerHealth {
            load: Some(LoadAverage {
                one: 5.0,
                five: 3.0,
                fifteen: 2.0,
                cpu_cores: 2,
            }),
            ..Default::default()
        };
        let warnings = generate_warnings(&health);
        assert!(warnings.iter().any(|w| w.contains("High load")));
    }

    #[test]
    fn test_warnings_high_disk() {
        let health = ServerHealth {
            disk: Some(DiskUsage {
                used: "135G".into(),
                total: "150G".into(),
                percent: 92,
            }),
            ..Default::default()
        };
        let warnings = generate_warnings(&health);
        assert!(warnings.iter().any(|w| w.contains("critically full")));
    }

    #[test]
    fn test_warnings_high_memory() {
        let health = ServerHealth {
            memory: Some(MemoryUsage {
                used: "7.0Gi".into(),
                total: "7.6Gi".into(),
                percent: 92,
            }),
            ..Default::default()
        };
        let warnings = generate_warnings(&health);
        assert!(warnings
            .iter()
            .any(|w| w.contains("Memory critically high")));
    }

    #[test]
    fn test_warnings_service_down() {
        let health = ServerHealth {
            services: vec![
                ServiceStatus {
                    name: "nginx".into(),
                    active: true,
                    status: "active".into(),
                },
                ServiceStatus {
                    name: "php-fpm".into(),
                    active: false,
                    status: "inactive".into(),
                },
            ],
            ..Default::default()
        };
        let warnings = generate_warnings(&health);
        assert!(warnings.iter().any(|w| w.contains("php-fpm")));
        assert!(!warnings.iter().any(|w| w.contains("nginx")));
    }

    #[test]
    fn test_no_warnings_when_healthy() {
        let health = ServerHealth {
            load: Some(LoadAverage {
                one: 0.5,
                five: 0.3,
                fifteen: 0.2,
                cpu_cores: 4,
            }),
            disk: Some(DiskUsage {
                used: "30G".into(),
                total: "150G".into(),
                percent: 20,
            }),
            memory: Some(MemoryUsage {
                used: "2G".into(),
                total: "8G".into(),
                percent: 25,
            }),
            services: vec![ServiceStatus {
                name: "nginx".into(),
                active: true,
                status: "active".into(),
            }],
            ..Default::default()
        };
        let warnings = generate_warnings(&health);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_shell_quote_service_safe() {
        assert_eq!(shell_quote_service("nginx"), "nginx");
        assert_eq!(shell_quote_service("php8.4-fpm"), "php8.4-fpm");
        assert_eq!(shell_quote_service("user@.service"), "user@.service");
    }

    #[test]
    fn test_shell_quote_service_unsafe() {
        let quoted = shell_quote_service("foo; rm -rf /");
        assert!(quoted.starts_with('\''));
    }
}
