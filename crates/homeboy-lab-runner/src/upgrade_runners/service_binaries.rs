//! Runner-service binary convergence (#15733).
//!
//! A runner host can run the Lab daemon from systemd user units: one scoped
//! unit per controller (`runner service install`) plus, on hosts installed
//! before controller scoping (#15160), one unscoped legacy unit. Each unit
//! executes a stable link into an immutable `_homeboy_binaries` slot, so
//! upgrading the runner's configured binary does not change what a unit runs.
//!
//! The runner-side `homeboy upgrade` owns neither those slots nor the units,
//! so the owner of a unit's binary is the controller-side runner convergence:
//! this controller's scoped unit is repointed and restarted through the
//! established `refresh-homeboy --reconnect` handoff, which refuses to restart
//! a daemon with active jobs. Every unit is observed after convergence, and a
//! runner whose in-scope unit still runs another version is never reported
//! converged.

use super::*;
use crate::connection::ServiceUnitObservation;
use crate::{Runner, RunnerKind};
use homeboy_core::Result;
use homeboy_upgrade::upgrade::{RunnerServiceBinaryEntry, RunnerUpgradeEntry};

/// Result of asking the established refresh handoff to converge this
/// controller's runner-service unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceRefreshOutcome {
    Refreshed,
    /// The daemon is busy; the refresh was deferred without a restart.
    Deferred(String),
    Failed(String),
}

fn active(state: Option<&str>) -> Option<bool> {
    state.map(|state| matches!(state, "active" | "activating" | "reloading"))
}

/// Pure classification of observed units against `target_version`.
pub(crate) fn classify_service_binaries(
    runner_id: &str,
    homeboy_path: &str,
    observations: &[ServiceUnitObservation],
    scope_of: &impl Fn(&str) -> Option<&'static str>,
    target_version: &str,
) -> Vec<RunnerServiceBinaryEntry> {
    observations
        .iter()
        .filter_map(|observation| {
            let scope = scope_of(&observation.unit)?;
            let binary_version = observation
                .binary_version
                .as_deref()
                .and_then(parse_cli_version_output);
            let running_version = observation
                .running_version
                .as_deref()
                .and_then(parse_cli_version_output);
            let (status, reason) = match active(observation.active_state.as_deref()) {
                Some(false) => (
                    "inactive",
                    Some(format!(
                        "unit is {}; it runs no daemon",
                        observation.active_state.as_deref().unwrap_or("inactive")
                    )),
                ),
                _ => match (running_version.as_deref(), binary_version.as_deref()) {
                    (None, None) => (
                        "unknown",
                        Some(
                            "neither the unit binary nor its running process reported a version"
                                .to_string(),
                        ),
                    ),
                    (running, binary)
                        if running.is_some_and(|version| version != target_version)
                            || binary.is_some_and(|version| version != target_version) =>
                    {
                        (
                            "stale",
                            Some(format!(
                                "unit runs {} from {} (link resolves to {}); target is {target_version}",
                                running.unwrap_or("an unverified version"),
                                observation.binary,
                                binary.unwrap_or("an unverified version"),
                            )),
                        )
                    }
                    _ => ("current", None),
                },
            };
            let mut entry = RunnerServiceBinaryEntry {
                unit: observation.unit.clone(),
                scope: scope.to_string(),
                binary: observation.binary.clone(),
                binary_version,
                running_version,
                active_state: observation.active_state.clone(),
                target_version: target_version.to_string(),
                status: status.to_string(),
                reason,
                recovery_commands: Vec::new(),
            };
            attach_recovery(runner_id, homeboy_path, &mut entry);
            Some(entry)
        })
        .collect()
}

fn attach_recovery(runner_id: &str, homeboy_path: &str, entry: &mut RunnerServiceBinaryEntry) {
    if !entry.blocks_convergence() {
        entry.recovery_commands.clear();
        return;
    }
    let runner = shell_arg(runner_id);
    entry.recovery_commands = match entry.scope.as_str() {
        // The established idle-gated handoff: it refuses to restart a daemon
        // with active jobs and defers instead.
        "controller" => vec![format!(
            "homeboy runner refresh-homeboy {runner} --select {} --reconnect",
            shell_arg(homeboy_path)
        )],
        // No controller owns the unscoped unit, so nothing repoints it. Retire
        // it once its daemon is drained; scoped units replace it.
        "legacy" => {
            if !entry
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("legacy"))
            {
                let detail = entry.reason.take().unwrap_or_default();
                entry.reason = Some(format!(
                    "{detail}; legacy unscoped unit predates controller-scoped runner services and no controller refreshes it"
                ));
            }
            vec![
                format!(
                    "homeboy runner exec {runner} -- sh -c {}",
                    shell_arg(&format!(
                        "HOMEBOY_DAEMON_STATE_DIR=\"$HOME/.config/homeboy/daemon-generations/{}/primary\" {} daemon status",
                        homeboy_core::paths::sanitize_path_segment(runner_id),
                        shell_arg(&entry.binary),
                    ))
                ),
                format!(
                    "homeboy runner exec {runner} -- systemctl --user disable --now {}",
                    shell_arg(&entry.unit)
                ),
            ]
        }
        _ => Vec::new(),
    };
}

fn shell_arg(value: &str) -> String {
    homeboy_core::engine::shell::quote_arg(value)
}

fn controller_unit<'a>(
    entries: &'a mut [RunnerServiceBinaryEntry],
) -> Option<&'a mut RunnerServiceBinaryEntry> {
    entries.iter_mut().find(|entry| entry.scope == "controller")
}

/// Observe the runner's service units after convergence, refresh this
/// controller's stale unit through the idle-gated handoff when the runner is
/// service-managed, and fold any in-scope stale unit into the entry so it is
/// never reported converged.
pub(crate) fn converge_runner_service_binaries(
    runner: &Runner,
    mut entry: RunnerUpgradeEntry,
    observe: &mut impl FnMut(&str) -> Result<Vec<ServiceUnitObservation>>,
    scope_of: &impl Fn(&str) -> Option<&'static str>,
    controller_unit_name: &str,
    refresh: &mut impl FnMut(&Runner, &str) -> ServiceRefreshOutcome,
) -> RunnerUpgradeEntry {
    let Some(target) = entry.new_version.clone() else {
        return entry;
    };
    let service_managed = runner.settings.service_managed;
    let observe_and_classify =
        |observe: &mut dyn FnMut(&str) -> Result<Vec<ServiceUnitObservation>>| {
            observe(&runner.id).map(|observations| {
                classify_service_binaries(
                    &runner.id,
                    &entry.homeboy_path,
                    &observations,
                    scope_of,
                    &target,
                )
            })
        };
    let mut services = match observe_and_classify(&mut *observe) {
        Ok(services) => services,
        Err(error) if service_managed => vec![RunnerServiceBinaryEntry {
            unit: controller_unit_name.to_string(),
            scope: "controller".to_string(),
            binary: String::new(),
            binary_version: None,
            running_version: None,
            active_state: None,
            target_version: target.clone(),
            status: "unknown".to_string(),
            reason: Some(format!(
                "runner service inventory failed: {}",
                error.message
            )),
            recovery_commands: Vec::new(),
        }],
        Err(error) => {
            entry.detail = format!(
                "{}; runner service inventory unavailable: {}",
                entry.detail, error.message
            );
            return entry;
        }
    };
    if service_managed && controller_unit(&mut services).is_none() {
        services.push(RunnerServiceBinaryEntry {
            unit: controller_unit_name.to_string(),
            scope: "controller".to_string(),
            binary: String::new(),
            binary_version: None,
            running_version: None,
            active_state: None,
            target_version: target.clone(),
            status: "unknown".to_string(),
            reason: Some(
                "runner is service-managed but this controller's unit was not found".to_string(),
            ),
            recovery_commands: Vec::new(),
        });
    }

    let controller_stale = controller_unit(&mut services)
        .is_some_and(|unit| unit.status == "stale" && !unit.binary.is_empty());
    if service_managed && controller_stale {
        match refresh(runner, &entry.homeboy_path) {
            ServiceRefreshOutcome::Refreshed => match observe_and_classify(&mut *observe) {
                Ok(mut refreshed) => {
                    if let Some(unit) = controller_unit(&mut refreshed) {
                        if unit.status == "current" {
                            unit.status = "refreshed".to_string();
                        } else {
                            let reason = unit.reason.take().unwrap_or_default();
                            unit.reason = Some(format!(
                                "refresh-homeboy --reconnect completed but the unit is still {}: {reason}",
                                unit.status
                            ));
                        }
                    }
                    services = refreshed;
                }
                Err(error) => {
                    if let Some(unit) = controller_unit(&mut services) {
                        unit.status = "unknown".to_string();
                        unit.reason = Some(format!(
                            "unit was refreshed but could not be re-observed: {}",
                            error.message
                        ));
                    }
                }
            },
            ServiceRefreshOutcome::Deferred(reason) => {
                if let Some(unit) = controller_unit(&mut services) {
                    unit.status = "pending".to_string();
                    unit.reason = Some(format!("{} not restarted: {reason}", unit.unit));
                }
            }
            ServiceRefreshOutcome::Failed(reason) => {
                if let Some(unit) = controller_unit(&mut services) {
                    let stale = unit.reason.take().unwrap_or_default();
                    unit.reason = Some(format!("{stale}; refresh failed: {reason}"));
                }
            }
        }
    }
    for unit in services.iter_mut() {
        attach_recovery(&runner.id, &entry.homeboy_path, unit);
    }

    let blocking = services
        .iter()
        .filter(|unit| unit.blocks_convergence())
        .collect::<Vec<_>>();
    if !blocking.is_empty() {
        let summary = blocking
            .iter()
            .map(|unit| {
                format!(
                    "{} ({} {}: {})",
                    unit.unit,
                    unit.scope,
                    unit.status,
                    unit.reason.as_deref().unwrap_or("no detail")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        homeboy_core::log_status!(
            "upgrade",
            "  {} runner service not converged: {}",
            entry.runner_id,
            summary
        );
        entry.success = false;
        if entry.exit_code == 0 {
            entry.exit_code = 1;
        }
        if entry.path_drift.is_none() {
            entry.path_drift = Some(format!("runner service binary not converged: {summary}"));
        }
        for command in blocking
            .iter()
            .flat_map(|unit| unit.recovery_commands.iter())
        {
            if !entry.recovery_commands.contains(command) {
                entry.recovery_commands.push(command.clone());
            }
        }
        entry.detail = format!("{}; runner service not converged: {summary}", entry.detail);
    }
    entry.service_binaries = services;
    entry
}

/// Production refresh: the `refresh-homeboy --select <path> --reconnect`
/// handoff, never forced, so active jobs defer it instead of being killed.
pub(crate) fn refresh_runner_service(runner: &Runner, homeboy_path: &str) -> ServiceRefreshOutcome {
    let options = crate::HomeboyBinaryRefreshOptions {
        runner_id: runner.id.clone(),
        mode: crate::HomeboyBinaryRefreshMode::Select {
            binary_path: homeboy_path.to_string(),
        },
        source: None,
        git_ref: None,
        target_dir: None,
        reconnect: true,
        force: false,
        allow_downgrade: false,
        dry_run: false,
    };
    match crate::refresh_homeboy_binary(options) {
        Ok((output, _)) if output.reconnect_deferred.is_some() => {
            let deferred = output.reconnect_deferred.expect("checked above");
            ServiceRefreshOutcome::Deferred(format!(
                "{} ({} active job(s): {})",
                deferred.reason,
                deferred.active_job_ids.len(),
                deferred.active_job_ids.join(", ")
            ))
        }
        Ok((output, 0)) if output.failure.is_none() => ServiceRefreshOutcome::Refreshed,
        Ok((output, exit_code)) => ServiceRefreshOutcome::Failed(format!(
            "refresh-homeboy exited {exit_code}: {}",
            output
                .failure
                .map(|failure| format!("{failure:?}"))
                .unwrap_or_else(|| "no failure detail".to_string())
        )),
        Err(error) => ServiceRefreshOutcome::Failed(error.message),
    }
}

/// Production post-processing for `upgrade --upgrade-runner`: apply service
/// convergence to every entry and re-split by success.
pub(crate) fn converge_service_binaries_for_entries(
    runners: &[Runner],
    updated: Vec<RunnerUpgradeEntry>,
    skipped: Vec<RunnerUpgradeEntry>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    converge_service_binaries_for_entries_with(
        runners,
        updated,
        skipped,
        &mut crate::connection::observe_service_units,
        &mut refresh_runner_service,
    )
}

pub(crate) fn converge_service_binaries_for_entries_with(
    runners: &[Runner],
    updated: Vec<RunnerUpgradeEntry>,
    skipped: Vec<RunnerUpgradeEntry>,
    observe: &mut impl FnMut(&str) -> Result<Vec<ServiceUnitObservation>>,
    refresh: &mut impl FnMut(&Runner, &str) -> ServiceRefreshOutcome,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let mut converged = Vec::new();
    let mut not_converged = Vec::new();
    for entry in updated.into_iter().chain(skipped) {
        let entry = match runners
            .iter()
            .find(|runner| runner.id == entry.runner_id && runner.kind == RunnerKind::Ssh)
        {
            Some(runner) => {
                let scope_of = |unit: &str| crate::connection::service_unit_scope(&runner.id, unit);
                converge_runner_service_binaries(
                    runner,
                    entry,
                    observe,
                    &scope_of,
                    &crate::connection::runner_service_unit_name(&runner.id),
                    refresh,
                )
            }
            None => entry,
        };
        if entry.success {
            converged.push(entry);
        } else {
            not_converged.push(entry);
        }
    }
    (converged, not_converged)
}

/// Read-only `upgrade --check` view of the selected runners.
pub fn check_configured_runners(
    runner_targets: &[String],
    target_version: &str,
) -> Result<Vec<homeboy_upgrade::upgrade::RunnerCheckEntry>> {
    let runners = runner_upgrade_targets(runner_targets)?;
    Ok(runners
        .iter()
        .map(|runner| {
            let scope_of = |unit: &str| crate::connection::service_unit_scope(&runner.id, unit);
            check_runner_with(
                runner,
                target_version,
                &mut crate::connection::probe_runner_binary_version,
                &mut crate::connection::observe_service_units,
                &scope_of,
            )
        })
        .collect())
}

pub(crate) fn check_runner_with(
    runner: &Runner,
    target_version: &str,
    probe_selected: &mut impl FnMut(&Runner, &str) -> Result<Option<String>>,
    observe: &mut impl FnMut(&str) -> Result<Vec<ServiceUnitObservation>>,
    scope_of: &impl Fn(&str) -> Option<&'static str>,
) -> homeboy_upgrade::upgrade::RunnerCheckEntry {
    let homeboy_path = runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let mut errors = Vec::new();
    let selected_binary_version = match probe_selected(runner, &homeboy_path) {
        Ok(line) => line.as_deref().and_then(parse_cli_version_output),
        Err(error) => {
            errors.push(format!("selected binary probe failed: {}", error.message));
            None
        }
    };
    let selected_binary_status = match selected_binary_version.as_deref() {
        Some(version) if version == target_version => "current",
        Some(_) => "stale",
        None => "unknown",
    };
    let service_binaries = if runner.kind == RunnerKind::Ssh {
        match observe(&runner.id) {
            Ok(observations) => classify_service_binaries(
                &runner.id,
                &homeboy_path,
                &observations,
                scope_of,
                target_version,
            ),
            Err(error) => {
                errors.push(format!(
                    "runner service inventory failed: {}",
                    error.message
                ));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let converged = selected_binary_status == "current"
        && errors.is_empty()
        && !service_binaries
            .iter()
            .any(|unit| unit.blocks_convergence());
    homeboy_upgrade::upgrade::RunnerCheckEntry {
        runner_id: runner.id.clone(),
        homeboy_path,
        target_version: target_version.to_string(),
        selected_binary_version,
        selected_binary_status: selected_binary_status.to_string(),
        service_binaries,
        converged,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "homeboy-runner-homeboy-lab-own.service";
    const LEGACY: &str = "homeboy-runner-homeboy-lab.service";
    const OTHER: &str = "homeboy-runner-homeboy-lab-other.service";

    fn scope(unit: &str) -> Option<&'static str> {
        match unit {
            OWN => Some("controller"),
            LEGACY => Some("legacy"),
            OTHER => Some("other_controller"),
            _ => None,
        }
    }

    fn unit(name: &str, version: &str) -> ServiceUnitObservation {
        ServiceUnitObservation {
            unit: name.to_string(),
            binary: format!("/home/u/.local/share/homeboy/runner-service/{name}/homeboy"),
            binary_version: Some(format!("homeboy {version}+abcdef0")),
            running_version: Some(format!("homeboy {version}+abcdef0")),
            active_state: Some("active".to_string()),
        }
    }

    fn converged_entry(runner_id: &str, version: &str) -> RunnerUpgradeEntry {
        RunnerUpgradeEntry {
            runner_id: runner_id.to_string(),
            homeboy_path: "/home/u/Developer/_homeboy_binaries/homeboy-new/homeboy".to_string(),
            success: true,
            upgraded: true,
            previous_version: Some("0.417.14".to_string()),
            new_version: Some(version.to_string()),
            bare_homeboy_version: None,
            path_drift: None,
            recovery_commands: Vec::new(),
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            service_binaries: Vec::new(),
            exit_code: 0,
            detail: "runner upgraded".to_string(),
        }
    }

    fn runner(service_managed: bool) -> Runner {
        let mut runner =
            super::super::tests::ssh_runner("homeboy-lab", Some("/home/u/.local/bin/homeboy"));
        runner.settings.service_managed = service_managed;
        runner
    }

    fn never_refresh(_: &Runner, _: &str) -> ServiceRefreshOutcome {
        panic!("a non-stale or unowned unit must not be refreshed")
    }

    /// #15733 defect 2: the legacy unscoped unit still ran 0.395.1 while the
    /// controller reported `runners.status=converged`. A stale in-scope service
    /// binary must keep the runner out of `converged` and name the unit.
    #[test]
    fn stale_legacy_unit_blocks_convergence_with_remediation() {
        let entry = converge_runner_service_binaries(
            &runner(false),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(vec![unit(LEGACY, "0.395.1"), unit(OTHER, "0.399.21")]),
            &scope,
            OWN,
            &mut never_refresh,
        );

        assert!(!entry.success, "{entry:?}");
        assert_eq!(entry.exit_code, 1);
        let drift = entry.path_drift.as_deref().unwrap();
        assert!(drift.contains(LEGACY), "{drift}");
        assert!(drift.contains("0.395.1"), "{drift}");
        let legacy = entry
            .service_binaries
            .iter()
            .find(|unit| unit.unit == LEGACY)
            .unwrap();
        assert_eq!(legacy.status, "stale");
        assert_eq!(legacy.running_version.as_deref(), Some("0.395.1"));
        assert!(legacy
            .recovery_commands
            .iter()
            .any(|command| command.contains("disable --now") && command.contains(LEGACY)));
        assert!(entry
            .recovery_commands
            .iter()
            .any(|command| command.contains(LEGACY)));
        // Another controller's stale unit is reported but not ours to converge.
        let other = entry
            .service_binaries
            .iter()
            .find(|unit| unit.unit == OTHER)
            .unwrap();
        assert_eq!(other.status, "stale");
        assert!(!other.blocks_convergence());
        assert!(other.recovery_commands.is_empty());
    }

    #[test]
    fn only_other_controllers_stale_units_leave_the_runner_converged() {
        let entry = converge_runner_service_binaries(
            &runner(false),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(vec![unit(OTHER, "0.399.21")]),
            &scope,
            OWN,
            &mut never_refresh,
        );

        assert!(entry.success);
        assert!(entry.path_drift.is_none());
        assert_eq!(entry.service_binaries.len(), 1);
    }

    #[test]
    fn stale_running_process_with_current_link_is_still_stale() {
        let mut observed = unit(LEGACY, "0.417.15");
        observed.running_version = Some("homeboy 0.395.1+5171701".to_string());
        let services =
            classify_service_binaries("homeboy-lab", "/b", &[observed], &scope, "0.417.15");
        assert_eq!(services[0].status, "stale");
        assert!(services[0].reason.as_deref().unwrap().contains("0.395.1"));
    }

    #[test]
    fn inactive_units_do_not_block_and_unversioned_active_units_do() {
        let mut inactive = unit(LEGACY, "0.395.1");
        inactive.active_state = Some("inactive".to_string());
        let mut unversioned = unit(OWN, "0.417.15");
        unversioned.binary_version = None;
        unversioned.running_version = None;
        let services = classify_service_binaries(
            "homeboy-lab",
            "/b",
            &[inactive, unversioned],
            &scope,
            "0.417.15",
        );
        assert_eq!(services[0].status, "inactive");
        assert!(!services[0].blocks_convergence());
        assert_eq!(services[1].status, "unknown");
        assert!(services[1].blocks_convergence());
    }

    /// The owned unit is refreshed through the idle-gated refresh handoff,
    /// re-observed, and only then counted converged.
    #[test]
    fn stale_own_unit_is_refreshed_through_the_handoff_and_reobserved() {
        let mut observations =
            vec![vec![unit(OWN, "0.417.14")], vec![unit(OWN, "0.417.15")]].into_iter();
        let mut refreshed_paths = Vec::new();
        let entry = converge_runner_service_binaries(
            &runner(true),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(observations.next().expect("observed at most twice")),
            &scope,
            OWN,
            &mut |runner, path| {
                refreshed_paths.push((runner.id.clone(), path.to_string()));
                ServiceRefreshOutcome::Refreshed
            },
        );

        assert!(entry.success, "{entry:?}");
        assert_eq!(entry.service_binaries[0].status, "refreshed");
        assert_eq!(
            refreshed_paths,
            vec![(
                "homeboy-lab".to_string(),
                "/home/u/Developer/_homeboy_binaries/homeboy-new/homeboy".to_string()
            )]
        );
    }

    /// A busy daemon is never restarted: the handoff defers and the unit is
    /// reported `pending`, naming the unit and the reason.
    #[test]
    fn busy_own_unit_is_pending_not_converged() {
        let entry = converge_runner_service_binaries(
            &runner(true),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(vec![unit(OWN, "0.417.14")]),
            &scope,
            OWN,
            &mut |_, _| {
                ServiceRefreshOutcome::Deferred(
                    "active_daemon_jobs (1 active job(s): job-1)".to_string(),
                )
            },
        );

        assert!(!entry.success);
        let own = &entry.service_binaries[0];
        assert_eq!(own.status, "pending");
        let reason = own.reason.as_deref().unwrap();
        assert!(reason.contains(OWN) && reason.contains("job-1"), "{reason}");
        assert!(own.recovery_commands[0].contains("refresh-homeboy"));
    }

    #[test]
    fn refresh_that_does_not_converge_the_unit_stays_blocking() {
        let entry = converge_runner_service_binaries(
            &runner(true),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(vec![unit(OWN, "0.417.14")]),
            &scope,
            OWN,
            &mut |_, _| ServiceRefreshOutcome::Refreshed,
        );

        assert!(!entry.success);
        assert_eq!(entry.service_binaries[0].status, "stale");
    }

    #[test]
    fn service_managed_runner_without_its_unit_is_not_converged() {
        let entry = converge_runner_service_binaries(
            &runner(true),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Ok(Vec::new()),
            &scope,
            OWN,
            &mut never_refresh,
        );
        assert!(!entry.success);
        assert_eq!(entry.service_binaries[0].unit, OWN);
        assert_eq!(entry.service_binaries[0].status, "unknown");
    }

    #[test]
    fn inventory_failure_is_not_a_claim_for_unmanaged_runners() {
        let entry = converge_runner_service_binaries(
            &runner(false),
            converged_entry("homeboy-lab", "0.417.15"),
            &mut |_| Err(homeboy_core::error::Error::internal_unexpected("ssh down")),
            &scope,
            OWN,
            &mut never_refresh,
        );
        assert!(entry.success);
        assert!(entry.detail.contains("inventory unavailable: ssh down"));
    }

    #[test]
    fn entries_with_stale_services_move_from_converged_to_not_converged() {
        let runners = vec![runner(false)];
        let (updated, skipped) = converge_service_binaries_for_entries_with(
            &runners,
            vec![converged_entry("homeboy-lab", "0.417.15")],
            Vec::new(),
            &mut |_| Ok(vec![unit("homeboy-runner-homeboy-lab.service", "0.395.1")]),
            &mut never_refresh,
        );
        assert!(updated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert!(!skipped[0].success);
    }

    /// #15733 defect 3: `--check` with runner selection reports the selected
    /// binary and every service binary against the target, read-only.
    #[test]
    fn check_reports_selected_and_service_binary_versions() {
        let runner = runner(false);
        let mut probed = Vec::new();
        let check = check_runner_with(
            &runner,
            "0.417.15",
            &mut |runner, path| {
                probed.push((runner.id.clone(), path.to_string()));
                Ok(Some("homeboy 0.417.15+708e00a".to_string()))
            },
            &mut |_| Ok(vec![unit(LEGACY, "0.395.1"), unit(OWN, "0.417.15")]),
            &scope,
        );

        assert_eq!(
            probed,
            vec![(
                "homeboy-lab".to_string(),
                "/home/u/.local/bin/homeboy".to_string()
            )]
        );
        assert_eq!(check.selected_binary_version.as_deref(), Some("0.417.15"));
        assert_eq!(check.selected_binary_status, "current");
        assert!(!check.converged, "a stale legacy unit is not converged");
        let legacy = check
            .service_binaries
            .iter()
            .find(|unit| unit.unit == LEGACY)
            .unwrap();
        assert_eq!(legacy.status, "stale");
        assert_eq!(legacy.running_version.as_deref(), Some("0.395.1"));
        let own = check
            .service_binaries
            .iter()
            .find(|unit| unit.unit == OWN)
            .unwrap();
        assert_eq!(own.status, "current");
    }

    #[test]
    fn check_reports_a_stale_selected_binary() {
        let check = check_runner_with(
            &runner(false),
            "0.417.15",
            &mut |_, _| Ok(Some("homeboy 0.417.14+bf3743f".to_string())),
            &mut |_| Ok(Vec::new()),
            &scope,
        );
        assert_eq!(check.selected_binary_status, "stale");
        assert!(!check.converged);
        assert!(check.error.is_none());
    }
}
