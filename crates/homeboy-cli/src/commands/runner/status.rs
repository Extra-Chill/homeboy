use std::collections::BTreeMap;

use homeboy::core::daemon::{DaemonRecoveryEvidence, DaemonStaleReasonCode};
use homeboy::runner::readonly_probe;
use homeboy::runner::runners::{
    self as runner, Runner, RunnerActiveJobState, RunnerStatusReport, RuntimeMaterializationStatus,
};
use homeboy_engine_primitives::shell::shell_arg;
use homeboy_runner_contract::RunnerKind;

use super::super::CmdResult;
use super::types::{
    LabFollowup, LabRunnerConnectionCapability, LabRunnerHomeboyOutput, LabSelectedRunnerOutput,
    RunnerArtifactFeatureDiagnostics, RunnerConnectionOutput, RunnerExecutionCapabilities,
    RunnerExecutionCapability, RunnerExtra, RunnerOperatorCommand, RunnerOperatorSummary,
    RunnerOutput, RunnerReconciliationOutcome, RunnerReconciliationStatus, RunnerStatusInspection,
    RunnerStatusUnavailable, RunnerTruncation, RunnerWorkflowBinaryGuidance,
};

const DEFAULT_STATUS_SESSION_LIMIT: usize = 20;

pub(super) fn status(id: Option<&str>, full: bool) -> CmdResult<RunnerOutput> {
    if let Some(id) = id {
        let runners = runner::list()?;
        if is_controller_local_runner(&runners, id) {
            return Ok((controller_local_status_output(id), 0));
        }
        let lab_readiness = runner::lab_runner_readiness()?;
        let preferred_lab_runner = lab_readiness.selected_runner_id.clone();
        let admission_snapshot = runner::runner_admission_snapshot(id)?;
        let report = admission_snapshot.status;
        let capabilities =
            global_execution_capabilities(&runners, Some(&report), Some(&lab_readiness))?;
        // Lead with the compact authoritative admission answer (#9478/#9522);
        // `--full` adds the session detail behind it.
        let admission_summary = Some(admission_snapshot.summary);
        if !full {
            let operator_summary =
                operator_summary_with_admission(&report, admission_summary.as_ref());
            return Ok((
                RunnerOutput {
                    command: "runner.status".to_string(),
                    id: Some(id.to_string()),
                    extra: RunnerExtra {
                        admission_summary,
                        execution_capabilities: Some(capabilities),
                        operator_summary: Some(operator_summary),
                        probe_degradations: readonly_probe::take_degradations(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ));
        }

        let mut operator_hints = runner_status_operator_hints(&report);
        let operator_commands = runner_status_operator_commands(&report);
        let selected_lab_runner = selected_lab_runner_status(Some(id), Some(report.clone()))?;
        let managed_followups =
            runner_followups_with_admission(Some(id), Some(&report), admission_summary.as_ref());
        // Collected last so every bounded probe issued above is represented.
        operator_hints.extend(probe_degradation_hints());
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    admission_summary,
                    execution_capabilities: Some(capabilities),
                    connection: Some(RunnerConnectionOutput::Status(Box::new(
                        sanitized_status_for_output(report),
                    ))),
                    preferred_lab_runner,
                    selected_lab_runner,
                    managed_followups,
                    operator_hints,
                    operator_commands,
                    probe_degradations: readonly_probe::take_degradations(),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }

    let lab_readiness = runner::lab_runner_readiness()?;
    let preferred_lab_runner = lab_readiness.selected_runner_id.clone();
    let runners = runner::list()?;
    let sessions = non_local_sessions(&runners, runner::persisted_statuses()?);
    let capabilities = execution_capabilities(&runners, &sessions, Some(&lab_readiness));
    if !full {
        let omitted = sessions.len().saturating_sub(DEFAULT_STATUS_SESSION_LIMIT);
        let sessions = sessions
            .iter()
            .take(DEFAULT_STATUS_SESSION_LIMIT)
            .map(operator_summary)
            .collect::<Vec<_>>();
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                extra: RunnerExtra {
                    operator_summaries: sessions,
                    execution_capabilities: Some(capabilities),
                    truncation: Some(RunnerTruncation {
                        omitted_sessions: omitted,
                        evidence_ref: "runner:session-inventory".to_string(),
                        full_command: "homeboy runner status --full".to_string(),
                    }),
                    probe_degradations: readonly_probe::take_degradations(),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }
    let (sessions, inspection) = full_status_projections(sessions, runner::statuses_indexed());
    let capabilities = execution_capabilities(&runners, &sessions, Some(&lab_readiness));
    let mut operator_hints: Vec<String> = sessions
        .iter()
        .flat_map(runner_status_operator_hints)
        .collect();
    let operator_commands = sessions
        .iter()
        .flat_map(runner_status_operator_commands)
        .collect();
    let selected_status = preferred_lab_runner.as_deref().and_then(|runner_id| {
        sessions
            .iter()
            .find(|report| report.runner_id == runner_id)
            .cloned()
    });
    let selected_lab_runner =
        selected_lab_runner_status(preferred_lab_runner.as_deref(), selected_status.clone())?;
    let selected_admission = selected_admission_summary(selected_status.as_ref());
    let managed_followups = runner_followups_with_admission(
        preferred_lab_runner.as_deref(),
        selected_status.as_ref(),
        selected_admission.as_ref(),
    );
    // Collected last so every bounded probe issued above is represented.
    operator_hints.extend(probe_degradation_hints());
    Ok((
        RunnerOutput {
            command: "runner.status".to_string(),
            extra: RunnerExtra {
                sessions: sessions
                    .into_iter()
                    .map(sanitized_status_for_output)
                    .collect(),
                execution_capabilities: Some(capabilities),
                inspection: Some(inspection),
                preferred_lab_runner,
                selected_lab_runner,
                managed_followups,
                operator_hints,
                operator_commands,
                probe_degradations: readonly_probe::take_degradations(),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

/// A controller-local query answers from local state only. Cross-runner
/// execution capabilities (Lab admission readiness) are a remote probe and
/// belong to `runner status` without an id or to a specific remote runner
/// query; this query never touches a remote host (#15935).
pub(super) fn controller_local_status_output(id: &str) -> RunnerOutput {
    RunnerOutput {
        command: "runner.status".to_string(),
        id: Some(id.to_string()),
        extra: RunnerExtra {
            operator_hints: vec![
                "Controller-local execution is available; use `homeboy --placement local agent-task cook ...`."
                    .to_string(),
                "Lab runner readiness: `homeboy runner status`.".to_string(),
            ],
            ..Default::default()
        },
        ..Default::default()
    }
}

fn is_controller_local_runner(runners: &[Runner], id: &str) -> bool {
    runners
        .iter()
        .any(|runner| runner.id == id && runner.kind == RunnerKind::Local)
}

pub(super) fn non_local_sessions(
    runners: &[Runner],
    sessions: Vec<RunnerStatusReport>,
) -> Vec<RunnerStatusReport> {
    sessions
        .into_iter()
        .filter(|report| !is_controller_local_runner(runners, &report.runner_id))
        .collect()
}

fn global_execution_capabilities(
    runners: &[Runner],
    queried_report: Option<&RunnerStatusReport>,
    lab_readiness: Option<&runner::LabRunnerReadiness>,
) -> homeboy::core::Result<RunnerExecutionCapabilities> {
    let mut sessions = runner::persisted_statuses()?;
    if let Some(queried_report) = queried_report {
        if let Some(session) = sessions
            .iter_mut()
            .find(|session| session.runner_id == queried_report.runner_id)
        {
            *session = queried_report.clone();
        } else {
            sessions.push(queried_report.clone());
        }
    }
    Ok(execution_capabilities(runners, &sessions, lab_readiness))
}

fn execution_capabilities(
    runners: &[Runner],
    sessions: &[RunnerStatusReport],
    lab_readiness: Option<&runner::LabRunnerReadiness>,
) -> RunnerExecutionCapabilities {
    execution_capabilities_with_local_placement(true, runners, sessions, lab_readiness)
}

/// `lab_readiness` is the same admission projection dispatch preflight
/// consults (`resolve_parsed_command_preflight` requires
/// `LabRunnerReadinessState::ConnectedReady`). Reusing it here keeps
/// `runner status` from reporting a connected-but-not-admitted runner as
/// dispatch-ready (#13631).
pub(super) fn execution_capabilities_with_local_placement(
    local_placement_available: bool,
    runners: &[Runner],
    sessions: &[RunnerStatusReport],
    lab_readiness: Option<&runner::LabRunnerReadiness>,
) -> RunnerExecutionCapabilities {
    let connected_runner_ids = sessions
        .iter()
        .filter(|report| !is_controller_local_runner(runners, &report.runner_id))
        .filter(|report| report.connected)
        .map(|report| report.runner_id.clone())
        .collect::<Vec<_>>();
    let admitted_runner_ids = lab_readiness
        .map(|readiness| readiness.available_runner_ids.clone())
        .unwrap_or_default();
    let available = !admitted_runner_ids.is_empty();
    let (reasons, next_action) = if available {
        (Vec::new(), None)
    } else if connected_runner_ids.is_empty() {
        (
            Vec::new(),
            sessions
                .iter()
                .find(|report| !is_controller_local_runner(runners, &report.runner_id))
                .map(|report| {
                    terminal_daemon_ownership_diagnostic(report)
                        .unwrap_or_else(|| report.status_action().render_command())
                }),
        )
    } else {
        // Connected but not admitted: report the true state plainly instead of
        // presenting connectivity as dispatch readiness, and surface the
        // action that would admit the runner (#13631).
        let reasons = lab_readiness
            .map(|readiness| readiness.reasons.clone())
            .unwrap_or_default();
        let next_action = lab_readiness
            .and_then(|readiness| readiness.remediation_commands.first().cloned())
            .or_else(|| {
                sessions
                    .iter()
                    .find(|report| {
                        report.connected && !is_controller_local_runner(runners, &report.runner_id)
                    })
                    .map(|report| {
                        terminal_daemon_ownership_diagnostic(report)
                            .unwrap_or_else(|| report.status_action().render_command())
                    })
            });
        (reasons, next_action)
    };
    RunnerExecutionCapabilities {
        local_placement: RunnerExecutionCapability {
            available: local_placement_available,
            state: if local_placement_available {
                "available"
            } else {
                "unavailable"
            },
            next_action: if local_placement_available {
                "homeboy --placement local agent-task cook ...".to_string()
            } else {
                "Inspect controller-local execution prerequisites.".to_string()
            },
        },
        lab_runner_connection: LabRunnerConnectionCapability {
            available,
            connected_runner_ids,
            reasons,
            next_action,
        },
    }
}

pub(super) fn reconcile(
    id: &str,
    confirm_workload_processes_absent: bool,
) -> CmdResult<RunnerOutput> {
    let (report, attested_recovery) = if confirm_workload_processes_absent {
        // The attestation runs only when the runner's lifecycle plan is blocked
        // by daemon ownership evidence; otherwise this is the plain reconcile.
        let outcome = runner::reconcile_status_with_remote_attestation(id)?;
        (outcome.status, outcome.attested)
    } else {
        (runner::reconcile_status(id)?, None)
    };
    let (mut output, exit_code) = reconcile_output(id, report)?;
    if let Some(applied) = attested_recovery {
        output.extra.operator_hints.push(format!(
            "applied remote attested dead-lease recovery for lease `{}` in `{}` ({} job(s) terminalized): {}",
            applied.lease_id,
            applied.state_dir,
            applied.job_ids.len(),
            applied.command
        ));
    }
    Ok((output, exit_code))
}

pub(super) fn reconcile_output(
    id: &str,
    report: homeboy::runner::runners::RunnerStatusReport,
) -> CmdResult<RunnerOutput> {
    let admission_snapshot = runner::runner_admission_snapshot_for_status(report);
    let report = admission_snapshot.status;
    let admission_summary = admission_snapshot.summary;
    let reconciliation = reconciliation_outcome(id, &report, &admission_summary);
    let exit_code = if reconciliation.status == RunnerReconciliationStatus::Converged {
        0
    } else {
        1
    };
    let reconcile_command = format!("homeboy runner reconcile {}", shell_arg(id));
    Ok((
        RunnerOutput {
            command: "runner.reconcile".to_string(),
            id: Some(id.to_string()),
            extra: RunnerExtra {
                admission_summary: Some(admission_summary),
                reconciliation: Some(reconciliation),
                connection: Some(RunnerConnectionOutput::Status(Box::new(
                    sanitized_status_for_output(report.clone()),
                ))),
                operator_hints: runner_status_operator_hints(&report),
                // Reconcile has already consumed the current evidence. A repeat
                // is valid only after new evidence, never as its own follow-up.
                operator_commands: runner_status_operator_commands(&report)
                    .into_iter()
                    .filter(|command| command.command != reconcile_command)
                    .collect(),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

pub(crate) fn reconciliation_outcome(
    runner_id: &str,
    report: &homeboy::runner::runners::RunnerStatusReport,
    admission: &homeboy::runner::runners::RunnerAdmissionSummary,
) -> RunnerReconciliationOutcome {
    if admission.accepting_jobs {
        return RunnerReconciliationOutcome {
            owner: RECONCILIATION_OWNER,
            scope: format!("runner `{runner_id}` and its current daemon session"),
            postcondition: RECONCILIATION_POSTCONDITION,
            status: RunnerReconciliationStatus::Converged,
            remaining_blocker: None,
            next_action: None,
            retry_predicate: None,
        };
    }

    let reconcile_command = format!("homeboy runner reconcile {}", shell_arg(runner_id));
    let attested_reconcile_command = format!(
        "homeboy runner reconcile {} --confirm-workload-processes-absent",
        shell_arg(runner_id)
    );
    let (remaining_blocker, next_action, retry_predicate) =
        if terminal_daemon_ownership_blocker(report) {
            // The attested reconcile is the one action that can still recover this
            // state, so the blocked plan names it exactly and never dead-ends
            // (#15556). Applying it remains the operator's explicit choice.
            (
                "daemon_ownership_evidence_unavailable".to_string(),
                Some(attested_reconcile_command),
                format!(
                    "ownership evidence required before daemon recovery: {}",
                    report
                        .daemon_freshness
                        .as_ref()
                        .and_then(|freshness| freshness.ownership_evidence.as_deref())
                        .unwrap_or("no ownership evidence was recorded")
                ),
            )
        } else if !admission.connected {
            (
                "runner_disconnected".to_string(),
                Some(
                    reconciliation_remediation(&reconcile_command, admission, report)
                        .unwrap_or_else(|| {
                            format!("homeboy runner connect {}", shell_arg(runner_id))
                        }),
                ),
                "the runner reconnects and publishes a current daemon session".to_string(),
            )
        } else if !admission.daemon_fresh {
            (
                daemon_freshness_blocker(report),
                Some(
                    reconciliation_remediation(&reconcile_command, admission, report)
                        .unwrap_or_else(|| {
                            format!(
                                "homeboy runner doctor {} --scope lab-offload",
                                shell_arg(runner_id)
                            )
                        }),
                ),
                "daemon_fresh=true after the selected daemon repair completes".to_string(),
            )
        } else if !admission.daemon_compatible {
            (
                "daemon_compatibility".to_string(),
                Some(
                    reconciliation_remediation(&reconcile_command, admission, report)
                        .unwrap_or_else(|| {
                            format!(
                                "homeboy runner doctor {} --scope lab-offload",
                                shell_arg(runner_id)
                            )
                        }),
                ),
                "daemon_compatible=true after the selected daemon identity is repaired".to_string(),
            )
        } else {
            (
                "admission_unavailable".to_string(),
                Some(format!(
                    "homeboy runner status {} --full",
                    shell_arg(runner_id)
                )),
                "an authoritative active-job view is available".to_string(),
            )
        };

    RunnerReconciliationOutcome {
        owner: RECONCILIATION_OWNER,
        scope: format!("runner `{runner_id}` and its current daemon session"),
        postcondition: RECONCILIATION_POSTCONDITION,
        status: RunnerReconciliationStatus::Blocked,
        remaining_blocker: Some(remaining_blocker),
        next_action,
        retry_predicate: Some(retry_predicate),
    }
}

/// State plane that owns direct-runner session reconciliation.
const RECONCILIATION_OWNER: &str = "runner_session";
const RECONCILIATION_POSTCONDITION: &str = "the runner's current daemon session accepts jobs";

fn terminal_daemon_ownership_blocker(report: &RunnerStatusReport) -> bool {
    report
        .daemon_freshness
        .as_ref()
        .is_some_and(|freshness| freshness.has_terminal_recovery_ownership_blocker())
}

fn terminal_daemon_ownership_diagnostic(report: &RunnerStatusReport) -> Option<String> {
    terminal_daemon_ownership_blocker(report).then(|| {
        format!(
            "No automatic daemon recovery: typed ownership evidence is insufficient: {}",
            report
                .daemon_freshness
                .as_ref()
                .and_then(|freshness| freshness.ownership_evidence.as_deref())
                .unwrap_or("no ownership evidence was recorded")
        )
    })
}

fn terminal_daemon_ownership_blocks_mutation(report: &RunnerStatusReport) -> bool {
    terminal_daemon_ownership_blocker(report)
}

fn reconciliation_remediation(
    reconcile_command: &str,
    admission: &homeboy::runner::runners::RunnerAdmissionSummary,
    report: &homeboy::runner::runners::RunnerStatusReport,
) -> Option<String> {
    admission
        .next_action
        .as_ref()
        .filter(|action| action.as_str() != reconcile_command)
        .cloned()
        .or_else(|| {
            report
                .admission_action()
                .map(|action| action.render_command())
                .filter(|action| action != reconcile_command)
        })
}

fn daemon_freshness_blocker(report: &homeboy::runner::runners::RunnerStatusReport) -> String {
    match report
        .daemon_freshness
        .as_ref()
        .and_then(|freshness| freshness.stale_reason_code)
    {
        Some(reason) => format!("daemon_{}", daemon_stale_reason_code(reason)),
        None => "daemon_freshness_unavailable".to_string(),
    }
}

fn daemon_stale_reason_code(reason: DaemonStaleReasonCode) -> &'static str {
    match reason {
        DaemonStaleReasonCode::LeaseMissing => "lease_missing",
        DaemonStaleReasonCode::LeaseCorrupt => "lease_corrupt",
        DaemonStaleReasonCode::LeaseSchemaMismatch => "lease_schema_mismatch",
        DaemonStaleReasonCode::PidDead => "pid_dead",
        DaemonStaleReasonCode::BuildIdentityMismatch => "build_identity_mismatch",
        DaemonStaleReasonCode::BinaryHashMismatch => "binary_hash_mismatch",
        DaemonStaleReasonCode::VersionMismatch => "version_mismatch",
        DaemonStaleReasonCode::RuntimePathsDrift => "runtime_paths_drift",
        DaemonStaleReasonCode::TransportUnreachable => "transport_unreachable",
    }
}

/// Operator-facing hints for read-only probes that hit their bound while this
/// status was assembled (#10418). Without these an operator cannot distinguish
/// "the Lab answered and is idle" from "the Lab never answered".
fn probe_degradation_hints() -> Vec<String> {
    readonly_probe::degradations()
        .into_iter()
        .map(|degradation| {
            // `PARTIAL STATUS` means "the runner did not answer in time". A
            // probe that failed in the *controller's* own environment says
            // nothing about the runner: label it apart (#10525).
            let label = if degradation.reason_code == readonly_probe::REASON_PROBE_UNAVAILABLE {
                "DEGRADED PROBE"
            } else {
                "PARTIAL STATUS"
            };
            format!(
                "{label} ({}): {}",
                degradation.reason_code, degradation.detail
            )
        })
        .collect()
}

fn full_status_projections(
    mut sessions: Vec<RunnerStatusReport>,
    snapshots: homeboy::core::Result<Vec<runner::RunnerActiveJobsSnapshot>>,
) -> (Vec<RunnerStatusReport>, RunnerStatusInspection) {
    let snapshots = match snapshots {
        Ok(snapshots) => snapshots
            .into_iter()
            .map(|snapshot| (snapshot.runner_id.clone(), snapshot))
            .collect::<BTreeMap<_, _>>(),
        Err(error) => {
            let unavailable = RunnerStatusUnavailable {
                runner_id: "controller".to_string(),
                code: error.code.as_str().to_string(),
                message: error.message,
                refresh_command: "homeboy runner status --full".to_string(),
            };
            return (
                sessions,
                RunnerStatusInspection {
                    status: "partial",
                    partial: true,
                    runner_unavailable: vec![unavailable],
                    runner_unqueried: Vec::new(),
                },
            );
        }
    };
    let mut unavailable = Vec::new();
    let mut unqueried = Vec::new();
    for report in &mut sessions {
        let Some(snapshot) = snapshots.get(&report.runner_id) else {
            unqueried.push(report.runner_id.clone());
            continue;
        };
        report.active_job_count = snapshot.active_jobs.len();
        report.active_runner_jobs = snapshot.active_jobs.iter().map(Into::into).collect();
        report.active_jobs = snapshot.active_jobs.clone();
        report.active_job_state = snapshot.active_job_state.clone();
        report.active_job_error = snapshot.active_job_error.clone();
        if let Some(error) = &snapshot.active_job_error {
            unavailable.push(RunnerStatusUnavailable {
                runner_id: report.runner_id.clone(),
                code: error.code.clone(),
                message: error.message.clone(),
                refresh_command: format!(
                    "homeboy runner status {} --full",
                    shell_arg(&report.runner_id)
                ),
            });
        } else if report.active_job_state == RunnerActiveJobState::NotQueried {
            unqueried.push(report.runner_id.clone());
        }
    }
    let partial = !unavailable.is_empty() || !unqueried.is_empty();
    (
        sessions,
        RunnerStatusInspection {
            status: if partial { "partial" } else { "complete" },
            partial,
            runner_unavailable: unavailable,
            runner_unqueried: unqueried,
        },
    )
}

pub(super) fn operator_summary(report: &RunnerStatusReport) -> RunnerOperatorSummary {
    let mut risk = Vec::new();
    if !report.connected {
        risk.push("disconnected".to_string());
    }
    // "unverified" is its own risk, distinct from a proven mismatch. Rendering
    // both as `stale_daemon` would make "I could not check" indistinguishable
    // from "it is stale" (#11106).
    if let Some(warning) = report.admission_blocking_stale_daemon() {
        risk.push(if warning.is_unverified() {
            "unverified_daemon".to_string()
        } else {
            "stale_daemon".to_string()
        });
    }
    if report.active_job_count > 0 {
        risk.push(format!("{} active job(s)", report.active_job_count));
    }
    if let Some(error) = &report.active_job_error {
        risk.push(error.code.clone());
    }
    if let Some(warning) = report.stale_daemon.as_ref() {
        let controller = warning
            .controller_homeboy_build_identity
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| homeboy_product_identity::build_identity().display);
        risk.push(format!(
                "compatibility_skew{}: controller `{controller}`; configured executable `{}`; daemon `{}`",
                warning
                    .compatibility_reason
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default(),
                warning.job_command_binary_build_identity.as_deref().unwrap_or(&warning.job_command_binary_version),
                warning.active_daemon_control_plane_build_identity.as_deref().unwrap_or(&warning.active_daemon_control_plane_version),
            ));
    }
    RunnerOperatorSummary {
        identity: bounded_status_text(&report.runner_id),
        state: format!("{:?}", report.state).to_ascii_lowercase(),
        risk,
        next_action: terminal_daemon_ownership_diagnostic(report)
            .unwrap_or_else(|| report.status_action().render_command()),
    }
}

fn operator_summary_with_admission(
    report: &RunnerStatusReport,
    admission: Option<&runner::RunnerAdmissionSummary>,
) -> RunnerOperatorSummary {
    let mut summary = operator_summary(report);
    if let Some(action) = admission.and_then(|admission| admission.next_action.as_ref()) {
        summary.next_action = action.clone();
    }
    summary
}

fn bounded_status_text(value: &str) -> String {
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(256).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn selected_lab_runner_status(
    runner_id: Option<&str>,
    status: Option<RunnerStatusReport>,
) -> homeboy::core::Result<Option<LabSelectedRunnerOutput>> {
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let runner_config = runner::load(runner_id)?;
    let status = sanitized_status_for_output(match status {
        Some(status) => status,
        None => runner::status(runner_id)?,
    });
    let configured_executable = runner_config
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    Ok(Some(LabSelectedRunnerOutput {
        runner_id: runner_id.to_string(),
        kind: format!("{:?}", runner_config.kind).to_ascii_lowercase(),
        configured_executable: configured_executable.clone(),
        runner_homeboy: lab_runner_homeboy_output(runner_id, &configured_executable, &status),
        executable_requirements: Vec::new(),
        selected_runtime: None,
        runtime_diagnostics: Vec::new(),
        daemon_enabled: runner_config.settings.daemon,
        workspace_root: runner_config.workspace_root.clone(),
        readiness_state: format!("{:?}", status.state).to_ascii_lowercase(),
        connected: status.connected,
        availability: status.admission_availability(runner_config.settings.concurrency_limit),
    }))
}

pub(super) fn lab_runner_homeboy_output(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> LabRunnerHomeboyOutput {
    lab_runner_homeboy_output_with_recovery_guidance(
        runner_id,
        configured_executable,
        status,
        recovery_refresh_guidance(status),
    )
}

fn lab_runner_homeboy_output_with_recovery_guidance(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
    guidance: RecoveryRefreshGuidance,
) -> LabRunnerHomeboyOutput {
    let mut materialization =
        RuntimeMaterializationStatus::for_homeboy_runner(runner_id, configured_executable, status);
    let recovery_blocked = terminal_daemon_ownership_blocks_mutation(status);
    let refresh_safe = authoritative_idle_active_job_projection(status);
    let refresh_command = (!recovery_blocked && refresh_safe)
        .then(|| recovery_refresh_command(&shell_arg(runner_id), guidance))
        .flatten();
    materialization.stale_daemon_refresh_command = refresh_command.clone();
    let stale_daemon = status
        .stale_daemon
        .as_ref()
        .and_then(|warning| stale_daemon_output(warning, refresh_command));
    let binary_roles: Vec<_> = materialization.binary_sources.iter().cloned().collect();
    let controller_cli = binary_roles[0].clone();
    let active_daemon = binary_roles[1].clone();
    let configured_job_binary = binary_roles[2].clone();
    let has_drift = materialization.has_drift();
    LabRunnerHomeboyOutput {
        controller_version: materialization.controller_version,
        controller_build_identity: materialization.controller_build_identity,
        configured_executable: materialization.configured_executable,
        controller_cli,
        active_daemon,
        configured_job_binary,
        binary_roles,
        workflow_binary_guidance: runner_workflow_binary_guidance(),
        active_daemon_version: materialization.active_daemon_version,
        active_daemon_build_identity: materialization.active_daemon_build_identity,
        job_command_binary_version: materialization.job_command_binary_version,
        job_command_binary_build_identity: materialization.job_command_binary_build_identity,
        stale_daemon_severity: materialization.stale_daemon_severity.map(str::to_string),
        stale_daemon_refresh_command: materialization.stale_daemon_refresh_command,
        stale_daemon,
        version_drift: materialization.version_drift,
        command_availability_checks: lab_command_availability_checks(configured_executable),
        artifact_features: runner_artifact_feature_diagnostics(
            runner_id,
            configured_executable,
            status,
            has_drift,
        ),
        refresh_commands: (!recovery_blocked && refresh_safe)
            .then(|| lab_runner_homeboy_refresh_commands(runner_id, status))
            .unwrap_or_default(),
        // An exact refresh owns stale-daemon recovery end-to-end: it selects the
        // controller commit, rotates the daemon, verifies both identities, and
        // proves admission. Do not present the known-weaker fleet upgrade beside it.
        upgrade_command: (!recovery_blocked
            && !(status.admission_blocking_stale_daemon().is_some()
                && refresh_safe
                && recovery_refresh_command(
                    &shell_arg(runner_id),
                    recovery_refresh_guidance(status),
                )
                .is_some()))
        .then(|| {
            format!(
                "homeboy upgrade --force --upgrade-runner {}",
                shell_arg(runner_id)
            )
        }),
        dev_sync: runner::load(runner_id)
            .ok()
            .and_then(|runner| runner.resources.get("dev_sync").cloned()),
    }
}

fn lab_command_availability_checks(homeboy_path: &str) -> Vec<String> {
    let binary = shell_arg(homeboy_path);
    vec![
        format!("{binary} --version"),
        format!("{binary} runner exec --help"),
        format!("{binary} runs artifact --help"),
        format!("{binary} fuzz --help"),
        format!("{binary} runs evidence --help"),
        format!("{binary} extension list"),
    ]
}

fn runner_workflow_binary_guidance() -> RunnerWorkflowBinaryGuidance {
    RunnerWorkflowBinaryGuidance {
        recent_workflows: "Recent or already-queued runner workflows may still be owned by the active_daemon session shown here until the runner reconnects.",
        explicit_workflows: "Explicit runner workflow commands and capability checks use configured_job_binary unless the workflow overrides the command binary itself.",
        capability_checks: "Use command_availability_checks or artifact_features.runner_command_checks to verify the configured_job_binary on the runner before assuming controller_cli features are available remotely.",
    }
}

pub(super) fn runner_artifact_feature_diagnostics(
    runner_id: &str,
    homeboy_path: &str,
    status: &RunnerStatusReport,
    version_drift: bool,
) -> RunnerArtifactFeatureDiagnostics {
    let binary = shell_arg(homeboy_path);
    let runner_arg = shell_arg(runner_id);
    let mut hints = Vec::new();
    if !terminal_daemon_ownership_blocks_mutation(status)
        && (version_drift || status.stale_daemon.is_some())
    {
        hints.push(format!(
            "Runner `{runner_id}` reports informational Homeboy version/build drift; compatibility is determined by the Lab contract and capabilities."
        ));
    }
    if status.connected
        && status
            .session
            .as_ref()
            .and_then(|session| session.local_url.as_ref())
            .is_none()
    {
        hints.push(format!(
            "Runner `{runner_id}` has no direct daemon URL in the active session; verify artifact command support through managed exec instead of assuming the controller binary matches the runner binary."
        ));
    }

    RunnerArtifactFeatureDiagnostics {
        required_features: vec!["runner_exec_artifact_output", "runs_artifact_attach"],
        controller_commands: vec![
            "homeboy runner exec --run-id <run-id> --artifact <path> <runner-id> -- <command>"
                .to_string(),
            "homeboy runs artifact attach <run-id> --runner <runner-id> --path <path> --name <name>"
                .to_string(),
        ],
        runner_command_checks: vec![
            format!("{binary} runner exec --help"),
            format!("{binary} runs artifact attach --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runner exec --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runs artifact attach --help"),
        ],
        hints,
    }
}

fn lab_runner_homeboy_refresh_commands(
    runner_id: &str,
    status: &RunnerStatusReport,
) -> Vec<String> {
    if !authoritative_idle_active_job_projection(status) {
        return Vec::new();
    }
    let runner_arg = shell_arg(runner_id);
    if let Some(command) = recovery_refresh_command(&runner_arg, recovery_refresh_guidance(status))
    {
        return vec![command];
    }
    vec![
        format!("homeboy runner disconnect {runner_arg}"),
        format!("homeboy runner connect {runner_arg}"),
    ]
}

pub(super) fn runner_followups(
    runner_id: Option<&str>,
    status: Option<&RunnerStatusReport>,
) -> Vec<LabFollowup> {
    let mut followups = declared_run_followups(None, runner_id);
    let Some(runner_id) = runner_id else {
        return followups;
    };
    let runner_arg = shell_arg(runner_id);
    if status.is_some_and(authoritative_idle_active_job_projection) {
        if let Some(command) = recovery_refresh_command(
            &runner_arg,
            status.map(recovery_refresh_guidance).unwrap_or_else(|| {
                RecoveryRefreshGuidance::IdentityDrift {
                    controller: homeboy_product_identity::build_identity().git_commit,
                    runner: "unavailable".to_string(),
                }
            }),
        ) {
            followups.push(LabFollowup {
                label: "refresh_homeboy".to_string(),
                command,
                purpose: "Materialize a clean runner-side Homeboy binary, select it for Lab jobs, and refresh the daemon session.".to_string(),
            });
        }
    }
    let has_exact_stale_daemon_refresh = status.is_some_and(|status| {
        authoritative_idle_active_job_projection(status)
            && status.admission_blocking_stale_daemon().is_some()
            && recovery_refresh_command(&runner_arg, recovery_refresh_guidance(status)).is_some()
    });
    followups.extend([
        LabFollowup {
            label: "doctor".to_string(),
            command: format!("homeboy runner doctor {runner_arg} --scope lab-offload"),
            purpose: "Probe runner tools, workspace writability, artifact storage, and Lab offload readiness.".to_string(),
        },
        LabFollowup {
            label: "env".to_string(),
            command: format!("homeboy runner env {runner_arg}"),
            purpose: "Show the redacted environment Homeboy injects into runner jobs.".to_string(),
        },
        LabFollowup {
            label: "homeboy_binary_refresh".to_string(),
            command: format!(
                "homeboy runner disconnect {runner_arg} && homeboy runner connect {runner_arg}"
            ),
            purpose: "Restart the runner daemon so offload uses the currently configured Homeboy binary.".to_string(),
        },
        LabFollowup {
            label: "homeboy_binary_upgrade".to_string(),
            command: format!("homeboy upgrade --force --upgrade-runner {runner_arg}"),
            purpose: "Upgrade the Homeboy binary configured for this runner before reconnecting stale runs.".to_string(),
        },
        LabFollowup {
            label: "exec".to_string(),
            command: format!("homeboy runner exec {runner_arg} -- <command>"),
            purpose: "Run a managed follow-up command through Homeboy instead of opening an ad-hoc shell.".to_string(),
        },
        LabFollowup {
            label: "workspace_prune_preview".to_string(),
            command: format!("homeboy runner workspace prune {runner_arg}"),
            purpose: "Report safe orphaned Lab workspace counts and bytes before deleting anything.".to_string(),
        },
        LabFollowup {
            label: "workspace_prune_drain".to_string(),
            command: format!("homeboy runner workspace prune {runner_arg} --apply --passes 10"),
            purpose: "Reclaim safe orphaned Lab workspaces in bounded passes when the runner workspace filesystem is under disk pressure.".to_string(),
        },
    ]);
    if has_exact_stale_daemon_refresh {
        // The refresh action includes reconnection, identity verification, and
        // admission readiness. Suppress weaker local/upgrade alternatives.
        followups.retain(|followup| {
            !matches!(
                followup.label.as_str(),
                "exec" | "homeboy_binary_refresh" | "homeboy_binary_upgrade"
            )
        });
    }
    if let Ok(path) = std::env::current_dir() {
        followups.push(LabFollowup {
            label: "workspace_sync".to_string(),
            command: format!(
                "homeboy runner workspace sync {runner_arg} --path {} --mode snapshot",
                shell_arg(&path.display().to_string())
            ),
            purpose: "Materialize the current checkout into the runner workspace before a replay or follow-up run.".to_string(),
        });
    }
    followups
}

pub(crate) fn runner_followups_with_admission(
    runner_id: Option<&str>,
    status: Option<&RunnerStatusReport>,
    admission: Option<&runner::RunnerAdmissionSummary>,
) -> Vec<LabFollowup> {
    let mut followups = runner_followups(runner_id, status);
    let Some(admission) = admission else {
        return followups;
    };
    let terminal_ownership_blocker = status.is_some_and(terminal_daemon_ownership_blocker);
    let Some(action) = admission.next_action.as_ref() else {
        if !terminal_ownership_blocker {
            return followups;
        }
        // No repair is safe, but generic mutation guidance is unsafe too.
        // Retain only diagnostic follow-ups and the terminal freshness evidence.
        followups.retain(|followup| !is_terminal_ownership_mutating_followup(followup));
        return followups;
    };

    // A state-specific admission action supersedes generic binary mutation and
    // restart guidance. In particular, a missing lease with ambiguous daemon
    // candidates cannot be made safe by refreshing, upgrading, disconnecting,
    // or reconnecting the controller session.
    followups.retain(|followup| !is_terminal_ownership_mutating_followup(followup));
    followups.push(LabFollowup {
        label: "admission_recovery".to_string(),
        command: action.clone(),
        purpose: "Apply the authoritative runner admission recovery selected from the current daemon evidence.".to_string(),
    });
    followups
}

fn is_terminal_ownership_mutating_followup(followup: &LabFollowup) -> bool {
    // Generic runner exec accepts arbitrary argv, so it is mutating for
    // terminal ownership recovery even when its displayed purpose is diagnostic.
    matches!(
        followup.label.as_str(),
        "exec"
            | "refresh_homeboy"
            | "homeboy_binary_refresh"
            | "homeboy_binary_upgrade"
            | "workspace_prune_drain"
            | "workspace_sync"
    )
}

pub(crate) fn selected_admission_summary(
    selected_status: Option<&RunnerStatusReport>,
) -> Option<runner::RunnerAdmissionSummary> {
    selected_status.map(|report| report.admission_summary())
}

/// Recovery targets the controller release; refresh enforces the version guard.
fn recovery_refresh_guidance(_status: &RunnerStatusReport) -> RecoveryRefreshGuidance {
    RecoveryRefreshGuidance::Refresh(homeboy::runner::controller_refresh_ref())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecoveryRefreshGuidance {
    Refresh(String),
    IdentityDrift {
        controller: Option<String>,
        runner: String,
    },
}

fn recovery_refresh_command(runner_arg: &str, guidance: RecoveryRefreshGuidance) -> Option<String> {
    match guidance {
        RecoveryRefreshGuidance::Refresh(refresh_ref) => Some(format!(
            "homeboy runner refresh-homeboy {runner_arg} --ref {refresh_ref} --reconnect"
        )),
        RecoveryRefreshGuidance::IdentityDrift { .. } => None,
    }
}

fn authoritative_idle_active_job_projection(status: &RunnerStatusReport) -> bool {
    status.active_job_state == RunnerActiveJobState::Available && status.active_job_count == 0
}

fn sanitized_status_for_output(mut report: RunnerStatusReport) -> RunnerStatusReport {
    report.stale_daemon = report
        .stale_daemon
        .as_ref()
        .map(homeboy::runner::runners::RunnerStaleDaemonWarning::sanitized_for_output);
    report
}

fn stale_daemon_output(
    warning: &homeboy::runner::runners::RunnerStaleDaemonWarning,
    refresh_command: Option<String>,
) -> Option<serde_json::Value> {
    let mut warning = serde_json::to_value(warning.sanitized_for_output()).ok()?;
    let fields = warning.as_object_mut()?;
    if let Some(refresh_command) = refresh_command {
        fields.insert("refresh_command".to_string(), refresh_command.into());
    } else {
        fields.remove("refresh_command");
    }
    Some(warning)
}

pub(crate) fn declared_run_followups(
    run_kind: Option<&str>,
    _runner_id: Option<&str>,
) -> Vec<LabFollowup> {
    default_run_followup_declarations()
        .iter()
        .filter(|followup| followup_matches(followup, run_kind))
        .map(declared_run_followup)
        .collect()
}

fn followup_matches(followup: &RunFollowup, run_kind: Option<&str>) -> bool {
    let declared_kind = followup
        .run_kind
        .as_deref()
        .or(followup.workload.as_deref());
    match (run_kind, declared_kind) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(run_kind), Some(declared_kind)) => run_kind == declared_kind,
    }
}

fn default_run_followup_declarations() -> Vec<RunFollowup> {
    vec![
        run_followup(
            "recent_runs",
            None,
            "homeboy runs list --limit 5",
            "Find recent persisted run records before digging into runner state.",
        ),
        run_followup(
            "latest_bench_run",
            Some("bench"),
            "homeboy runs latest-run --kind bench",
            "Resolve the latest benchmark run id for evidence inspection.",
        ),
        run_followup(
            "latest_fuzz_run",
            Some("fuzz"),
            "homeboy runs latest-run --kind fuzz",
            "Resolve the latest fuzz run id for evidence inspection.",
        ),
        run_followup(
            "run_artifacts",
            None,
            "homeboy runs artifacts <run-id>",
            "List recorded run artifacts through Homeboy.",
        ),
        run_followup(
            "run_evidence",
            None,
            "homeboy runs evidence <run-id>",
            "Show stable evidence summary and reviewer-facing commands for one run.",
        ),
        run_followup(
            "run_refs",
            Some("bench"),
            "homeboy runs refs --kind bench --limit 10",
            "List recent benchmark run and artifact refs.",
        ),
        run_followup(
            "fuzz_run_refs",
            Some("fuzz"),
            "homeboy runs refs --kind fuzz --limit 10",
            "List recent fuzz run and artifact refs.",
        ),
    ]
}

fn run_followup(
    label: &str,
    run_kind: Option<&str>,
    command_script: &str,
    purpose: &str,
) -> RunFollowup {
    RunFollowup {
        label: label.to_string(),
        run_kind: run_kind.map(str::to_string),
        workload: None,
        command_script: command_script.to_string(),
        purpose: purpose.to_string(),
    }
}

fn declared_run_followup(declaration: &RunFollowup) -> LabFollowup {
    LabFollowup {
        label: declaration.label.clone(),
        command: declaration.command_script.clone(),
        purpose: declaration.purpose.clone(),
    }
}

pub(super) fn runner_status_operator_hints(report: &RunnerStatusReport) -> Vec<String> {
    // Migration can be blocked even without a session in the new layout.
    if let Some(error) = report.active_job_error.as_ref().filter(|error| {
        error.code == "legacy_daemon_busy" || error.code == "legacy_daemon_observation_failed"
    }) {
        return vec![error.message.clone()];
    }
    if report.session.is_none() || !report.connected {
        return Vec::new();
    }
    let mut hints = Vec::new();
    if report.active_job_state == RunnerActiveJobState::Unavailable {
        let reason = report
            .active_job_error
            .as_ref()
            .map(|err| err.message.as_str())
            .unwrap_or("active-job status endpoint was unavailable");
        hints.push(format!(
            "Active-job status for `{}` is unavailable: {reason}. Treat active_job_count=0 as unknown, not idle.",
            report.runner_id
        ));
    } else if let Some(error) = report
        .active_job_error
        .as_ref()
        .filter(|error| error.code == "active_job_view_inconsistent")
    {
        hints.push(format!(
            "Active-job view for `{}` is inconsistent: {}",
            report.runner_id, error.message
        ));
    }
    if report.stale_runner_job_count > 0 {
        hints.push(format!(
            "Runner `{}` has {} stale runner job(s) that are no longer active. Inspect each durable run with its listed agent-task status command, then retry only recoverable work.",
            report.runner_id, report.stale_runner_job_count
        ));
    }
    if report.admission_blocking_stale_daemon().is_some() {
        let mut materialization =
            RuntimeMaterializationStatus::for_homeboy_runner(&report.runner_id, "homeboy", report);
        materialization.stale_daemon_refresh_command =
            (!terminal_daemon_ownership_blocks_mutation(report)
                && authoritative_idle_active_job_projection(report))
            .then(|| {
                recovery_refresh_command(
                    &shell_arg(&report.runner_id),
                    recovery_refresh_guidance(report),
                )
            })
            .flatten();
        hints.extend(materialization.stale_daemon_hint());
    }
    if !terminal_daemon_ownership_blocks_mutation(report) {
        if let RecoveryRefreshGuidance::IdentityDrift { controller, runner } =
            recovery_refresh_guidance(report)
        {
            hints.push(format!(
                "Runner `{}` reports exact Homeboy build drift: controller={} runner={runner}. This is informational; compatibility is determined by the Lab contract and capabilities.",
                report.runner_id,
                controller.as_deref().unwrap_or("unavailable"),
            ));
        }
    }
    if report.active_job_count > 0 {
        hints.push(format!(
            "Active daemon jobs block recovery for `{}`. List authoritative live jobs and retained durable projections with `homeboy runner job list {} --active` before inspecting or cancelling a known job.",
            report.runner_id, report.runner_id
        ));
    }
    hints
}

pub(super) fn runner_status_operator_commands(
    report: &RunnerStatusReport,
) -> Vec<RunnerOperatorCommand> {
    runner_status_operator_commands_with_recovery_guidance(
        report,
        recovery_refresh_guidance(report),
    )
}

fn runner_status_operator_commands_with_recovery_guidance(
    report: &RunnerStatusReport,
    guidance: RecoveryRefreshGuidance,
) -> Vec<RunnerOperatorCommand> {
    if report.session.is_none() {
        return Vec::new();
    }

    if authoritative_idle_active_job_projection(report)
        && report.admission_blocking_stale_daemon().is_some()
    {
        if let Some(command) =
            recovery_refresh_command(&shell_arg(&report.runner_id), guidance.clone())
        {
            return vec![RunnerOperatorCommand {
                scope: "daemon_refresh",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command,
                description: "Refresh the configured job command binary, rotate the idle daemon, and verify identity plus admission convergence.".to_string(),
            }];
        }
    }

    let mut commands = Vec::new();
    if report.active_job_count > 0 {
        commands.push(RunnerOperatorCommand {
            scope: "job_list",
            runner_id: report.runner_id.clone(),
            job_id: None,
            command: format!(
                "homeboy runner job list {} --active",
                shell_arg(&report.runner_id)
            ),
            description:
                "List the authoritative jobs blocking runner recovery before selecting a follow-up."
                    .to_string(),
        });
    }
    for job in report
        .active_runner_jobs
        .iter()
        .chain(report.stale_runner_jobs.iter())
    {
        if job.job_id.starts_with("unknown-daemon-owner-") {
            if !commands
                .iter()
                .any(|command| command.scope == "unknown_owner_reconcile")
            {
                commands.push(RunnerOperatorCommand {
                    scope: "unknown_owner_reconcile",
                    runner_id: report.runner_id.clone(),
                    job_id: None,
                    command: format!("homeboy runner reconcile {}", shell_arg(&report.runner_id)),
                    description: "Reconcile the daemon lease and /jobs store projection without cancelling the unprojected live owner.".to_string(),
                });
            }
            continue;
        }
        if job.stale_reason.as_deref() == Some("child_run_running_without_active_runner_job") {
            if let Some(run_id) = job.durable_run_id.as_deref() {
                commands.push(RunnerOperatorCommand {
                    scope: "agent_task_status",
                    runner_id: report.runner_id.clone(),
                    job_id: None,
                    command: format!("homeboy agent-task status {run_id}"),
                    description:
                        "Inspect the durable orphaned agent-task run and its preserved evidence."
                            .to_string(),
                });
                commands.push(RunnerOperatorCommand {
                    scope: "agent_task_retry",
                    runner_id: report.runner_id.clone(),
                    job_id: None,
                    command: format!("homeboy agent-task retry {run_id}"),
                    description: "Create a fresh durable attempt after reviewing the orphaned run."
                        .to_string(),
                });
            }
            continue;
        }
        commands.push(RunnerOperatorCommand {
            scope: "job_logs",
            runner_id: report.runner_id.clone(),
            job_id: Some(job.job_id.clone()),
            command: format!(
                "homeboy runner job logs {} {} --follow",
                report.runner_id, job.job_id
            ),
            description: "Follow the active runner job event stream.".to_string(),
        });
        if !job.status.is_terminal() {
            commands.push(RunnerOperatorCommand {
                scope: "job_cancel",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!(
                    "homeboy runner job cancel {} {}",
                    report.runner_id, job.job_id
                ),
                description: "Request cancellation for a queued or running runner job.".to_string(),
            });
        }
        if let Some(run_id) = job.durable_run_id.as_deref() {
            commands.push(RunnerOperatorCommand {
                scope: "artifact_get",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!("homeboy runs artifact get {run_id} <artifact-id> -o <path>"),
                description: "Fetch a mirrored observation artifact after the run records one."
                    .to_string(),
            });
        }
    }

    if report.connected {
        commands.push(RunnerOperatorCommand {
            scope: "session_reconcile",
            runner_id: report.runner_id.clone(),
            job_id: None,
            command: format!("homeboy runner reconcile {}", shell_arg(&report.runner_id)),
            description:
                "Reconcile the current daemon session's metadata and settle its terminal jobs."
                    .to_string(),
        });
    }

    if authoritative_idle_active_job_projection(report)
        && report.admission_blocking_stale_daemon().is_some()
    {
        if let Some(refresh_command) =
            recovery_refresh_command(&shell_arg(&report.runner_id), guidance)
        {
            commands.push(RunnerOperatorCommand {
            scope: "daemon_refresh",
            runner_id: report.runner_id.clone(),
            job_id: None,
            command: refresh_command,
            description: "Restart the active runner daemon so the control plane uses the configured job command binary.".to_string(),
        });
        }
    }

    if let Some(freshness) = report.daemon_freshness.as_ref().filter(|freshness| {
        freshness.has_recovery_ownership_proof()
            && freshness.active_jobs > 0
            && freshness.stale_reason_code == Some(DaemonStaleReasonCode::VersionMismatch)
    }) {
        if let Some(command) = freshness.adoption_command.as_ref() {
            commands.push(RunnerOperatorCommand {
                scope: "daemon_reconcile_split_view",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command: command.clone(),
                description: "Reconcile the stale daemon split view only after the remote owner-lock, process, and listener probes prove no daemon-owned process remains alive.".to_string(),
            });
        }
    }

    // A fresh, authoritatively idle remote daemon whose controller session was
    // lost is recoverable by a plain reconnect (#8694). Surface the reconnect
    // command so operators are not left with a disconnected, non-restartable
    // runner and no actionable path.
    if let Some(freshness) = report.daemon_freshness.as_ref().filter(|freshness| {
        freshness.recovery_evidence == Some(DaemonRecoveryEvidence::Recoverable)
    }) {
        if let Some(command) = freshness.adoption_command.as_ref() {
            commands.push(RunnerOperatorCommand {
                scope: "daemon_reconnect_fresh_idle",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command: command.clone(),
                description: "Reconnect the controller session to the fresh, idle remote daemon; it reports zero authoritatively idle jobs and needs no replacement.".to_string(),
            });
        }
    }

    if terminal_daemon_ownership_blocks_mutation(report) {
        commands.retain(|command| !is_terminal_ownership_mutating_command(command));
    }

    commands
}

fn is_terminal_ownership_mutating_command(command: &RunnerOperatorCommand) -> bool {
    matches!(
        command.scope,
        "unknown_owner_reconcile"
            | "agent_task_retry"
            | "job_cancel"
            | "session_reconcile"
            | "daemon_refresh"
            | "daemon_reconcile_split_view"
            | "daemon_reconnect_fresh_idle"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::runner::runners::{RunnerSession, RunnerSessionState, RunnerTunnelMode};
    use homeboy::runner::{RunnerActiveJobState, RunnerSessionRole, RunnerStaleDaemonWarning};

    #[test]
    fn stale_daemon_status_hint_recommends_job_binary_refresh() {
        let report = stale_daemon_report();

        let hints = runner_status_operator_hints(&report);
        let commands = runner_status_operator_commands(&report);

        let hint = hints
            .iter()
            .find(|hint| hint.contains("stale daemon"))
            .expect("stale daemon hint");
        let job_binary_refresh = format!(
            "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect",
            homeboy_product_identity::product_version()
        );
        assert!(hint.contains(&job_binary_refresh));

        let refresh = commands
            .iter()
            .find(|command| command.scope == "daemon_refresh")
            .expect("daemon refresh command");
        assert!(refresh.command.starts_with(&job_binary_refresh));
    }

    #[test]
    fn build_drift_alone_is_not_a_stale_daemon_with_refresh_guidance() {
        let mut report = stale_daemon_report();
        let warning = report.stale_daemon.as_mut().expect("warning");
        warning.stale_runtime_paths.clear();
        warning.changed_runtime_paths.clear();

        assert!(!runner_status_operator_hints(&report)
            .iter()
            .any(|hint| hint.contains("stale daemon")));
        assert!(!runner_status_operator_commands(&report)
            .iter()
            .any(|command| command.scope == "daemon_refresh"));
    }

    #[test]
    fn active_refresh_guidance_uses_controller_release_tag() {
        let expected = format!("v{}", homeboy_product_identity::product_version());
        let report = stale_daemon_report();
        let commands = lab_runner_homeboy_refresh_commands("homeboy-lab", &report);
        let followups = runner_followups(Some("homeboy-lab"), Some(&report));

        assert_eq!(
            commands[0],
            format!("homeboy runner refresh-homeboy homeboy-lab --ref {expected} --reconnect")
        );
        assert_eq!(
            followups
                .iter()
                .find(|followup| followup.label == "refresh_homeboy")
                .expect("refresh followup")
                .command,
            commands[0]
        );
        assert_eq!(commands.len(), 1);
        assert!(followups.iter().all(|followup| {
            !matches!(
                followup.label.as_str(),
                "exec" | "homeboy_binary_refresh" | "homeboy_binary_upgrade"
            )
        }));

        let output = lab_runner_homeboy_output("homeboy-lab", "/opt/homeboy", &report);
        assert_eq!(output.refresh_commands, commands);
        assert_eq!(output.upgrade_command, None);

        let operator_commands = runner_status_operator_commands(&report);
        assert_eq!(operator_commands.len(), 1);
        assert_eq!(operator_commands[0].scope, "daemon_refresh");
        assert_eq!(operator_commands[0].command, commands[0]);
    }

    #[test]
    fn unknown_active_job_states_never_emit_or_suppress_refresh_recovery() {
        for state in [
            RunnerActiveJobState::Unavailable,
            RunnerActiveJobState::NotQueried,
        ] {
            let mut report = stale_daemon_report();
            report.active_job_state = state;

            let commands = runner_status_operator_commands(&report);
            let followups = runner_followups(Some("homeboy-lab"), Some(&report));
            let output = lab_runner_homeboy_output("homeboy-lab", "/opt/homeboy", &report);

            assert!(commands
                .iter()
                .all(|command| command.scope != "daemon_refresh"));
            assert!(output.stale_daemon_refresh_command.is_none());
            assert!(output.refresh_commands.is_empty());
            assert!(output.upgrade_command.is_some());
            assert!(followups.iter().any(|followup| {
                matches!(
                    followup.label.as_str(),
                    "exec" | "homeboy_binary_refresh" | "homeboy_binary_upgrade"
                )
            }));
        }
    }

    #[test]
    fn no_id_full_status_uses_indexed_projections_without_remote_fallback() {
        let (_sessions, inspection) = full_status_projections(
            Vec::new(),
            Err(homeboy::core::Error::internal_unexpected(
                "indexed probe failed",
            )),
        );
        assert_eq!(inspection.status, "partial");
        assert!(inspection.partial);
        assert_eq!(inspection.runner_unavailable.len(), 1);
        assert_eq!(inspection.runner_unavailable[0].runner_id, "controller");
        assert_eq!(
            inspection.runner_unavailable[0].refresh_command,
            "homeboy runner status --full"
        );
    }

    #[test]
    fn identity_drift_never_leaks_the_legacy_daemon_refresh_command() {
        let mut report = stale_daemon_report();
        report
            .session
            .as_mut()
            .expect("session")
            .homeboy_build_identity = Some("homeboy 0.342.0+7e29bde24e00".to_string());
        report
            .stale_daemon
            .as_mut()
            .expect("stale daemon")
            .refresh_command =
            "homeboy runner refresh-homeboy homeboy-lab --ref 93bc3e103459 --reconnect".to_string();
        let guidance = RecoveryRefreshGuidance::IdentityDrift {
            controller: Some("93bc3e103459".to_string()),
            runner: "7e29bde24e00".to_string(),
        };

        let commands = runner_status_operator_commands_with_recovery_guidance(&report, guidance);
        assert!(commands
            .iter()
            .all(|command| command.scope != "daemon_refresh"));

        let output = lab_runner_homeboy_output_with_recovery_guidance(
            "homeboy-lab",
            "/opt/homeboy",
            &report,
            RecoveryRefreshGuidance::IdentityDrift {
                controller: Some("93bc3e103459".to_string()),
                runner: "7e29bde24e00".to_string(),
            },
        );
        let serialized = serde_json::to_value(output).expect("serialize output");
        assert!(serialized.get("stale_daemon_refresh_command").is_none());
        assert!(serialized["stale_daemon"].get("refresh_command").is_none());
        assert_eq!(
            serialized["stale_daemon"]["recovery_commands"],
            serde_json::json!([format!(
                "homeboy runner refresh-homeboy homeboy-lab --ref {} --reconnect",
                homeboy::runner::controller_refresh_ref()
            )])
        );
        assert!(!serialized
            .to_string()
            .contains("--ref 93bc3e103459 --reconnect"));

        let report = sanitized_status_for_output(report);
        let serialized = serde_json::to_value(report).expect("serialize full status");
        assert_eq!(
            serialized["stale_daemon"]["refresh_command"],
            format!(
                "homeboy runner refresh-homeboy homeboy-lab --ref {} --reconnect",
                homeboy::runner::controller_refresh_ref()
            )
        );
        assert!(!serialized
            .to_string()
            .contains("--ref 93bc3e103459 --reconnect"));

        let nested = serde_json::json!({ "selected_lab_runner": { "status": serialized } });
        assert!(!nested
            .to_string()
            .contains("--ref 93bc3e103459 --reconnect"));
    }

    #[test]
    fn verified_recovery_keeps_the_safe_daemon_refresh_command() {
        let mut report = stale_daemon_report();
        report
            .session
            .as_mut()
            .expect("session")
            .homeboy_build_identity = Some("homeboy 0.259.0+7e29bde24e00".to_string());
        report.stale_daemon = Some(
            RunnerStaleDaemonWarning::new(
                "homeboy-lab",
                "homeboy 0.259.0".to_string(),
                "homeboy 0.262.0".to_string(),
                Some("homeboy 0.259.0+7e29bde24e00".to_string()),
                Some("homeboy 0.262.0+7e29bde24e00".to_string()),
            )
            .with_runtime_paths(
                "homeboy-lab",
                Vec::new(),
                vec![homeboy::runner::RunnerChangedRuntimePath {
                    env: "HOMEBOY_PLUGIN_PATH".to_string(),
                    loaded_path: Some("/old".to_string()),
                    configured_path: Some("/new".to_string()),
                }],
            ),
        );
        let refresh = format!(
            "homeboy runner refresh-homeboy homeboy-lab --ref {} --reconnect",
            homeboy::runner::controller_refresh_ref()
        );
        let commands = runner_status_operator_commands_with_recovery_guidance(
            &report,
            RecoveryRefreshGuidance::Refresh(homeboy::runner::controller_refresh_ref()),
        );
        assert!(commands
            .iter()
            .any(|command| { command.scope == "daemon_refresh" && command.command == refresh }));

        let output = lab_runner_homeboy_output_with_recovery_guidance(
            "homeboy-lab",
            "/opt/homeboy",
            &report,
            RecoveryRefreshGuidance::Refresh(homeboy::runner::controller_refresh_ref()),
        );
        let serialized = serde_json::to_value(output).expect("serialize output");
        assert_eq!(serialized["stale_daemon_refresh_command"], refresh);
        assert_eq!(serialized["stale_daemon"]["refresh_command"], refresh);
        assert_eq!(
            serialized["stale_daemon"]["recovery_commands"],
            serde_json::json!([refresh])
        );
    }

    #[test]
    fn legacy_blockers_are_visible_even_without_a_connected_session() {
        let mut report = stale_daemon_report();
        report.connected = false;
        report.session = None;
        report.active_job_state = RunnerActiveJobState::Unavailable;
        report.active_job_error = Some(homeboy_lab_runner::RunnerActiveJobError {
            code: "legacy_daemon_busy".to_string(),
            message: "legacy primary blocked by job-1, run-1, child_pid 42".to_string(),
        });
        let hints = runner_status_operator_hints(&report);
        assert_eq!(
            hints,
            vec!["legacy primary blocked by job-1, run-1, child_pid 42"]
        );
    }

    fn stale_daemon_report() -> RunnerStatusReport {
        let controller_commit = homeboy_product_identity::build_identity()
            .git_commit
            .expect("test build has an immutable controller commit");
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: Some("lab-server".to_string()),
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:7331".to_string()),
                local_port: Some(7331),
                local_url: Some("http://127.0.0.1:7331".to_string()),
                tunnel_pid: Some(12345),
                tunnel_process_start_identity: None,
                proxy_forward: None,
                remote_daemon_pid: Some(23456),
                remote_daemon_lease_id: Some("lease-23456".to_string()),
                homeboy_version: "homeboy 0.259.0".to_string(),
                homeboy_build_identity: Some(format!("homeboy 0.259.0+{controller_commit}")),
                connected_at: "2026-06-26T00:00:00Z".to_string(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
                leaseless_recovery_evidence: None,
            }),
            stale_daemon: Some(
                RunnerStaleDaemonWarning::new(
                    "homeboy-lab",
                    "homeboy 0.259.0".to_string(),
                    "homeboy 0.262.0".to_string(),
                    Some(format!("homeboy 0.259.0+{controller_commit}")),
                    Some("homeboy 0.262.0+binary".to_string()),
                )
                .with_runtime_paths(
                    "homeboy-lab",
                    Vec::new(),
                    vec![homeboy::runner::RunnerChangedRuntimePath {
                        env: "HOMEBOY_PLUGIN_PATH".to_string(),
                        loaded_path: Some("/old".to_string()),
                        configured_path: Some("/new".to_string()),
                    }],
                ),
            ),
            configured_job_binary_build_identity: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
    }
}

struct RunFollowup {
    label: String,
    command_script: String,
    purpose: String,
    workload: Option<String>,
    run_kind: Option<String>,
}
