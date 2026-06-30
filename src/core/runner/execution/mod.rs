//! Runner command execution: transports, process preparation, secret handling,
//! redaction, and failure-context construction.
//!
//! Split into focused submodules to keep each concern under the structural
//! thresholds. The public/in-crate surface is preserved via the re-exports
//! below so callers continue to reference `runner::execution::<item>`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command_contract::RunnerWorkload;
use crate::core::api_jobs::{Job, JobArtifactMetadata, JobEvent, JobStatus};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::error::{Error, Result};
use crate::core::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationPlan, RunnerExecutionArtifactRef,
    RunnerExecutionNextAction, RunnerExecutionRecord,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
};
use crate::core::source_snapshot::SourceSnapshot;

use super::resource_metrics::RunnerResourceMetrics;
use super::{
    connect, select_runner_transport, status, Runner, RunnerCapabilityPreflight, RunnerHandoff,
    RunnerJob, RunnerKind, RunnerMutationArtifacts, RunnerResult, RunnerSession, RunnerTransport,
};

const DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS: u64 = 20 * 60;
pub(crate) const RUNNER_EXEC_WAIT_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_EXEC_WAIT_TIMEOUT_SECS";
/// Opt-in: when set to a truthy value, a controller-side wait-timeout best-effort
/// cancels the still-running remote runner job (freeing its rig lock) instead of
/// only mirroring it. Off by default — the default contract leaves the remote job
/// in flight and uncancelled (#6891).
pub(crate) const RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_CANCEL_ON_WAIT_TIMEOUT";
pub(crate) const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";
pub(crate) const RUNNER_ID_ENV: &str = "HOMEBOY_RUNNER_ID";

mod broker;
mod daemon;
mod daemon_api;
mod extension_parity;
mod failure;
mod handoff;
mod paths;
mod policy;
mod process;
mod redaction;
mod secrets;
mod worker;

#[cfg(test)]
mod tests;

use extension_parity::{required_extensions_for_command, validate_runner_extension_parity};
use policy::remote_execution_preflight;

// Cross-submodule visibility: re-export each submodule's `pub(super)` surface so
// siblings reach one another through `use super::*` without widening the public
// API. `use` re-exports are not counted as structural items.
use broker::*;
use daemon::*;
use daemon_api::*;
use failure::*;
use handoff::*;
use paths::*;
use process::*;
use redaction::*;
use secrets::*;

// Crate-internal surface consumed by sibling `runner` modules (evidence, worker,
// lab_env, lab/offload) and re-exported by the parent `runner` module.
pub(crate) use daemon::result_event_data;
pub(crate) use daemon_api::{canonical_daemon_body, daemon_api_get};
pub(crate) use failure::{
    append_failure_context_error_summary, runner_exec_failure_context_from_output,
    runner_exec_failure_context_remediation_hint,
};
pub(crate) use handoff::lab_offload_handoff_hints;
pub(crate) use process::{
    execute_runner_process_until_cancelled_with_progress, prepare_daemon_local_process,
};
pub(crate) use secrets::runner_exec_secret_env_names;
pub(crate) use worker::exec_worker_local_until_cancelled_with_progress;

// Public surface re-exported by the parent `runner` module. These mirror the
// pre-split `pub` items so external callers keep referencing them unchanged.
pub use daemon_api::daemon_api_post;
pub use failure::runner_exec_failure_error;
pub use handoff::runner_job_cancel;

#[derive(Debug, Clone)]
pub struct RunnerExecOptions {
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub allow_diagnostic_ssh: bool,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub capture_patch: bool,
    pub raw_exec: bool,
    pub source_snapshot: Option<SourceSnapshot>,
    pub capability_preflight: Option<RunnerCapabilityPreflight>,
    pub required_extensions: Vec<String>,
    pub require_paths: Vec<String>,
    pub runner_workload: Option<RunnerWorkload>,
    pub run_id: Option<String>,
    pub detach_after_handoff: bool,
    pub mirror_evidence: bool,
    pub print_handoff: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExecMode {
    Daemon,
    Local,
    ReverseBroker,
    DiagnosticSsh,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerExecOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    pub mode: RunnerExecMode,
    pub argv: Vec<String>,
    pub remote_cwd: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job: Option<RunnerJob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_events: Option<Vec<JobEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_artifacts: Option<RunnerMutationArtifacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_outputs: Vec<RunnerExecPromotedOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structured_summaries: Vec<RunnerExecStructuredSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerResourceMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CommandCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_record: Option<RunnerExecutionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_result: Option<RunnerResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<RunnerHandoff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunnerExecDiagnostics>,
}

fn runner_execution_record_for_output(
    runner: &Runner,
    transport: &str,
    exit_code: i32,
    job_id: Option<String>,
    mirror_run_id: Option<String>,
    source_snapshot: Option<&SourceSnapshot>,
    require_paths: &[String],
    artifacts: &[JobArtifactMetadata],
    runner_result: Option<&RunnerResult>,
) -> RunnerExecutionRecord {
    let execution_id = job_id
        .clone()
        .or_else(|| mirror_run_id.clone())
        .unwrap_or_else(|| format!("runner-exec:{}:{}", runner.id, transport));
    let mut artifact_refs = job_artifact_refs(artifacts);
    if let Some(result) = runner_result {
        artifact_refs.extend(result.artifact_refs.iter().map(|artifact| {
            RunnerExecutionArtifactRef {
                id: artifact.artifact_id.clone(),
                name: artifact.name.clone(),
                path: artifact.path.clone(),
                url: artifact.url.clone(),
            }
        }));
    }
    artifact_refs.sort_by(|left, right| left.id.cmp(&right.id));
    artifact_refs.dedup_by(|left, right| left.id == right.id);

    let mut record = RunnerExecutionRecord::terminal(
        execution_id,
        runner.id.clone(),
        transport.to_string(),
        exit_code,
    )
    .with_mirror_run_id(mirror_run_id)
    .with_path_materialization_plan(path_materialization_plan(source_snapshot, require_paths))
    .with_artifact_refs(artifact_refs);
    if let Some(job_id) = job_id {
        record = record
            .with_job_id(job_id.clone())
            .with_next_actions(runner_execution_next_actions(&runner.id, &job_id));
    }
    record
}

fn path_materialization_plan(
    source_snapshot: Option<&SourceSnapshot>,
    require_paths: &[String],
) -> Option<PathMaterializationPlan> {
    let mut entries = Vec::new();
    if let Some(snapshot) = source_snapshot.and_then(path_materialization_entry_from_snapshot) {
        entries.push(snapshot);
    }
    entries.extend(require_paths.iter().filter_map(|path| {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(PathMaterializationEntry::required_existing_remote(trimmed))
    }));
    PathMaterializationPlan::non_empty(entries)
}

fn path_materialization_entry_from_snapshot(
    snapshot: &SourceSnapshot,
) -> Option<PathMaterializationEntry> {
    let remote_path = snapshot.remote_path.as_deref()?.trim();
    if remote_path.is_empty() {
        return None;
    }
    Some(PathMaterializationEntry::primary_workspace_materialized(
        PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT,
        snapshot.local_path.clone(),
        remote_path,
        snapshot.sync_mode.clone(),
    ))
}

fn job_artifact_refs(artifacts: &[JobArtifactMetadata]) -> Vec<RunnerExecutionArtifactRef> {
    artifacts
        .iter()
        .map(|artifact| RunnerExecutionArtifactRef {
            id: artifact.id.clone(),
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            url: artifact.url.clone(),
        })
        .collect()
}

fn runner_execution_next_actions(runner_id: &str, job_id: &str) -> Vec<RunnerExecutionNextAction> {
    vec![
        RunnerExecutionNextAction {
            label: "runner_job_logs".to_string(),
            command: vec![
                "homeboy".to_string(),
                "runner".to_string(),
                "job".to_string(),
                "logs".to_string(),
                runner_id.to_string(),
                job_id.to_string(),
            ],
        },
        RunnerExecutionNextAction {
            label: "runner_job_follow".to_string(),
            command: vec![
                "homeboy".to_string(),
                "runner".to_string(),
                "job".to_string(),
                "logs".to_string(),
                runner_id.to_string(),
                job_id.to_string(),
                "--follow".to_string(),
            ],
        },
    ]
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunnerExecPromotedOutput {
    pub role: String,
    pub run_id: String,
    pub runner_id: String,
    pub command: Vec<String>,
    pub declared_path: String,
    pub runner_path: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub artifact_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunnerExecStructuredSummary {
    pub run_id: String,
    pub runner_id: String,
    pub command: Vec<String>,
    pub declared_path: String,
    pub artifact_id: String,
    pub artifact_path: String,
    pub summary: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecFailureContext {
    pub schema: &'static str,
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_run_id: Option<String>,
    pub command: Vec<String>,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_field: Option<String>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<Value>,
}

pub(super) struct RunnerExecFailureContextInput<'a> {
    pub(super) runner_id: &'a str,
    pub(super) job_id: Option<&'a str>,
    pub(super) persisted_run_id: Option<&'a str>,
    pub(super) command: &'a [String],
    pub(super) exit_code: i32,
    pub(super) result: Option<&'a Value>,
    pub(super) stdout: &'a str,
    pub(super) stderr: &'a str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_snapshot_remote_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_binaries: Option<RunnerExecHomeboyBinaries>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecHomeboyBinaries {
    pub controller_cli: RunnerExecHomeboyBinary,
    pub active_daemon: RunnerExecHomeboyBinary,
    pub job_command_binary: RunnerExecHomeboyBinary,
    pub guidance: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecHomeboyBinary {
    pub owner: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<String>,
}

const RUNNER_EXEC_RUN_ID_ENV_NAMES: &[&str] = &[
    crate::core::observation::ACTIVE_RUN_ID_ENV,
    "HOMEBOY_RUN_ID",
    "HOMEBOY_BENCH_RUN_ID",
];

const RUNNER_EXEC_SCRUBBED_RUN_ID_ENV_NAMES: &[&str] = &["WORKFLOW_BENCH_RUN_ID"];

#[derive(Debug, Clone)]
pub(crate) struct RunnerProcessRequest {
    pub runner_id: String,
    pub runner: Option<Runner>,
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub capture_patch: bool,
    pub raw_exec: bool,
    pub source_snapshot: Option<SourceSnapshot>,
    pub require_paths: Vec<String>,
    pub validate_require_paths_on_host: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRunnerProcess {
    pub runner: Runner,
    pub cwd: String,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub source_snapshot: SourceSnapshot,
    pub require_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DaemonEnvelope {
    pub(super) success: bool,
    pub(super) data: Option<Value>,
    pub(super) error: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonJobHandoffState {
    InFlight,
    Terminal(JobStatus),
}

pub(crate) struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub metrics: Option<RunnerResourceMetrics>,
    pub capture: Option<CommandCaptureMetadata>,
}

pub fn exec(runner_id: &str, options: RunnerExecOptions) -> Result<(RunnerExecOutput, i32)> {
    if options.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let secret_env_names = runner_exec_secret_env_names(
        &options.command,
        options.capability_preflight.as_ref(),
        &options.secret_env_names,
        &options.env,
    );
    let mut plan = prepare_runner_process(RunnerProcessRequest {
        runner_id: runner_id.to_string(),
        runner: None,
        cwd: options.cwd.clone(),
        project_id: options.project_id.clone(),
        command: options.command.clone(),
        env: options.env.clone(),
        secret_env_names: secret_env_names.clone(),
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
        source_snapshot: options.source_snapshot.clone(),
        require_paths: options.require_paths.clone(),
        validate_require_paths_on_host: false,
    })?;
    let run_id_hint =
        apply_explicit_runner_exec_run_id_env(&mut plan.env, options.run_id.as_deref());
    let runner = plan.runner.clone();
    let cwd = plan.cwd.clone();
    let request_env = plan.env.clone();
    super::workload::validate_runner_workload_dispatch(
        options.runner_workload.as_ref(),
        runner_id,
        Some(&cwd),
        &options.command,
        &secret_env_names,
        options.capture_patch,
    )?;
    let required_extensions = required_extensions_for_command(
        &options.command,
        &super::workload::merge_runner_workload_required_extensions(
            options.required_extensions.clone(),
            options.runner_workload.as_ref(),
        ),
    );

    validate_runner_extension_parity(runner_id, &runner, &cwd, &required_extensions)?;

    // Remote capability-parity preflight: derive the contract from the command's
    // top-level executable when the caller did not supply an explicit one, so
    // remote dispatch always validates that the runner can satisfy the command
    // before starting execution instead of failing mid-run (#5093, #5422).
    let capability_preflight = super::workload::merge_runner_workload_capability_preflight(
        remote_execution_preflight(&options.command, options.capability_preflight.as_ref()),
        options.runner_workload.as_ref(),
    )?;
    let run_capability_preflight = |runner: &Runner| -> Result<()> {
        preflight_runner_capability_plan(runner, capability_preflight.as_ref(), &request_env)
    };

    if should_force_diagnostic_ssh(&runner, &options) {
        run_capability_preflight(&runner)?;
        return exec_diagnostic_ssh(
            &runner,
            cwd,
            options.command,
            request_env,
            &secret_env_names,
            options.require_paths,
        );
    }

    let mut connected = status(runner_id)?;
    if connected.connected && connected.stale_daemon.is_some() {
        let (refresh, refresh_exit_code) = connect(runner_id)?;
        if refresh_exit_code != 0 || !refresh.connected {
            return Err(Error::internal_unexpected(
                format!(
                    "runner `{runner_id}` has a stale daemon session and automatic refresh failed: {}",
                    refresh
                        .failure_message
                        .as_deref()
                        .unwrap_or("runner connect did not establish a fresh daemon session")
                )
            )
            .with_hint(format!(
                "Refresh the runner session with `homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}`."
            )));
        }
        connected = status(runner_id)?;
    }
    let result = match select_runner_transport(&runner, Some(&connected), false) {
        RunnerTransport::DirectDaemon(handle) => {
            run_capability_preflight(&runner)?;
            exec_via_daemon(
                &runner,
                handle.endpoint_url(),
                cwd,
                options.project_id,
                options.command,
                request_env,
                secret_env_names,
                options.capture_patch,
                Some(plan.source_snapshot),
                options.require_paths,
                options.runner_workload,
                options.run_id,
                options.detach_after_handoff,
                options.mirror_evidence,
                options.print_handoff,
            )
        }
        RunnerTransport::ReverseBroker(handle) => {
            run_capability_preflight(&runner)?;
            exec_via_reverse_broker(
                &runner,
                handle.endpoint_url(),
                cwd,
                options.project_id,
                options.command,
                request_env,
                secret_env_names,
                options.capture_patch,
                Some(plan.source_snapshot),
                options.require_paths,
                options.runner_workload,
                options.run_id,
                options.detach_after_handoff,
                options.mirror_evidence,
                options.print_handoff,
            )
        }
        RunnerTransport::Local => exec_local(plan),
        RunnerTransport::DiagnosticSsh => exec_diagnostic_ssh(
            &runner,
            cwd,
            options.command,
            request_env,
            &secret_env_names,
            options.require_paths,
        ),
        RunnerTransport::Unavailable => Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` or pass `--ssh` for explicit SSH diagnostics",
            Some(runner.id.clone()),
            Some(vec![
                "Daemon-backed execution preserves job metadata and artifact discovery.".to_string(),
                "SSH execution is intended for MVP diagnostics and must be explicit.".to_string(),
            ]),
        )),
    };
    result.map(|(mut output, exit_code)| {
        append_runner_exec_binary_diagnostics(&mut output, &runner, connected.session.as_ref());
        append_runner_exec_diagnostic_hint(&mut output, run_id_hint);
        (output, exit_code)
    })
}

fn apply_explicit_runner_exec_run_id_env(
    env: &mut HashMap<String, String>,
    run_id: Option<&str>,
) -> Option<String> {
    let run_id = run_id?;
    let mut ignored = Vec::new();

    for name in RUNNER_EXEC_RUN_ID_ENV_NAMES {
        if env.get(*name).is_some_and(|value| value != run_id) || ambient_env_is_present(name) {
            ignored.push(*name);
        }
        env.insert((*name).to_string(), run_id.to_string());
    }

    for name in RUNNER_EXEC_SCRUBBED_RUN_ID_ENV_NAMES {
        if env.remove(*name).is_some() || ambient_env_is_present(name) {
            ignored.push(*name);
        }
    }

    ignored.sort_unstable();
    ignored.dedup();
    (!ignored.is_empty()).then(|| {
        format!(
            "runner exec --run-id took precedence; ignored ambient/conflicting run-id env: {}.",
            ignored.join(", ")
        )
    })
}

fn runner_exec_request_metadata(run_id: Option<&str>, transport: &str) -> Value {
    let mut metadata = serde_json::json!({
        "transport": transport,
    });
    if let Some(run_id) = run_id.filter(|id| !id.trim().is_empty()) {
        metadata["durable_run_id"] = serde_json::json!(run_id);
        metadata["run_id"] = serde_json::json!(run_id);
    }
    metadata
}

fn ambient_env_is_present(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn append_runner_exec_diagnostic_hint(output: &mut RunnerExecOutput, hint: Option<String>) {
    let Some(hint) = hint else {
        return;
    };
    let diagnostics = output
        .diagnostics
        .get_or_insert_with(|| RunnerExecDiagnostics {
            runner_workspace_root: None,
            source_snapshot_remote_path: None,
            required_paths: Vec::new(),
            homeboy_binaries: None,
            hints: Vec::new(),
        });
    diagnostics.hints.push(hint);
}

fn append_runner_exec_binary_diagnostics(
    output: &mut RunnerExecOutput,
    runner: &Runner,
    session: Option<&RunnerSession>,
) {
    let diagnostics = output
        .diagnostics
        .get_or_insert_with(|| RunnerExecDiagnostics {
            runner_workspace_root: runner.workspace_root.clone(),
            source_snapshot_remote_path: None,
            required_paths: Vec::new(),
            homeboy_binaries: None,
            hints: Vec::new(),
        });
    diagnostics.homeboy_binaries = Some(runner_exec_homeboy_binaries(runner, session));
}

fn runner_exec_homeboy_binaries(
    runner: &Runner,
    session: Option<&RunnerSession>,
) -> RunnerExecHomeboyBinaries {
    let controller_identity = crate::core::build_identity::current();
    let configured_executable = runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    RunnerExecHomeboyBinaries {
        controller_cli: RunnerExecHomeboyBinary {
            owner: "operator_command",
            path: std::env::current_exe()
                .ok()
                .map(|path| path.display().to_string()),
            version: Some(controller_identity.version),
            build_identity: Some(controller_identity.display),
        },
        active_daemon: RunnerExecHomeboyBinary {
            owner: "runner_session",
            path: session.and_then(|session| session.remote_daemon_address.clone()),
            version: session.map(|session| session.homeboy_version.clone()),
            build_identity: session.and_then(|session| session.homeboy_build_identity.clone()),
        },
        job_command_binary: RunnerExecHomeboyBinary {
            owner: "runner_config.settings.homeboy_path",
            path: Some(configured_executable),
            version: None,
            build_identity: None,
        },
        guidance: "For Homeboy subcommands executed inside runner jobs, verify job_command_binary on the runner, not only this controller CLI. If active_daemon differs after refreshing homeboy_path, reconnect the runner daemon.".to_string(),
    }
}

fn should_force_diagnostic_ssh(runner: &Runner, options: &RunnerExecOptions) -> bool {
    select_runner_transport(runner, None, options.allow_diagnostic_ssh)
        == RunnerTransport::DiagnosticSsh
}
