use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::agents::agent_tasks::provider::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
};
use homeboy::core::engine::shell;
use homeboy::core::server::{self, Server, SshClient};
use homeboy::runner::runners::{
    self as runner, daemon_repair_codes, Runner, RunnerSession, RunnerToolRegistry, RunnerToolSpec,
    RunnerTunnelMode,
};
use homeboy_runner_contract::RunnerKind;
use serde::Serialize;

use crate::commands::output_runtime::{CommandPresentation, CommandRun};
use crate::commands::CmdResult;

mod checks;
mod common;
mod extension_parity;
mod local;
mod probes;
mod remote;
mod repair;
mod repair_policy;
mod target;
mod types;

pub use types::{RunnerDoctorOutput, RunnerDoctorStatus};

#[derive(Debug, Default)]
pub struct RunnerDoctorOptions {
    pub path: Option<String>,
    pub extensions: Vec<String>,
    pub required_tools: Vec<String>,
    pub agent_backend: Option<String>,
    pub agent_selector: Option<String>,
    pub scope: RunnerDoctorScope,
    pub repair: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunnerDoctorScope {
    #[default]
    General,
    LabOffload,
    SecretEnv,
}

pub fn run(runner_id: &str) -> CmdResult<RunnerDoctorOutput> {
    run_with_options(runner_id, RunnerDoctorOptions::default())
}

pub(crate) fn run_with_options(
    runner_id: &str,
    mut options: RunnerDoctorOptions,
) -> CmdResult<RunnerDoctorOutput> {
    options.scope = repair_scope(options.scope, options.repair);
    let target = target::resolve(runner_id)?;
    let mut report = report_for_target(&target, &options);

    let migration = runner::secret_env_migration_plan(runner_id)?;
    report.secret_env_migration = (!migration.is_empty()).then_some(migration);

    if options.repair {
        repair::apply(&target, &options, &mut report);

        // Repair is not success by itself. Re-probe the same resolved target so
        // the terminal report verifies its identity, binary, SSH, workspace,
        // daemon, and provider readiness after the mutation.
        let mut repairs = std::mem::take(&mut report.repairs);
        report = report_for_target(&target, &options);
        match target.ensure_current() {
            Ok(()) => {
                let migration = runner::secret_env_migration_plan(runner_id)?;
                report.secret_env_migration = (!migration.is_empty()).then_some(migration);
            }
            Err(error) => repairs.push(types::RunnerRepair {
                id: "repair.target_identity".to_string(),
                status: RunnerDoctorStatus::Error,
                message: error.message,
                commands: Vec::new(),
            }),
        }
        report.repairs = repairs;
    }

    // Only general doctor observes the complete capability surface. Scoped
    // Lab diagnostics deliberately skip CPU, tools, and artifact
    // probes, so treating that partial report as a complete observation would
    // evict known-good admission evidence.
    if observes_complete_capabilities(options.scope) {
        runner::observe_runner_capabilities(runner_id);
    }
    if options.scope == RunnerDoctorScope::LabOffload {
        let catalog = homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog::discover();
        let eligible_provider_ids = probes::eligible_provider_ids(
            catalog.providers(),
            options.agent_backend.as_deref(),
            options.agent_selector.as_deref(),
        );
        let (status, provider_readiness) =
            checks::lab_offload_status(&report.checks, &eligible_provider_ids);
        report.status = status;
        report.provider_readiness = Some(provider_readiness);
    } else {
        report.status = checks::overall_status(&report.checks);
    }
    ensure_failure(&mut report);
    let exit_code = report.status.operational_exit_code();
    Ok((report, exit_code))
}

fn report_for_target(
    target: &target::RunnerTarget,
    options: &RunnerDoctorOptions,
) -> RunnerDoctorOutput {
    match target {
        target::RunnerTarget::Local { id, runner } => {
            // The local probe's artifact root is the only filesystem root this
            // command reads, so it is resolved once here at the entry point
            // rather than inside the probe. The SSH branch resolves its root on
            // the remote host and deliberately does not take this one.
            let artifact_root = crate::core::paths::artifact_root().ok();
            local::report(id, runner.as_ref(), &options, artifact_root.as_deref())
        }
        target::RunnerTarget::Ssh {
            id,
            runner,
            server,
            client,
        } => remote::report(id, runner, server, client, &options),
    }
}

const COMPACT_CHECK_LIMIT: usize = 12;
const COMPACT_PROVIDER_LIMIT: usize = 10;
const COMPACT_TEXT_LIMIT: usize = 256;
const COMPACT_PROJECTION_BYTES: usize = 8 * 1024;

/// Keep default doctor output to the facts needed to decide whether the runner
/// is usable. `--full` remains a lossless, redacted evidence surface.
pub(crate) fn output_projection(mut report: RunnerDoctorOutput, full: bool) -> serde_json::Value {
    ensure_failure(&mut report);
    let value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    if full {
        return homeboy::core::redaction::redact_json(&value);
    }

    bounded_projection_envelope(compact_projection(&report))
}

fn compact_projection(report: &RunnerDoctorOutput) -> serde_json::Value {
    let failed_checks = report
        .checks
        .iter()
        .filter(|check| check.status != RunnerDoctorStatus::Ok)
        .count();
    let mut prioritized_checks = report.checks.iter().collect::<Vec<_>>();
    prioritized_checks.sort_by_key(|check| match check.status {
        RunnerDoctorStatus::Error => 0,
        RunnerDoctorStatus::Warning => 1,
        RunnerDoctorStatus::Ok => 2,
    });
    let checks = prioritized_checks
        .into_iter()
        .take(COMPACT_CHECK_LIMIT)
        .map(|check| {
            serde_json::json!({
                "id": bounded_text(&check.id),
                "status": check.status,
                "message": bounded_text(&check.message),
                "remediation": check.remediation.as_deref().map(bounded_text),
                "remediation_action": compact_remediation_action(check.remediation_action.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    let (ready_for, blocked_for, unverified_for, unverified_remediation) =
        report.provider_readiness.as_ref().map_or_else(
            || (Vec::new(), Vec::new(), Vec::new(), None),
            |readiness| {
                (
                    readiness
                        .ready_for
                        .iter()
                        .take(COMPACT_PROVIDER_LIMIT)
                        .map(|value| bounded_text(value))
                        .collect::<Vec<_>>(),
                    readiness
                        .blocked_for
                        .iter()
                        .take(COMPACT_PROVIDER_LIMIT)
                        .map(|value| bounded_text(value))
                        .collect::<Vec<_>>(),
                    readiness
                        .unverified_for
                        .iter()
                        .take(COMPACT_PROVIDER_LIMIT)
                        .map(|value| bounded_text(value))
                        .collect::<Vec<_>>(),
                    readiness
                        .unverified_remediation
                        .as_deref()
                        .map(bounded_text),
                )
            },
        );
    let provider_total = report.provider_readiness.as_ref().map_or(0, |readiness| {
        readiness.ready_for.len() + readiness.blocked_for.len() + readiness.unverified_for.len()
    });
    let runner_id = bounded_text(&report.runner_id);
    let failed_repairs = report
        .repairs
        .iter()
        .filter(|repair| repair.status != RunnerDoctorStatus::Ok)
        .map(|repair| serde_json::json!({
            "id": bounded_text(&repair.id),
            "status": repair.status,
            "message": bounded_text(&repair.message),
            "commands": repair.commands.iter().take(1).map(|command| bounded_text(command)).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>();
    let projection = serde_json::json!({
        "schema": "homeboy/runner-doctor/v1",
        "command": report.command,
        "runner_id": runner_id,
        "runner": compact_runner_summary(&report.runner),
        "status": report.status,
        "failure": report.failure.as_ref().map(compact_failure),
        "operator_summary": {
            "identity": "runner doctor",
            "state": match report.status { RunnerDoctorStatus::Ok => "ready", RunnerDoctorStatus::Warning => "degraded", RunnerDoctorStatus::Error => "blocked" },
            "risk": if failed_checks == 0 { Vec::new() } else { vec![format!("{failed_checks} check(s) need attention")] },
            "next_action": format!("homeboy runner doctor {runner_id} --full"),
        },
        "capabilities": report.capabilities,
        "resources": {
            "homeboy": { "version": bounded_text(&report.resources.homeboy.version) },
            "system": { "os": bounded_text(&report.resources.system.os), "arch": bounded_text(&report.resources.system.arch) },
            "cpu": { "count": report.resources.cpu.count },
        },
        "checks": checks,
        "repairs": failed_repairs,
        "provider_readiness": if provider_total == 0 { serde_json::Value::Null } else { serde_json::json!({ "ready_for": ready_for, "blocked_for": blocked_for, "unverified_for": unverified_for, "guidance": unverified_remediation }) },
        "truncation": {
            "checks": { "shown": checks.len(), "omitted": report.checks.len().saturating_sub(checks.len()), "evidence_ref": "runner:doctor:checks", "full_command": format!("homeboy runner doctor {runner_id} --full") },
            "provider_readiness": { "shown": ready_for.len() + blocked_for.len() + unverified_for.len(), "omitted": provider_total.saturating_sub(ready_for.len() + blocked_for.len() + unverified_for.len()), "evidence_ref": "runner:doctor:provider-readiness", "full_command": format!("homeboy runner doctor {runner_id} --full") },
            "omitted_sections": ["resource_maps", "probe_details", "diagnostics", "secret_env_migration", "daemon_recovery", "admission_summary"],
        }
    });
    projection
}

/// Every nonzero doctor result names one current failed check at the payload
/// root so the generic command-result envelope can preserve its cause.
fn ensure_failure(report: &mut RunnerDoctorOutput) {
    if report.status != RunnerDoctorStatus::Error || report.failure.is_some() {
        return;
    }
    let Some(check) = report
        .checks
        .iter()
        .find(|check| check.status == RunnerDoctorStatus::Error)
    else {
        report.failure = Some(types::RunnerDoctorFailure {
            code: "runner.doctor.readiness_error".to_string(),
            message: "Runner doctor reported an error without a failed check".to_string(),
            details: BTreeMap::from([("runner_id".to_string(), bounded_text(&report.runner_id))]),
            next_actions: vec![crate::commands::utils::response::CommandNextAction::new(
                "inspect runner doctor evidence",
                bounded_text(&format!(
                    "homeboy runner doctor {} --full",
                    report.runner_id
                )),
            )
            .with_kind(crate::commands::utils::response::CommandNextActionKind::Show)],
            retryable: None,
        });
        return;
    };

    let reason_code = check.details.get("reason_code").map(String::as_str);
    let code = format!(
        "runner.doctor.{}{}",
        failure_code_segment(&check.id),
        reason_code
            .map(|reason| format!(".{}", failure_code_segment(reason)))
            .unwrap_or_default(),
    );
    let mut details = BTreeMap::from([
        ("runner_id".to_string(), redacted_text(&report.runner_id)),
        ("check_id".to_string(), redacted_text(&check.id)),
    ]);
    for (key, value) in check.details.iter().take(6) {
        details.insert(redacted_text(key), redacted_text(value));
    }
    let next_actions = match check
        .remediation
        .as_deref()
        .map(str::trim)
        .filter(|command| is_homeboy_command(command))
    {
        Some(command) => {
            let kind = remediation_action_kind(command);
            vec![crate::commands::utils::response::CommandNextAction::new(
                format!(
                    "{} {}",
                    if matches!(
                        kind,
                        crate::commands::utils::response::CommandNextActionKind::Repair
                    ) {
                        "repair"
                    } else {
                        "inspect"
                    },
                    redacted_text(&check.id)
                ),
                redacted_text(command),
            )
            .with_kind(kind)]
        }
        _ => vec![crate::commands::utils::response::CommandNextAction::new(
            format!("inspect {}", redacted_text(&check.id)),
            redacted_text(&format!(
                "homeboy runner doctor {} --full",
                report.runner_id
            )),
        )
        .with_kind(crate::commands::utils::response::CommandNextActionKind::Show)],
    };
    report.failure = Some(types::RunnerDoctorFailure {
        code: redacted_text(&code),
        message: redacted_text(&check.message),
        details: details.into_iter().collect(),
        next_actions,
        retryable: None,
    });
}

fn is_homeboy_command(value: &str) -> bool {
    value.trim_start().starts_with("homeboy ")
}

fn remediation_action_kind(
    command: &str,
) -> crate::commands::utils::response::CommandNextActionKind {
    if command.starts_with("homeboy runner connect ")
        || (command.contains("homeboy runner doctor ") && command.contains(" --repair"))
    {
        crate::commands::utils::response::CommandNextActionKind::Repair
    } else {
        crate::commands::utils::response::CommandNextActionKind::Show
    }
}

fn redacted_text(value: &str) -> String {
    homeboy::core::redaction::redact_string(value)
}

fn failure_code_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn compact_failure(failure: &types::RunnerDoctorFailure) -> serde_json::Value {
    serde_json::json!({
        "code": bounded_text(&failure.code),
        "message": bounded_text(&failure.message),
        "details": failure.details.iter().take(8).map(|(key, value)| (bounded_text(key), bounded_text(value))).collect::<BTreeMap<_, _>>(),
        "next_actions": failure.next_actions.iter().take(1).map(|action| serde_json::json!({
            "label": bounded_text(&action.label),
            "command": bounded_text(&action.command),
            "kind": action.kind.as_ref(),
        })).collect::<Vec<_>>(),
        "retryable": failure.retryable,
    })
}

fn compact_remediation_action(action: Option<&types::RunnerRepairAction>) -> serde_json::Value {
    match action {
        Some(types::RunnerRepairAction::RefreshHomeboy {
            git_ref,
            allow_downgrade,
        }) => serde_json::json!({
            "action": "refresh_homeboy",
            "git_ref": git_ref.as_deref().map(bounded_text),
            "allow_downgrade": allow_downgrade,
        }),
        Some(types::RunnerRepairAction::Reconnect) => serde_json::json!({ "action": "reconnect" }),
        Some(types::RunnerRepairAction::RefreshManagedSources) => {
            serde_json::json!({ "action": "refresh_managed_sources" })
        }
        None => serde_json::Value::Null,
    }
}

fn compact_runner_summary(runner: &types::RunnerTargetSummary) -> serde_json::Value {
    serde_json::json!({
        "type": runner.target_type,
        "registry": runner.registry.as_ref().map(|registry| serde_json::json!({
            "id": bounded_text(&registry.id), "kind": registry.kind,
        })),
        "server": runner.server.as_ref().map(|server| serde_json::json!({
            "id": bounded_text(&server.id), "host": bounded_text(&server.host),
            "user": bounded_text(&server.user), "port": server.port,
            "is_localhost": server.is_localhost,
        })),
    })
}

fn bounded_projection_envelope(projection: serde_json::Value) -> serde_json::Value {
    let projection = homeboy::core::redaction::redact_json(&projection);
    if projection_envelope_bytes(&projection).is_ok_and(|bytes| bytes <= COMPACT_PROJECTION_BYTES) {
        return projection;
    }
    // Even the size-cap fallback must retain the failed repair that explains
    // why the operator should not retry a generic connect choreography.
    let repairs = projection
        .get("repairs")
        .and_then(serde_json::Value::as_array)
        .and_then(|repairs| repairs.first())
        .cloned()
        .map(|repair| vec![repair])
        .unwrap_or_default();
    let failure = projection
        .get("failure")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "schema": "homeboy/runner-doctor/v1",
        "command": "runner.doctor",
        "status": "error",
        "failure": failure,
        "operator_summary": {
            "identity": "runner doctor",
            "state": "blocked",
            "risk": ["doctor details exceed the default response budget"],
            "next_action": "homeboy runner doctor <runner-id> --full",
        },
        "checks": [],
        "repairs": repairs,
        "truncation": { "checks": { "shown": 0, "omitted": "see_full_output", "full_command": "homeboy runner doctor <runner-id> --full" } },
    })
}

fn projection_envelope_bytes(payload: &serde_json::Value) -> homeboy::core::Result<usize> {
    let data = serde_json::to_value(super::types::RunnerCommandOutput::Doctor(Box::new(
        payload.clone(),
    )))?;
    let run = compact_command_run(Ok(data), 0).with_identity(
        &crate::commands::utils::response::CommandIdentity::with_operation("runner", "doctor"),
    );
    run.stdout_bytes()
}

pub(crate) fn compact_command_run(
    stdout_result: homeboy::core::Result<serde_json::Value>,
    exit_code: i32,
) -> CommandRun {
    let summary = stdout_result.as_ref().ok().and_then(render_summary);
    CommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
        CommandPresentation {
            stdout: summary,
            stderr: None,
        },
    )
}

fn bounded_text(value: &str) -> String {
    if value.len() <= COMPACT_TEXT_LIMIT {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .find_map(|(index, _)| (index >= COMPACT_TEXT_LIMIT).then_some(index))
        .unwrap_or(value.len());
    format!("{}...", &value[..end])
}

pub(crate) fn render_summary(payload: &serde_json::Value) -> Option<String> {
    let summary = payload.get("operator_summary")?;
    let checks = payload.get("checks")?.as_array()?.len();
    Some(format!(
        "Runner doctor\nStatus: {}\nChecks shown: {checks}\nNext: {}",
        summary.get("state")?.as_str()?,
        summary.get("next_action")?.as_str()?,
    ))
}

/// A bare `--repair` is the Lab daemon recovery request emitted by runner
/// recovery guidance. Resolve it before probing so that command both diagnoses
/// and applies the repair it advertises.
pub(super) fn repair_scope(scope: RunnerDoctorScope, repair: bool) -> RunnerDoctorScope {
    if repair && scope == RunnerDoctorScope::General {
        RunnerDoctorScope::LabOffload
    } else {
        scope
    }
}

fn observes_complete_capabilities(scope: RunnerDoctorScope) -> bool {
    scope != RunnerDoctorScope::LabOffload
}

fn runner_summary(
    target_type: &'static str,
    runner: Option<&Runner>,
    server: Option<&Server>,
) -> types::RunnerTargetSummary {
    types::RunnerTargetSummary {
        target_type,
        registry: runner.map(|runner| types::RunnerRegistrySummary {
            id: runner.id.clone(),
            kind: runner.kind.clone(),
        }),
        server: server.map(|server| types::RunnerServerSummary {
            id: server.id.clone(),
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            is_localhost: matches!(server.host.as_str(), "localhost" | "127.0.0.1" | "::1"),
        }),
    }
}

fn normalized_extension_ids(extension_ids: &[String]) -> Vec<String> {
    let mut ids = extension_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn normalized_required_tools(commands: &[String]) -> Vec<String> {
    let mut tools = commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    tools
}

#[cfg(test)]
mod tests;
