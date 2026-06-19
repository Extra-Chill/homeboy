//! Restart of long-running, binary-resident services after an upgrade.
//!
//! When `homeboy upgrade` swaps the on-disk binary, the CLI process can simply
//! re-exec itself, but any long-running service (e.g. a systemd unit) keeps the
//! *old* binary resident in memory until it is restarted. The set of such
//! services is host/environment-specific, so it is declared entirely in config
//! (`resident_services`) — core hardcodes no service name, unit, or host. See
//! issues #5197 and #5118.

use std::process::Command;

use crate::core::defaults::ResidentServiceConfig;

use super::types::ServiceRestartEntry;

/// Resolve the restart command for a declared service.
///
/// Precedence: an explicit `restart_command` overrides everything; otherwise a
/// `systemd_unit` yields `systemctl restart <unit>`. Returns `None` when the
/// descriptor declares neither — such a descriptor is unrunnable and is
/// surfaced to the operator as pending rather than silently dropped.
pub(super) fn restart_command_for(service: &ResidentServiceConfig) -> Option<String> {
    if let Some(cmd) = service
        .restart_command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        return Some(cmd.to_string());
    }

    service
        .systemd_unit
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(|unit| format!("systemctl restart {}", unit))
}

/// Restart every declared resident service after a successful binary swap.
///
/// Returns `(restarted, pending)`:
/// - `restarted`: services that came back successfully.
/// - `pending`: services that still hold the old binary — either the restart
///   failed, or the descriptor was unrunnable (no unit/command). Failures are
///   surfaced clearly rather than failing the whole upgrade silently.
///
/// `run` executes a restart command and returns `Ok(())` on success or an
/// `Err(detail)` describing the failure. It is injected so the logic can be
/// exercised without spawning processes.
pub(super) fn restart_resident_services<R>(
    services: &[ResidentServiceConfig],
    mut run: R,
) -> (Vec<ServiceRestartEntry>, Vec<ServiceRestartEntry>)
where
    R: FnMut(&str) -> Result<(), String>,
{
    let mut restarted = Vec::new();
    let mut pending = Vec::new();

    for service in services {
        let Some(command) = restart_command_for(service) else {
            pending.push(ServiceRestartEntry {
                service_id: service.id.clone(),
                restart_command: String::new(),
                restarted: false,
                detail: Some(
                    "no systemd_unit or restart_command declared; cannot restart".to_string(),
                ),
            });
            continue;
        };

        match run(&command) {
            Ok(()) => restarted.push(ServiceRestartEntry {
                service_id: service.id.clone(),
                restart_command: command,
                restarted: true,
                detail: None,
            }),
            Err(detail) => pending.push(ServiceRestartEntry {
                service_id: service.id.clone(),
                restart_command: command,
                restarted: false,
                detail: Some(detail),
            }),
        }
    }

    (restarted, pending)
}

/// Build the pending-restart entries for declared services when restarts are
/// skipped entirely (e.g. `--no-restart-services`). Every declared service is
/// reported as pending with its recovery command so the operator knows exactly
/// what still needs restarting.
pub(super) fn pending_when_skipped(services: &[ResidentServiceConfig]) -> Vec<ServiceRestartEntry> {
    services
        .iter()
        .map(|service| {
            let command = restart_command_for(service).unwrap_or_default();
            let detail = if command.is_empty() {
                "no systemd_unit or restart_command declared; cannot restart".to_string()
            } else {
                "restart skipped (--no-restart-services); restart manually".to_string()
            };
            ServiceRestartEntry {
                service_id: service.id.clone(),
                restart_command: command,
                restarted: false,
                detail: Some(detail),
            }
        })
        .collect()
}

/// Run a restart command via `sh -c`, returning `Err(detail)` on a non-zero
/// exit or spawn failure. This is the production runner injected into
/// [`restart_resident_services`].
pub(super) fn run_restart_command(command: &str) -> Result<(), String> {
    let output = Command::new("sh")
        .args(["-c", command])
        .output()
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("exit code {}", output.status.code().unwrap_or(1))
    };
    Err(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(id: &str, unit: Option<&str>, cmd: Option<&str>) -> ResidentServiceConfig {
        ResidentServiceConfig {
            id: id.to_string(),
            systemd_unit: unit.map(str::to_string),
            restart_command: cmd.map(str::to_string),
        }
    }

    #[test]
    fn restart_command_prefers_explicit_command_over_systemd_unit() {
        let svc = service("ingress", Some("homeboy-ingress"), Some("custom restart"));
        assert_eq!(restart_command_for(&svc).as_deref(), Some("custom restart"));
    }

    #[test]
    fn restart_command_falls_back_to_systemctl_for_unit() {
        let svc = service("ingress", Some("homeboy-ingress"), None);
        assert_eq!(
            restart_command_for(&svc).as_deref(),
            Some("systemctl restart homeboy-ingress")
        );
    }

    #[test]
    fn restart_command_is_none_without_unit_or_command() {
        let svc = service("ingress", None, None);
        assert!(restart_command_for(&svc).is_none());
    }

    #[test]
    fn restart_command_ignores_blank_command_and_unit() {
        let svc = service("ingress", Some("   "), Some("  "));
        assert!(restart_command_for(&svc).is_none());
    }

    #[test]
    fn restart_resident_services_records_successes_and_failures() {
        // Service list provided via fixture, not hardcoded in production code.
        let services = vec![
            service("ingress", Some("homeboy-ingress"), None),
            service("broker", Some("homeboy-broker"), None),
            service("custom", None, Some("supervisorctl restart custom")),
        ];

        let (restarted, pending) = restart_resident_services(&services, |cmd| {
            if cmd.contains("homeboy-broker") {
                Err("Unit homeboy-broker not found".to_string())
            } else {
                Ok(())
            }
        });

        assert_eq!(restarted.len(), 2);
        assert!(restarted.iter().all(|e| e.restarted));
        assert!(restarted.iter().any(|e| e.service_id == "ingress"));
        assert!(restarted.iter().any(|e| e.service_id == "custom"));

        assert_eq!(pending.len(), 1);
        let failed = &pending[0];
        assert_eq!(failed.service_id, "broker");
        assert!(!failed.restarted);
        assert_eq!(failed.restart_command, "systemctl restart homeboy-broker");
        assert_eq!(
            failed.detail.as_deref(),
            Some("Unit homeboy-broker not found")
        );
    }

    #[test]
    fn restart_resident_services_marks_unrunnable_descriptor_pending() {
        let services = vec![service("orphan", None, None)];

        let (restarted, pending) = restart_resident_services(&services, |_| Ok(()));

        assert!(restarted.is_empty());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].service_id, "orphan");
        assert!(pending[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("cannot restart"));
    }

    #[test]
    fn restart_resident_services_empty_list_is_noop() {
        let (restarted, pending) = restart_resident_services(&[], |_| Ok(()));
        assert!(restarted.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_when_skipped_reports_all_declared_services() {
        let services = vec![
            service("ingress", Some("homeboy-ingress"), None),
            service("orphan", None, None),
        ];

        let pending = pending_when_skipped(&services);

        assert_eq!(pending.len(), 2);
        let ingress = pending.iter().find(|e| e.service_id == "ingress").unwrap();
        assert_eq!(ingress.restart_command, "systemctl restart homeboy-ingress");
        assert!(ingress.detail.as_deref().unwrap().contains("skipped"));

        let orphan = pending.iter().find(|e| e.service_id == "orphan").unwrap();
        assert!(orphan.restart_command.is_empty());
        assert!(orphan.detail.as_deref().unwrap().contains("cannot restart"));
    }
}
