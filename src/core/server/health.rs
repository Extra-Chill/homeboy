//! Server health metrics collection via SSH.
//!
//! Core primitive used by both `fleet status` and `project status` commands
//! to collect uptime, load, disk, memory, and service health from remote servers.

mod collection;
mod parsing;
mod types;

pub use collection::*;
pub use parsing::*;
pub use types::*;


use super::SshClient;
use crate::project::Project;
use serde::Serialize;

// ============================================================================
// Types
// ============================================================================

// ============================================================================
// Collection
// ============================================================================

// ============================================================================
// Parsing
// ============================================================================

// ============================================================================
// Warnings
// ============================================================================

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
