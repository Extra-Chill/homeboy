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
use crate::core::source_snapshot::SourceSnapshot;

use super::resource_metrics::RunnerResourceMetrics;
use super::{
    connect, select_runner_transport, status, Runner, RunnerCapabilityPreflight, RunnerHandoff,
    RunnerJob, RunnerKind, RunnerMutationArtifacts, RunnerResult, RunnerTransport,
};

const DEFAULT_RUNNER_EXEC_WAIT_TIMEOUT_SECS: u64 = 20 * 60;
pub(crate) const RUNNER_EXEC_WAIT_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_EXEC_WAIT_TIMEOUT_SECS";
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
pub(crate) use process::{execute_runner_process_until_cancelled, prepare_daemon_local_process};
pub(crate) use secrets::runner_exec_secret_env_names;
pub(crate) use worker::exec_worker_local_until_cancelled;

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
    pub detach_after_handoff: bool,
    /// Explicit run label for ad hoc evidence commands. When omitted the
    /// persisted run label is derived from the command being executed instead of
    /// inheriting an unrelated workload name (#6362).
    pub run_label: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerResourceMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CommandCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_result: Option<RunnerResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<RunnerHandoff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunnerExecDiagnostics>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

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
    );
    let plan = prepare_runner_process(RunnerProcessRequest {
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
    match select_runner_transport(&runner, Some(&connected), false) {
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
                options.detach_after_handoff,
                options.run_label,
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
                options.detach_after_handoff,
                options.run_label,
            )
        }
        RunnerTransport::Local => exec_local(plan),
        RunnerTransport::DiagnosticSsh => exec_diagnostic_ssh(
            &runner,
            cwd,
            options.command,
            request_env,
            options.require_paths,
        ),
        RunnerTransport::Unavailable => Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` or pass `--ssh` for explicit SSH diagnostics",
            Some(runner.id),
            Some(vec![
                "Daemon-backed execution preserves job metadata and artifact discovery.".to_string(),
                "SSH execution is intended for MVP diagnostics and must be explicit.".to_string(),
            ]),
        )),
    }
}

fn should_force_diagnostic_ssh(runner: &Runner, options: &RunnerExecOptions) -> bool {
    select_runner_transport(runner, None, options.allow_diagnostic_ssh)
        == RunnerTransport::DiagnosticSsh
}
