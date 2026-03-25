//! parsing — extracted from health.rs.

use super::super::SshClient;
use crate::project::Project;
use serde::Serialize;
use super::ServiceStatus;
use super::LoadAverage;
use super::DiskUsage;
use super::ServerHealth;
use super::MemoryUsage;


/// Quote a service name for safe use in shell commands.
pub(crate) fn shell_quote_service(name: &str) -> String {
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
pub(crate) fn parse_health_output(output: &str, services: &[String]) -> ServerHealth {
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
pub(crate) fn parse_uptime(line: &str) -> String {
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
pub(crate) fn parse_load_average(line: &str, existing: Option<&LoadAverage>) -> Option<LoadAverage> {
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
pub(crate) fn parse_disk_usage(line: &str) -> Option<DiskUsage> {
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
pub(crate) fn parse_memory_usage(line: &str) -> Option<MemoryUsage> {
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
pub(crate) fn parse_human_bytes(s: &str) -> u64 {
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
pub(crate) fn parse_service_status(line: &str) -> Option<ServiceStatus> {
    let (name, status) = line.split_once(':')?;
    let status_str = status.trim();
    Some(ServiceStatus {
        name: name.trim().to_string(),
        active: status_str == "active",
        status: status_str.to_string(),
    })
}

/// Generate warning messages based on health thresholds.
pub(crate) fn generate_warnings(health: &ServerHealth) -> Vec<String> {
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
