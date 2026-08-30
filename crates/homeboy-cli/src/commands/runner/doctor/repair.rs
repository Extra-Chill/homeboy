use super::*;
use types::{RunnerDoctorOutput, RunnerDoctorStatus, RunnerRepair};

pub fn apply(
    target: &target::RunnerTarget,
    options: &RunnerDoctorOptions,
    report: &mut RunnerDoctorOutput,
) {
    if options.scope == RunnerDoctorScope::SecretEnv {
        match runner::apply_secret_env_migration(&report.runner_id) {
            Ok(plan) => report.repairs.push(RunnerRepair {
                id: "repair.secret_env_migration".to_string(),
                status: RunnerDoctorStatus::Ok,
                message: format!(
                    "Migrated {} runner env entries into OS keychain references",
                    plan.entries.len()
                ),
                commands: Vec::new(),
            }),
            Err(error) => report.repairs.push(RunnerRepair {
                id: "repair.secret_env_migration".to_string(),
                status: RunnerDoctorStatus::Error,
                message: error.message,
                commands: Vec::new(),
            }),
        }
        return;
    }

    if options.scope != RunnerDoctorScope::LabOffload {
        report.repairs.push(RunnerRepair {
            id: "repair.scope".to_string(),
            status: RunnerDoctorStatus::Warning,
            message:
                "No repairs were applied because --repair is only active for --scope lab-offload"
                    .to_string(),
            commands: Vec::new(),
        });
        return;
    }

    let target::RunnerTarget::Ssh {
        id,
        runner: runner_config,
        client,
        ..
    } = target
    else {
        report.repairs.push(RunnerRepair {
            id: "repair.runner".to_string(),
            status: RunnerDoctorStatus::Warning,
            message: "No Lab daemon repair is available for local runner targets".to_string(),
            commands: Vec::new(),
        });
        return;
    };

    repair_managed_sources(client, report);

    if let Some(repair) = terminal_daemon_recovery_repair(report) {
        report.repairs.push(repair);
        return;
    }

    // The report already carries a lease-specific, controller-frame plan.
    // Executing it is the whole point of `--repair`; the fixed
    // disconnect/connect pair below is the fallback for a connected daemon whose
    // exec probe failed without any typed plan behind it (#11103).
    if apply_daemon_repair_plan(id, report) {
        return;
    }

    let daemon_failed = report
        .checks
        .iter()
        .any(|check| check.id == "daemon.exec" && check.status == RunnerDoctorStatus::Error);
    let daemon_admission_closed = daemon_admission_ready(id)
        .map(|ready| !ready)
        .unwrap_or(false);
    if !daemon_failed && !daemon_admission_closed {
        report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Ok,
            message: "Connected Lab daemon did not require repair".to_string(),
            commands: Vec::new(),
        });
        return;
    }

    let commands = vec![
        format!("homeboy runner disconnect {id}"),
        format!("homeboy runner connect {id}"),
    ];
    let disconnect_error = runner::disconnect(id).err();
    // Connect owns lease-safe dead-daemon adoption. A failed disconnect must not
    // force operators through repeated stop/adopt cycles when its authoritative
    // probe has already established that the recorded owner is gone.
    match runner::connect(id) {
        Ok((_, 0)) => {
            report.checks.retain(|check| check.id != "daemon.exec");
            let workspace_root = runner_config.workspace_root.as_deref().unwrap_or(".");
            report
                .checks
                .extend(probes::connected_daemon_exec_checks(id, workspace_root));
            let (status, message) = match daemon_admission_ready(id) {
                Ok(true) => (
                    RunnerDoctorStatus::Ok,
                    match disconnect_error {
                        Some(error) => format!(
                            "Recovered the Lab runner daemon after bounded disconnect failed ({}) and reran the daemon exec probe",
                            error.message
                        ),
                        None => {
                            "Reconnected the Lab runner daemon and reran the daemon exec probe"
                                .to_string()
                        }
                    },
                ),
                Ok(false) => (
                    RunnerDoctorStatus::Error,
                    "Reconnected the Lab runner daemon, but Lab admission remains closed".to_string(),
                ),
                Err(error) => (
                    RunnerDoctorStatus::Error,
                    format!(
                        "Reconnected the Lab runner daemon, but could not verify Lab admission: {}",
                        error.message
                    ),
                ),
            };
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status,
                message,
                commands,
            });
        }
        Ok((connect_report, exit_code)) => {
            let failure = connect_report
                .failure_message
                .unwrap_or_else(|| format!("runner connect exited with code {exit_code}"));
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: match disconnect_error {
                    Some(disconnect_error) => format!(
                        "Could not recover Lab daemon after bounded disconnect failed ({}): {}",
                        disconnect_error.message, failure
                    ),
                    None => format!("Could not reconnect Lab daemon: {failure}"),
                },
                commands,
            });
        }
        Err(err) => {
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: match disconnect_error {
                    Some(disconnect_error) => format!(
                        "Could not recover Lab daemon after bounded disconnect failed ({}): {}",
                        disconnect_error.message, err.message
                    ),
                    None => format!("Could not reconnect Lab daemon: {}", err.message),
                },
                commands,
            });
        }
    }
}

fn terminal_daemon_recovery_blocker(report: &RunnerDoctorOutput) -> bool {
    report
        .daemon_recovery
        .as_ref()
        .is_some_and(|recovery| recovery.blocks_automatic_recovery())
}

fn terminal_daemon_recovery_repair(report: &RunnerDoctorOutput) -> Option<RunnerRepair> {
    terminal_daemon_recovery_blocker(report).then(|| RunnerRepair {
        id: "repair.daemon".to_string(),
        status: RunnerDoctorStatus::Warning,
        message: format!(
            "No automatic daemon repair was applied because the current daemon evidence authorizes no recovery: {}",
            report
                .daemon_recovery
                .as_ref()
                .and_then(|recovery| recovery.ownership_evidence.as_deref())
                .unwrap_or("no ownership evidence was recorded")
        ),
        commands: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::runner::doctor::types::{
        RunnerCapabilities, RunnerResources, RunnerTargetSummary,
    };

    #[test]
    fn unavailable_lease_missing_ownership_with_a_plan_is_terminal() {
        for recovery_evidence in [Some(
            homeboy::core::daemon::DaemonRecoveryEvidence::Unavailable,
        )] {
            let mut report = RunnerDoctorOutput {
                variant: "doctor",
                command: "runner.doctor",
                runner_id: "lab".to_string(),
                runner: RunnerTargetSummary {
                    target_type: "ssh",
                    registry: None,
                    server: None,
                },
                status: RunnerDoctorStatus::Error,
                capabilities: RunnerCapabilities::default(),
                resources: RunnerResources::default(),
                checks: Vec::new(),
                secret_env_migration: None,
                diagnostics: None,
                daemon_recovery: Some(homeboy::core::daemon::DaemonFreshnessReport {
                    fresh: false,
                    stale_reason_code: Some(
                        homeboy::core::daemon::DaemonStaleReasonCode::LeaseMissing,
                    ),
                    restartable: false,
                    lease_id: None,
                    pid: None,
                    recovery_evidence,
                    ownership_evidence: Some("remote lease ownership unavailable".to_string()),
                    adoption_command: None,
                    binary_hash: None,
                    daemon_version: Some("0.349.0".to_string()),
                    daemon_build_identity: Some("homeboy 0.349.0+incompatible".to_string()),
                    runtime_paths: None,
                    active_jobs: 0,
                    termination_evidence: None,
                    repair_plan: vec![homeboy::core::daemon::DaemonRepairStep::text(
                        "runner_reconcile_leaseless_orphans",
                        "homeboy runner connect lab --reconcile-leaseless-orphans",
                    )],
                }),
                admission_summary: None,
                provider_readiness: None,
                repairs: Vec::new(),
            };

            assert!(
                !apply_daemon_repair_plan("lab", &mut report),
                "unavailable ownership must reject a non-empty reconciliation plan"
            );
            let repair = terminal_daemon_recovery_repair(&report).expect("terminal repair result");

            assert_eq!(repair.status, RunnerDoctorStatus::Warning);
            assert!(repair.commands.is_empty());
            assert!(repair
                .message
                .contains("remote lease ownership unavailable"));
        }
    }

    #[test]
    fn stale_typed_proof_without_a_plan_is_terminal_before_generic_reconnect() {
        let report = RunnerDoctorOutput {
            variant: "doctor",
            command: "runner.doctor",
            runner_id: "lab".to_string(),
            runner: RunnerTargetSummary {
                target_type: "ssh",
                registry: None,
                server: None,
            },
            status: RunnerDoctorStatus::Error,
            capabilities: RunnerCapabilities::default(),
            resources: RunnerResources::default(),
            checks: Vec::new(),
            secret_env_migration: None,
            diagnostics: None,
            daemon_recovery: Some(homeboy::core::daemon::DaemonFreshnessReport {
                fresh: false,
                stale_reason_code: Some(
                    homeboy::core::daemon::DaemonStaleReasonCode::LeaseSchemaMismatch,
                ),
                restartable: true,
                lease_id: None,
                pid: None,
                recovery_evidence: Some(homeboy::core::daemon::DaemonRecoveryEvidence::Recoverable),
                ownership_evidence: Some("no validated recovery action".to_string()),
                adoption_command: None,
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }),
            admission_summary: None,
            provider_readiness: None,
            repairs: Vec::new(),
        };

        let repair = terminal_daemon_recovery_repair(&report).expect("terminal repair result");
        assert!(repair.commands.is_empty());
        assert!(repair.message.contains("no validated recovery action"));
    }
}

/// What an executor should do about one typed repair step.
///
/// Dispatch is a pure function of the step's code plus typed evidence from the
/// report, so the decision is testable without a runner, an SSH transport, or a
/// daemon. The rendered `step.command` is never parsed back into arguments —
/// that is the drift the codes exist to prevent (#11103).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DaemonRepairDispatch {
    Disconnect,
    Connect,
    AdoptOrphanLease {
        lease_id: String,
    },
    ReconcileLeaselessOrphans,
    ReconcileUnleasedCandidates,
    RefreshHomeboy {
        git_ref: Option<String>,
        allow_downgrade: bool,
    },
    /// Nothing may be executed for this step, and the reason is explicit.
    NotAutomatable {
        reason: String,
    },
}

pub(super) fn dispatch_for(
    step: &homeboy::core::daemon::DaemonRepairStep,
    lease_id: Option<&str>,
) -> DaemonRepairDispatch {
    match step.code.as_str() {
        code if code == daemon_repair_codes::RUNNER_DISCONNECT => DaemonRepairDispatch::Disconnect,
        code if code == daemon_repair_codes::RUNNER_CONNECT => DaemonRepairDispatch::Connect,
        code if code == daemon_repair_codes::RUNNER_ADOPT_ORPHAN_LEASE => {
            // The lease is read off the report, never scraped out of the
            // rendered command. Without one there is nothing to adopt.
            match lease_id {
                Some(lease_id) => DaemonRepairDispatch::AdoptOrphanLease {
                    lease_id: lease_id.to_string(),
                },
                None => DaemonRepairDispatch::NotAutomatable {
                    reason: "the report named an orphan-lease adoption but carried no lease id"
                        .to_string(),
                },
            }
        }
        code if code == daemon_repair_codes::RUNNER_RECONCILE_LEASELESS_ORPHANS => {
            DaemonRepairDispatch::ReconcileLeaselessOrphans
        }
        code if code == daemon_repair_codes::RUNNER_RECONCILE_UNLEASED_CANDIDATES => {
            DaemonRepairDispatch::ReconcileUnleasedCandidates
        }
        code if code == daemon_repair_codes::RUNNER_REFRESH_HOMEBOY => match step.action.as_ref() {
            // The recovery ref is a property of the plan rather than of the
            // daemon's lease, so it is the one value taken from the step's argv.
            Some(action) => DaemonRepairDispatch::RefreshHomeboy {
                git_ref: action
                    .args
                    .iter()
                    .position(|arg| arg == "--ref")
                    .and_then(|index| action.args.get(index + 1))
                    .cloned(),
                allow_downgrade: action.args.iter().any(|arg| arg == "--allow-downgrade"),
            },
            None => DaemonRepairDispatch::NotAutomatable {
                reason: "the refresh step carried no executable arguments".to_string(),
            },
        },
        code if code == daemon_repair_codes::RUNNER_DIAGNOSE => {
            DaemonRepairDispatch::NotAutomatable {
                reason: "the report authorizes no mutation; the diagnostic step is for the operator to run".to_string(),
            }
        }
        code => DaemonRepairDispatch::NotAutomatable {
            reason: format!("repair step `{code}` carries no executable dispatch"),
        },
    }
}

/// Execute the typed repair plan the freshness report already computed.
///
/// Returns `false` when there is no plan to act on, so the caller falls through
/// to its own probe-driven recovery.
fn apply_daemon_repair_plan(runner_id: &str, report: &mut RunnerDoctorOutput) -> bool {
    let Some(recovery) = report.daemon_recovery.as_ref() else {
        return false;
    };
    if recovery.repair_plan.is_empty() || !recovery.has_recovery_ownership_proof() {
        return false;
    }

    let plan = recovery.repair_plan.clone();
    let lease_id = recovery.lease_id.clone();
    let commands: Vec<String> = plan.iter().map(|step| step.command.clone()).collect();

    let mut applied = 0usize;
    for step in &plan {
        let outcome = match dispatch_for(step, lease_id.as_deref()) {
            DaemonRepairDispatch::Disconnect => runner::disconnect(runner_id)
                .map(|_| ())
                .map_err(|error| error.message),
            DaemonRepairDispatch::Connect => connect_outcome(runner::connect(runner_id)),
            DaemonRepairDispatch::AdoptOrphanLease { lease_id } => {
                connect_outcome(runner::connect_with_orphan_adoption(
                    runner_id,
                    Some(&lease_id),
                    &[],
                    false,
                    None,
                    None,
                    None,
                ))
            }
            DaemonRepairDispatch::ReconcileLeaselessOrphans => connect_outcome(
                runner::connect_with_orphan_adoption(runner_id, None, &[], true, None, None, None),
            ),
            DaemonRepairDispatch::ReconcileUnleasedCandidates => connect_outcome(
                runner::connect_with_unleased_candidate_reconciliation(runner_id),
            ),
            DaemonRepairDispatch::RefreshHomeboy {
                git_ref,
                allow_downgrade,
            } => refresh_outcome(runner_id, git_ref, allow_downgrade),
            // A read-only diagnosis, or a recovery command a runner advertised
            // as text with no argv behind it. The operator gets the plan and an
            // explicit reason instead of silence.
            DaemonRepairDispatch::NotAutomatable { reason } => {
                report.repairs.push(RunnerRepair {
                    id: "repair.daemon".to_string(),
                    status: RunnerDoctorStatus::Warning,
                    message: format!(
                        "No automatic repair was applied for daemon repair step `{}`: {reason}. Run the reported commands to repair it explicitly.",
                        step.code
                    ),
                    commands: commands.clone(),
                });
                return true;
            }
        };

        if let Err(message) = outcome {
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: format!(
                    "Could not apply daemon repair step `{}` (step {} of {}): {message}",
                    step.code,
                    applied + 1,
                    plan.len()
                ),
                commands,
            });
            return true;
        }
        applied += 1;
    }

    report.checks.retain(|check| check.id != "daemon.exec");
    let steps = plan
        .iter()
        .map(|step| step.code.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match daemon_admission_ready(runner_id) {
        Ok(true) => report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Ok,
            message: format!("Applied the reported daemon repair plan ({applied} step(s): {steps})"),
            commands,
        }),
        Ok(false) => report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Error,
            message: format!(
                "Applied the reported daemon repair plan ({applied} step(s): {steps}), but Lab admission remains closed"
            ),
            commands,
        }),
        Err(error) => report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Error,
            message: format!(
                "Applied the reported daemon repair plan ({applied} step(s): {steps}), but could not verify Lab admission: {}",
                error.message
            ),
            commands,
        }),
    }
    true
}

/// The same typed readiness decision rendered as `accepting_jobs` by runner
/// status and used by Lab admission. Repair success is only valid when it is
/// true after the final mutation.
pub(super) fn daemon_admission_ready(runner_id: &str) -> homeboy::core::Result<bool> {
    Ok(runner::runner_admission_snapshot(runner_id)?
        .summary
        .accepting_jobs)
}

fn connect_outcome(
    result: homeboy::core::Result<(runner::RunnerConnectReport, i32)>,
) -> Result<(), String> {
    match result {
        Ok((_, 0)) => Ok(()),
        Ok((connect_report, exit_code)) => Err(connect_report
            .failure_message
            .unwrap_or_else(|| format!("runner connect exited with code {exit_code}"))),
        Err(error) => Err(error.message),
    }
}

fn refresh_outcome(
    runner_id: &str,
    git_ref: Option<String>,
    allow_downgrade: bool,
) -> Result<(), String> {
    runner::refresh_homeboy_binary(runner::HomeboyBinaryRefreshOptions {
        runner_id: runner_id.to_string(),
        mode: runner::HomeboyBinaryRefreshMode::Materialize,
        source: None,
        git_ref,
        target_dir: None,
        reconnect: true,
        force: false,
        allow_downgrade,
        dry_run: false,
    })
    .map_err(|error| error.message)
    .and_then(|(_, exit_code)| match exit_code {
        0 => Ok(()),
        code => Err(format!("runner refresh-homeboy exited with code {code}")),
    })
}

fn repair_managed_sources(client: &SshClient, report: &mut RunnerDoctorOutput) {
    let contracts = homeboy::agents::agent_tasks::provider::provider_runner_source_contracts();
    let plans = runner::plan_managed_runner_source_syncs(&contracts);
    if plans.is_empty() {
        return;
    }

    let mut failed = false;
    for plan in plans {
        let output = client.execute(&plan.script);
        if !output.success {
            failed = true;
            report.repairs.push(RunnerRepair {
                id: format!("repair.managed_source.{}", plan.id),
                status: RunnerDoctorStatus::Error,
                message: format!(
                    "Could not refresh managed runner source `{}`: {}",
                    plan.label,
                    output.stderr.trim()
                ),
                commands: Vec::new(),
            });
            continue;
        }

        report.repairs.push(RunnerRepair {
            id: format!("repair.managed_source.{}", plan.id),
            status: RunnerDoctorStatus::Ok,
            message: format!("Refreshed managed runner source `{}`", plan.label),
            commands: Vec::new(),
        });
    }

    if failed {
        return;
    }

    report
        .checks
        .retain(|check| !check.id.starts_with("lab.managed_source."));
    report
        .checks
        .extend(probes::managed_runner_source_checks(client, &contracts));
}
