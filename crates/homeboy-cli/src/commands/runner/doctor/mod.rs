use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::agents::agent_tasks::provider::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
};
use homeboy::core::engine::shell;
use homeboy::core::server::{self, Server, SshClient};
use homeboy::runner::runners::{
    self as runner, daemon_repair_codes, Runner, RunnerKind, RunnerSession, RunnerToolRegistry,
    RunnerToolSpec, RunnerTunnelMode,
};
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
    let mut report = match &target {
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
    };

    let migration = runner::secret_env_migration_plan(runner_id)?;
    report.secret_env_migration = (!migration.is_empty()).then_some(migration);

    if options.repair {
        repair::apply(&target, &options, &mut report);
    }

    // Doctor probes the runner directly. Its observation supersedes any
    // process-local capability answer, so the next execution preflight probes
    // the exact command environment rather than replaying stale remediation.
    runner::observe_runner_capabilities(runner_id);
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
    let exit_code = report.status.operational_exit_code();
    Ok((report, exit_code))
}

const COMPACT_CHECK_LIMIT: usize = 12;
const COMPACT_PROVIDER_LIMIT: usize = 10;
const COMPACT_TEXT_LIMIT: usize = 256;
const COMPACT_PROJECTION_BYTES: usize = 8 * 1024;

/// Keep default doctor output to the facts needed to decide whether the runner
/// is usable. `--full` remains a lossless, redacted evidence surface.
pub(crate) fn output_projection(report: RunnerDoctorOutput, full: bool) -> serde_json::Value {
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
            })
        })
        .collect::<Vec<_>>();
    let (ready_for, blocked_for) = report.provider_readiness.as_ref().map_or_else(
        || (Vec::new(), Vec::new()),
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
            )
        },
    );
    let provider_total = report.provider_readiness.as_ref().map_or(0, |readiness| {
        readiness.ready_for.len() + readiness.blocked_for.len()
    });
    let runner_id = bounded_text(&report.runner_id);
    let projection = serde_json::json!({
        "schema": "homeboy/runner-doctor/v1",
        "command": report.command,
        "runner_id": runner_id,
        "runner": compact_runner_summary(&report.runner),
        "status": report.status,
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
        "provider_readiness": if provider_total == 0 { serde_json::Value::Null } else { serde_json::json!({ "ready_for": ready_for, "blocked_for": blocked_for }) },
        "truncation": {
            "checks": { "shown": checks.len(), "omitted": report.checks.len().saturating_sub(checks.len()), "evidence_ref": "runner:doctor:checks", "full_command": format!("homeboy runner doctor {runner_id} --full") },
            "provider_readiness": { "shown": ready_for.len() + blocked_for.len(), "omitted": provider_total.saturating_sub(ready_for.len() + blocked_for.len()), "evidence_ref": "runner:doctor:provider-readiness", "full_command": format!("homeboy runner doctor {runner_id} --full") },
            "omitted_sections": ["resource_maps", "probe_details", "diagnostics", "repairs", "secret_env_migration", "daemon_recovery", "admission_summary"],
        }
    });
    projection
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
    serde_json::json!({
        "schema": "homeboy/runner-doctor/v1",
        "command": "runner.doctor",
        "status": "error",
        "operator_summary": {
            "identity": "runner doctor",
            "state": "blocked",
            "risk": ["doctor details exceed the default response budget"],
            "next_action": "homeboy runner doctor <runner-id> --full",
        },
        "checks": [],
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
