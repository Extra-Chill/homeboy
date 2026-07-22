//! Durable controller-job contract for Lab staging and final dispatch.
//!
//! Detached Lab routing constructs this job after the agent-task attempt exists
//! and before workspace staging begins.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::lab_contract::{
    LabRigWorkloadKind, LabRoutingPolicy, LabRunnerWorkloadCapability, LabSecretEnvSource,
    LabSourcePathMode, LabWorkspaceModePolicy,
};
use homeboy_core::runner_execution_envelope::PathMaterializationPlan;
use homeboy_core::{Error, Result};
use sha2::{Digest, Sha256};

use crate::{LabOffloadCommand, LabOffloadRequest};

pub const LAB_STAGING_DISPATCH_JOB_TYPE: &str = "lab.staging-dispatch";
pub const LAB_STAGING_DISPATCH_VERSION: u32 = 1;
pub const LAB_STAGING_DISPATCH_SCHEMA: &str = "homeboy/lab-staging-dispatch/v1";
pub const LAB_STAGING_CHECKPOINT_SCHEMA: &str = "homeboy/lab-staging-dispatch-checkpoint/v1";
pub const LAB_STAGING_RECIPE_SCHEMA: &str = "homeboy/lab-staging-recipe/v1";
pub const LAB_STAGING_RECIPE_ATTACHMENT_KIND: &str = "lab-staging-recipe";
const DURABLE_LAB_WORKSPACE_STAGE_ATTACHMENT_KIND: &str = "durable-lab-workspace-stage";
const DURABLE_LAB_RUNTIME_STAGE_ATTACHMENT_KIND: &str = "durable-lab-runtime-stage";
const DURABLE_LAB_HYDRATION_STAGE_ATTACHMENT_KIND: &str = "durable-lab-hydration-stage";
const DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND: &str = "durable-lab-dispatch-receipt";
const DURABLE_LAB_TERMINAL_RECEIPT_ATTACHMENT_KIND: &str = "durable-lab-terminal-receipt";
const CONTROLLER_SUBMISSION_ATTEMPTS: usize = 3;

/// Serializable, owned counterpart to the static Lab command contract.
/// This is the persisted, owned command representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingCommand {
    pub hot_label: String,
    pub portable: bool,
    pub source_path_mode: LabSourcePathMode,
    pub workspace_mode_policy: LabWorkspaceModePolicy,
    pub capture_mutation_patch: bool,
    pub mutation_flag: Option<String>,
    pub extra_required_capabilities: Vec<String>,
    pub secret_env_sources: Vec<LabSecretEnvSource>,
    pub routing_policy: LabRoutingPolicy,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<LabRunnerWorkloadCapability>,
    pub workload: Option<LabStagingWorkload>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingWorkload {
    pub kind: LabRigWorkloadKind,
    pub rig_ids: Vec<String>,
    pub component: Option<String>,
    pub extension_overrides: Vec<String>,
}

impl From<&LabOffloadCommand> for LabStagingCommand {
    fn from(command: &LabOffloadCommand) -> Self {
        Self {
            hot_label: command.command.hot_label.to_string(),
            portable: command.command.is_portable(),
            source_path_mode: command.command.source_path_mode,
            workspace_mode_policy: command.command.workspace_mode_policy,
            capture_mutation_patch: command.command.capture_mutation_patch,
            mutation_flag: command.command.mutation_flag.map(str::to_string),
            extra_required_capabilities: command
                .command
                .extra_required_capabilities
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            secret_env_sources: command.command.secret_env_sources.to_vec(),
            routing_policy: command.command.routing_policy,
            required_extensions: command.required_extensions.clone(),
            required_capabilities: command.required_capabilities.clone(),
            workload: command
                .workload
                .as_ref()
                .map(|workload| LabStagingWorkload {
                    kind: workload.kind,
                    rig_ids: workload.rig_ids.clone(),
                    component: workload.component.clone(),
                    extension_overrides: workload.extension_overrides.clone(),
                }),
        }
    }
}

/// Immutable pre-staging input stored privately against an agent-task attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingRecipe {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    /// Immutable Homeboy build selected by the controller for this handoff.
    /// Optional only for recipes persisted by older controllers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_homeboy_build_identity: Option<String>,
    #[serde(default = "default_lab_staging_tunnel_mode")]
    pub tunnel_mode: crate::RunnerTunnelMode,
    pub command: LabStagingCommand,
    pub normalized_args: Vec<String>,
    pub placement: homeboy_cli_contract::Placement,
    pub allow_local_fallback: bool,
    pub allow_dirty_lab_workspace: bool,
    pub skip_deps_hydration: bool,
    pub capture_patch: bool,
    pub mutation_flag: Option<String>,
    pub detach_after_handoff: bool,
    pub output_file_requested: bool,
    /// The initiating CLI emitted the durable run id before controller-job
    /// creation when a local output was requested.
    pub local_output_run_id_emitted: bool,
    pub read_only_polling: bool,
    pub require_controller_git_bundle: bool,
    pub reuse_compatible_snapshot: bool,
    pub source_path: Option<PathBuf>,
    pub verified_cook_baseline: Option<Value>,
    /// Public override values only. Declared secret names are removed before
    /// this recipe reaches the private attachment store.
    pub job_override_env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub workspace_root: Option<String>,
}

impl LabStagingRecipe {
    pub fn from_request(
        run_id: impl Into<String>,
        runner_id: impl Into<String>,
        request: &LabOffloadRequest<'_>,
    ) -> Result<Self> {
        Self::from_request_with_transport(
            run_id,
            runner_id,
            crate::RunnerTunnelMode::DirectSsh,
            request,
            false,
        )
    }

    fn from_request_with_transport(
        run_id: impl Into<String>,
        runner_id: impl Into<String>,
        tunnel_mode: crate::RunnerTunnelMode,
        request: &LabOffloadRequest<'_>,
        local_output_run_id_emitted: bool,
    ) -> Result<Self> {
        let command = request.command.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "command",
                "Lab staging requires a resolved agent-task Lab command",
                None,
                None,
            )
        })?;
        let secret_env_names = request.job_overrides.secret_env_names.clone();
        if secret_env_names
            .iter()
            .any(|name| request.job_overrides.env.contains_key(name))
        {
            return Err(Error::validation_invalid_argument(
                "secret_env_names",
                "Lab staging declared secret identities cannot carry inline values",
                None,
                None,
            ));
        }
        let mut job_override_env = request.job_overrides.env.clone();
        for name in &secret_env_names {
            job_override_env.remove(name);
        }
        let recipe = Self {
            schema: LAB_STAGING_RECIPE_SCHEMA.to_string(),
            run_id: run_id.into(),
            runner_id: runner_id.into(),
            required_homeboy_build_identity: Some(
                homeboy_product_identity::build_identity().display,
            ),
            tunnel_mode,
            command: command.into(),
            normalized_args: request.normalized_args.to_vec(),
            placement: request.placement,
            allow_local_fallback: request.allow_local_fallback,
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            skip_deps_hydration: request.skip_deps_hydration,
            capture_patch: request.capture_patch,
            mutation_flag: request.mutation_flag.map(str::to_string),
            detach_after_handoff: request.detach_after_handoff,
            output_file_requested: request.output_file_requested,
            local_output_run_id_emitted: !request.output_file_requested
                || local_output_run_id_emitted,
            read_only_polling: request.read_only_polling,
            require_controller_git_bundle: request.require_controller_git_bundle,
            reuse_compatible_snapshot: request.reuse_compatible_snapshot,
            source_path: request.source_path.map(PathBuf::from),
            verified_cook_baseline: request.verified_cook_baseline.cloned(),
            job_override_env,
            secret_env_names,
            workspace_root: request.job_overrides.workspace_root.clone(),
        };
        recipe.validate()?;
        Ok(recipe)
    }

    fn validate(&self) -> Result<()> {
        // Canonical argv may contain the executable and global options before
        // the resolved subcommand token.
        let normalized_agent_task = self.normalized_args.iter().any(|arg| arg == "agent-task");
        if self.schema != LAB_STAGING_RECIPE_SCHEMA
            || self.run_id.trim().is_empty()
            || self.runner_id.trim().is_empty()
            || !self.command.portable
            || !self.command.hot_label.starts_with("agent-task")
            || !normalized_agent_task
        {
            return Err(Error::validation_invalid_argument(
                "lab_staging_recipe",
                "Lab staging requires its v1 schema, bound run and runner identities, and an agent-task command shape",
                None,
                None,
            ));
        }
        if self.output_file_requested && !self.local_output_run_id_emitted {
            return Err(Error::validation_invalid_argument(
                "local_output_run_id_emitted",
                "Lab staging requires the initiating CLI to emit the durable run id before controller-job creation when local output was requested",
                None,
                None,
            ));
        }
        let mut names = self.secret_env_names.clone();
        names.sort();
        names.dedup();
        if names != self.secret_env_names
            || names.iter().any(|name| {
                name.is_empty()
                    || !name.bytes().all(|byte| {
                        byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit()
                    })
            })
            || names
                .iter()
                .any(|name| self.job_override_env.contains_key(name))
        {
            return Err(Error::validation_invalid_argument(
                "secret_env_names",
                "Lab staging secret identities must be unique environment names and cannot carry inline values",
                None,
                None,
            ));
        }
        Ok(())
    }
}

/// The four identities that determine whether a handoff can execute safely.
/// This is persisted with the workspace receipt so recovery evidence names the
/// requested capability as well as the binary that actually ran the command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LabHandoffHomeboyIdentity {
    requested_build_identity: String,
    configured_command_build_identity: String,
    daemon_build_identity: String,
    /// This is populated only when the command execution itself reports it.
    /// Session and configured-binary identities are pre-dispatch evidence.
    executed_command_build_identity: Option<String>,
}

/// Execution evidence is appended only after runner admission. The current
/// runtime reports the configured command provenance, not a child-process
/// measurement, so `executed_command_build_identity` remains null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LabHandoffExecutionEvidence {
    runner_job_id: String,
    execution_record_id: String,
    command_build_identity: Option<String>,
}

fn canonical_homeboy_identity(identity: &str) -> &str {
    identity
        .trim()
        .strip_prefix("homeboy ")
        .unwrap_or(identity.trim())
}

fn handoff_build_reference(identity: &str) -> String {
    let identity = canonical_homeboy_identity(identity);
    identity
        .split_once('+')
        .map(|(_, commit)| commit.trim_end_matches("-dirty").to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| format!("v{}", identity.split('+').next().unwrap_or(identity)))
}

fn immutable_handoff_recovery_command(runner_id: &str, requested: &str) -> String {
    let reference = handoff_build_reference(requested);
    format!(
        "homeboy runner refresh-homeboy {} --ref {} --reconnect",
        homeboy_core::engine::shell::quote_arg(runner_id),
        homeboy_core::engine::shell::quote_arg(&reference),
    )
}

fn handoff_identity_error(
    runner_id: &str,
    requested: &str,
    configured: Option<&str>,
    daemon: Option<&str>,
) -> Error {
    let recovery = immutable_handoff_recovery_command(runner_id, requested);
    let mut error = Error::validation_invalid_argument(
        "runner",
        format!(
            "Lab handoff refused runner `{runner_id}` before workspace materialization because required Homeboy build `{requested}` does not match the selected runner command identity `{}` or active daemon identity `{}`",
            configured.unwrap_or("unavailable"),
            daemon.unwrap_or("unavailable"),
        ),
        Some(runner_id.to_string()),
        Some(vec![recovery.clone()]),
    );
    error.details["homeboy_handoff_identity"] = json!({
        "requested_build_identity": requested,
        "configured_command_build_identity": configured,
        "daemon_build_identity": daemon,
        "executed_command_build_identity": serde_json::Value::Null,
        "recovery_command": recovery,
    });
    error
}

fn handoff_execution_evidence(
    output: &crate::RunnerExecOutput,
    runner_job_id: &str,
) -> Option<LabHandoffExecutionEvidence> {
    let record = output.execution_record.as_ref()?;
    handoff_execution_evidence_from_record(record, runner_job_id)
}

fn handoff_execution_evidence_from_record(
    record: &homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
    runner_job_id: &str,
) -> Option<LabHandoffExecutionEvidence> {
    if record.job_id.as_deref() != Some(runner_job_id) {
        return None;
    }
    Some(LabHandoffExecutionEvidence {
        runner_job_id: runner_job_id.to_string(),
        execution_record_id: record.execution_id.clone(),
        command_build_identity: record
            .orchestration_provenance
            .as_ref()
            .and_then(|provenance| provenance.runner_command_binary.build_identity.clone()),
    })
}

fn automatic_handoff_convergence_allowed(runner: &crate::Runner) -> bool {
    runner.policy.allow_homeboy_convergence == Some(true)
}

fn handoff_refresh_options(
    runner_id: String,
    mode: crate::HomeboyBinaryRefreshMode,
    requested: &str,
) -> crate::HomeboyBinaryRefreshOptions {
    crate::HomeboyBinaryRefreshOptions {
        runner_id,
        mode,
        source: None,
        git_ref: Some(handoff_build_reference(requested)),
        target_dir: None,
        reconnect: true,
        force: false,
        // Automatic recovery must preserve #9088's monotonic upgrade contract.
        allow_downgrade: false,
        dry_run: false,
    }
}

fn default_lab_staging_tunnel_mode() -> crate::RunnerTunnelMode {
    crate::RunnerTunnelMode::DirectSsh
}

/// Owned replay inputs plus the separately loaded durable plan.
#[derive(Debug, Clone, PartialEq)]
pub struct LabStagingRequest {
    pub recipe: LabStagingRecipe,
    pub durable_agent_task_plan: homeboy_agents::agent_task_scheduler::AgentTaskPlan,
}

/// Future staging adapters consume this owned request directly. The current
/// offload runtime remains untouched until it has an owned request boundary.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LabStagingExecutionRequest {
    pub recipe: LabStagingRecipe,
    pub durable_agent_task_plan: homeboy_agents::agent_task_scheduler::AgentTaskPlan,
}

impl From<LabStagingRequest> for LabStagingExecutionRequest {
    fn from(value: LabStagingRequest) -> Self {
        Self {
            recipe: value.recipe,
            durable_agent_task_plan: value.durable_agent_task_plan,
        }
    }
}

pub fn persist_lab_staging_recipe(
    run_id: &str,
    runner_id: &str,
    request: &LabOffloadRequest<'_>,
) -> Result<LabStagingRecipe> {
    persist_lab_staging_recipe_for_transport(
        run_id,
        runner_id,
        crate::RunnerTunnelMode::DirectSsh,
        request,
        false,
    )
}

fn persist_lab_staging_recipe_for_transport(
    run_id: &str,
    runner_id: &str,
    tunnel_mode: crate::RunnerTunnelMode,
    request: &LabOffloadRequest<'_>,
    local_output_run_id_emitted: bool,
) -> Result<LabStagingRecipe> {
    let recipe = LabStagingRecipe::from_request_with_transport(
        run_id,
        runner_id,
        tunnel_mode,
        request,
        local_output_run_id_emitted,
    )?;
    homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
        run_id,
        LAB_STAGING_RECIPE_ATTACHMENT_KIND,
        &recipe,
    )
    .map(|attachment| attachment.payload)
}

/// Controller-owned admission for a detached agent-task attempt.
/// No workspace side effect occurs before this returns an accepted controller job.
pub(crate) fn submit_detached_staging(
    run_id: &str,
    runner_id: &str,
    tunnel_mode: crate::RunnerTunnelMode,
    request: &LabOffloadRequest<'_>,
) -> Result<String> {
    if !routing_ready() {
        return Err(Error::validation_invalid_argument(
            "lab_staging_adapter",
            "detached Lab offload requires a ready durable staging adapter",
            None,
            None,
        ));
    }
    let recipe = persist_lab_staging_recipe_for_transport(
        run_id,
        runner_id,
        tunnel_mode,
        request,
        request.local_output_file.is_some(),
    )?;
    let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
        LabStagingRecipe,
    >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)?;
    let plan = homeboy_agents::agent_task_lifecycle::load_controller_plan(run_id)?;
    let envelope = LabStagingDispatchEnvelope::new(
        run_id,
        runner_id,
        LabStagingInputRef::AgentTaskAttempt {
            run_id: run_id.to_string(),
            recipe: LabStagingRecipeRef::from_authority(attachment.payload_digest, &plan)?,
        },
    );
    envelope.validate()?;
    if recipe.run_id != run_id || recipe.runner_id != runner_id {
        return Err(Error::internal_unexpected(
            "persisted Lab staging recipe changed its attempt identity",
        ));
    }

    let daemon = homeboy_core::daemon::ensure_running(homeboy_core::daemon::DEFAULT_ADDR)?;
    let endpoint = format!("http://{}", daemon.address);
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build controller daemon client: {error}"))
        })?;
    let create = post_controller_job_with_retries(
        &client,
        format!("{endpoint}/controller/jobs"),
        Some(json!({
            "type": LAB_STAGING_DISPATCH_JOB_TYPE,
            "version": LAB_STAGING_DISPATCH_VERSION,
            "request": envelope,
            "idempotency_key": run_id,
        })),
        "create durable Lab staging controller job",
    )?;
    let job = create;
    validate_controller_job(&job)?;
    let job_id = job.id.to_string();
    homeboy_agents::agent_task_lifecycle::record_lab_staging_controller_job(
        run_id, runner_id, &job_id,
    )?;

    let started = post_controller_job_with_retries(
        &client,
        format!("{endpoint}/controller/jobs/{job_id}/start"),
        None,
        &format!("start durable Lab staging controller job `{job_id}`"),
    )?;
    validate_controller_job(&started)?;
    if started.id != job.id {
        return Err(Error::internal_unexpected(format!(
            "controller start returned job `{}` instead of requested job `{}`",
            started.id, job.id
        )));
    }
    Ok(job_id)
}

/// A response can be lost after the daemon accepted the request. Create uses
/// the durable run ID as its idempotency key; replaying start targets that same
/// job ID. Both operations can therefore be retried without admitting another
/// controller execution.
fn post_controller_job_with_retries(
    client: &Client,
    url: String,
    body: Option<Value>,
    operation: &str,
) -> Result<homeboy_core::api_jobs::Job> {
    let mut last_failure = None;
    for attempt in 1..=CONTROLLER_SUBMISSION_ATTEMPTS {
        let request = client.post(&url);
        let request = if let Some(body) = &body {
            request.json(body)
        } else {
            request
        };
        match request.send() {
            Ok(response) => {
                let status = response.status();
                match response.json::<Value>() {
                    Ok(value) => match controller_job_from_response(&value, status.as_u16()) {
                        Ok(job) => return Ok(job),
                        Err(error) => return Err(error),
                    },
                    Err(error) => {
                        last_failure = Some(format!("response parse failed after send: {error}"));
                    }
                }
            }
            Err(error) => last_failure = Some(format!("connection or timeout after send: {error}")),
        }
        if attempt < CONTROLLER_SUBMISSION_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(50 * attempt as u64));
        }
    }
    Err(Error::internal_unexpected(format!(
        "{operation} did not receive a valid response after {CONTROLLER_SUBMISSION_ATTEMPTS} idempotent attempts; the durable request may have been accepted, so retry this same run identity to reconcile it: {}",
        last_failure.unwrap_or_else(|| "unknown transport failure".to_string())
    ))
    .with_retryable(true))
}

fn controller_job_from_response(value: &Value, status: u16) -> Result<homeboy_core::api_jobs::Job> {
    if status >= 400 || value.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(Error::internal_unexpected(format!(
            "controller daemon rejected Lab staging job: {value}"
        )));
    }
    serde_json::from_value(
        value
            .pointer("/data/body/job")
            .cloned()
            .ok_or_else(|| Error::internal_unexpected("controller daemon response has no job"))?,
    )
    .map_err(|error| {
        Error::internal_json(error.to_string(), Some("parse controller job".to_string()))
    })
}

fn validate_controller_job(job: &homeboy_core::api_jobs::Job) -> Result<()> {
    if job.operation != format!("controller.{LAB_STAGING_DISPATCH_JOB_TYPE}")
        || !matches!(
            job.status,
            homeboy_core::api_jobs::JobStatus::Queued | homeboy_core::api_jobs::JobStatus::Running
        )
    {
        return Err(Error::internal_unexpected(
            "controller daemon returned an invalid Lab staging job identity or state",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingRecipeRef {
    pub attachment_digest: String,
    pub durable_plan_id: String,
    pub canonical_plan_digest: String,
}

fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut fields: Vec<_> = values.into_iter().collect();
            fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_json(value)))
                    .collect(),
            )
        }
        value => value,
    }
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(&canonicalize_json(serde_json::to_value(value).map_err(
        |error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize durable Lab workspace state".to_string()),
            )
        },
    )?))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize canonical durable Lab workspace state".to_string()),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Shared identity header carried by every durable Lab stage
/// (schema + run/runner identity + recipe/plan digests). The per-stage
/// `validate` methods all bind their state to the same request identity; this
/// centralizes that check so each `validate` only asserts its own extra fields.
///
/// Returns `Ok(true)` when the header matches the request, `Ok(false)` on any
/// mismatch (schema, run id, runner id, recipe digest, or plan digest), and
/// propagates a digest-computation error.
fn durable_stage_header_matches(
    schema: &str,
    expected_schema: &str,
    run_id: &str,
    runner_id: &str,
    recipe_digest: &str,
    plan_digest: &str,
    request: &LabStagingExecutionRequest,
) -> Result<bool> {
    Ok(schema == expected_schema
        && run_id == request.recipe.run_id
        && runner_id == request.recipe.runner_id
        && recipe_digest == canonical_digest(&request.recipe)?
        && plan_digest == canonical_digest(&request.durable_agent_task_plan)?)
}

/// Complete, owner-only output of the one admitted production boundary. Later
/// stages consume this attachment rather than recomputing or re-syncing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DurableLabWorkspaceStage {
    schema: String,
    run_id: String,
    runner_id: String,
    recipe_digest: String,
    plan_digest: String,
    source_snapshot_id: String,
    workspace_id: String,
    remote_cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    homeboy_handoff_identity: Option<LabHandoffHomeboyIdentity>,
    stage: Value,
}

/// Owner-only receipt for all runtime side effects. The workspace attachment
/// supplies inputs; this attachment is the immutable recovery authority once
/// rig, extension, and runtime publication have completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DurableLabRuntimeStage {
    schema: String,
    run_id: String,
    runner_id: String,
    recipe_digest: String,
    plan_digest: String,
    workspace_id: String,
    runtime_id: String,
    output: Value,
}

/// Immutable receipt for the dependency boundary. `dispatch_inputs` is the
/// complete post-hydration handoff: later stages consume it rather than loading
/// mutable runner state or replaying hydration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DurableLabHydrationStage {
    schema: String,
    run_id: String,
    runner_id: String,
    recipe_digest: String,
    plan_digest: String,
    workspace_id: String,
    runtime_id: String,
    hydration_id: String,
    plan: Value,
    hydration_record: Value,
    hydration_metadata: Value,
    execution_ids: Vec<String>,
    dispatch_inputs: Value,
}

/// Immutable private proof of the accepted child. Secrets and admission tokens
/// stay process-local; recovery retains only stable identities and digests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableLabDispatchReceipt {
    schema: String,
    run_id: String,
    runner_id: String,
    recipe_digest: String,
    workspace_attachment_digest: String,
    runtime_attachment_digest: String,
    hydration_attachment_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    daemon_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reservation_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reverse_submission_key: Option<String>,
    runner_job_id: String,
    remote_workspace: String,
    remote_command: Vec<String>,
    workload_identity: String,
    /// Optional for receipts persisted before handoff execution evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    homeboy_handoff_execution_evidence: Option<LabHandoffExecutionEvidence>,
}

/// Written only after the runner's terminal state has been read and projected
/// into the controller lifecycle. It is intentionally identity/status-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableLabTerminalReceipt {
    schema: String,
    run_id: String,
    runner_id: String,
    runner_job_id: String,
    status: String,
}

impl DurableLabTerminalReceipt {
    fn validate(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        if self.schema != "homeboy/durable-lab-terminal-receipt/v1"
            || self.run_id != request.recipe.run_id
            || self.runner_id != request.recipe.runner_id
            || checkpoint.final_runner_job_id.as_deref() != Some(self.runner_job_id.as_str())
            || !matches!(self.status.as_str(), "succeeded" | "failed" | "cancelled")
        {
            return Err(Error::validation_invalid_argument("durable_terminal_receipt", "Lab staging terminal receipt is missing, tampered, or does not match the authoritative runner job", None, None));
        }
        Ok(())
    }
}

impl DurableLabDispatchReceipt {
    fn validate(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        let authority_valid = match request.recipe.tunnel_mode {
            crate::RunnerTunnelMode::DirectSsh => {
                let live_admission = self
                    .daemon_lease_id
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && self
                        .reservation_job_id
                        .as_deref()
                        .is_some_and(|value| !value.is_empty());
                let recovered_admission =
                    if self.daemon_lease_id.is_none() && self.reservation_job_id.is_none() {
                        homeboy_agents::agent_task_lifecycle::accepted_lab_runner_job_identity(
                            &request.recipe.run_id,
                        )?
                        .is_some_and(|identity| {
                            identity.run_id == request.recipe.run_id
                                && identity.runner_id == request.recipe.runner_id
                                && identity.runner_job_id == self.runner_job_id
                        })
                    } else {
                        false
                    };
                (live_admission || recovered_admission) && self.reverse_submission_key.is_none()
            }
            crate::RunnerTunnelMode::Reverse => {
                self.daemon_lease_id.is_none()
                    && self.reservation_job_id.is_none()
                    && self.reverse_submission_key.as_deref().is_some_and(|value| {
                        value
                            == crate::execution::reverse_broker_submission_key(
                                &request.recipe.runner_id,
                                &request.recipe.run_id,
                            )
                    })
            }
        };
        if self.schema != "homeboy/durable-lab-dispatch-receipt/v1"
            || self.run_id != request.recipe.run_id
            || self.runner_id != request.recipe.runner_id
            || self.recipe_digest != canonical_digest(&request.recipe)?
            || self.workspace_attachment_digest.is_empty()
            || self.runtime_attachment_digest.is_empty()
            || self.hydration_attachment_digest.is_empty()
            || !authority_valid
            || self.runner_job_id.is_empty()
            || self.remote_workspace.is_empty()
            || self.remote_command.is_empty()
            || self.workload_identity.is_empty()
            || checkpoint
                .final_runner_job_id
                .as_deref()
                .is_some_and(|id| id != self.runner_job_id)
        {
            return Err(Error::validation_invalid_argument("durable_dispatch_receipt", "Lab staging dispatch receipt is missing, tampered, or bound to different run inputs", None, None));
        }
        Ok(())
    }
}

impl DurableLabRuntimeStage {
    fn validate(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        if !durable_stage_header_matches(
            &self.schema,
            "homeboy/durable-lab-runtime-stage/v1",
            &self.run_id,
            &self.runner_id,
            &self.recipe_digest,
            &self.plan_digest,
            request,
        )? || self.output.get("remote_cwd").and_then(Value::as_str)
            != Some(self.workspace_id.as_str())
            || self.runtime_id.trim().is_empty()
            || checkpoint.workspace_id.as_deref() != Some(self.workspace_id.as_str())
            || checkpoint
                .runtime_id
                .as_deref()
                .is_some_and(|id| id != self.runtime_id)
        {
            return Err(Error::validation_invalid_argument("durable_runtime_stage", "Lab staging durable runtime state is missing, tampered, or bound to different run inputs", None, None));
        }
        Ok(())
    }
}

impl DurableLabHydrationStage {
    fn validate(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        workspace: &DurableLabWorkspaceStage,
        runtime: &DurableLabRuntimeStage,
    ) -> Result<()> {
        let step_execution_ids = self
            .hydration_metadata
            .get("steps")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| step.get("job_id").and_then(Value::as_str))
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !durable_stage_header_matches(
            &self.schema,
            "homeboy/durable-lab-hydration-stage/v1",
            &self.run_id,
            &self.runner_id,
            &self.recipe_digest,
            &self.plan_digest,
            request,
        )? || self.workspace_id != workspace.workspace_id
            || self.runtime_id != runtime.runtime_id
            || self.hydration_id != canonical_digest(&self.hydration_record)?
            || self
                .hydration_metadata
                .get("schema")
                .and_then(Value::as_str)
                != Some(crate::lab::offload::HYDRATION_SCHEMA)
            || !matches!(
                self.hydration_metadata
                    .get("status")
                    .and_then(Value::as_str),
                Some("hydrated") | Some("skipped_no_provider") | Some("not_applied")
            )
            || self.hydration_metadata != hydration_metadata_from_record(&self.hydration_record)
            || self.execution_ids != step_execution_ids
            || self.dispatch_inputs.get("workspace_stage") != Some(&workspace.stage)
            || self.dispatch_inputs.get("runtime_output") != Some(&runtime.output)
            || self.dispatch_inputs.get("plan") != Some(&self.plan)
            || checkpoint.hydration_id.as_deref() != Some(self.hydration_id.as_str())
        {
            return Err(Error::validation_invalid_argument("durable_hydration_stage", "Lab staging durable hydration receipt is missing, tampered, or bound to different run inputs", None, None));
        }
        Ok(())
    }
}

fn hydration_metadata_from_record(record: &Value) -> Value {
    match record {
        Value::Object(value) if value.len() == 1 => match value.iter().next() {
            Some((kind, Value::Object(payload))) if kind == "NotApplied" => json!({
                "schema": crate::lab::offload::HYDRATION_SCHEMA,
                "status": "not_applied",
                "reason": payload.get("reason").cloned().unwrap_or(Value::Null),
            }),
            Some((kind, payload)) if kind == "Applied" => payload.clone(),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

impl DurableLabWorkspaceStage {
    fn validate(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        if !durable_stage_header_matches(
            &self.schema,
            "homeboy/durable-lab-workspace-stage/v1",
            &self.run_id,
            &self.runner_id,
            &self.recipe_digest,
            &self.plan_digest,
            request,
        )? || self.remote_cwd.trim().is_empty()
            || self.source_snapshot_id.trim().is_empty()
            || self.workspace_id != self.remote_cwd
            || self
                .stage
                .pointer("/source_snapshot/workspace_snapshot_identity")
                .and_then(Value::as_str)
                != Some(self.source_snapshot_id.as_str())
            || self
                .stage
                .pointer("/source_snapshot/remote_path")
                .and_then(Value::as_str)
                != Some(self.remote_cwd.as_str())
        {
            return Err(Error::validation_invalid_argument("durable_workspace_stage", "Lab staging durable workspace state is missing, tampered, or bound to different run inputs", None, None));
        }
        if let (Some(source), Some(workspace)) =
            (&checkpoint.source_snapshot_id, &checkpoint.workspace_id)
        {
            if source != &self.source_snapshot_id || workspace != &self.workspace_id {
                return Err(Error::validation_invalid_argument(
                    "checkpoint",
                    "Lab staging checkpoint does not match its durable workspace state",
                    None,
                    None,
                ));
            }
        }
        Ok(())
    }
}

impl LabStagingRecipeRef {
    pub fn from_authority(
        attachment_digest: String,
        plan: &homeboy_agents::agent_task_scheduler::AgentTaskPlan,
    ) -> Result<Self> {
        Ok(Self {
            attachment_digest,
            durable_plan_id: plan.plan_id.clone(),
            canonical_plan_digest: canonical_digest(plan)?,
        })
    }

    fn validate(&self) -> Result<()> {
        if !self.attachment_digest.starts_with("sha256:")
            || self.durable_plan_id.trim().is_empty()
            || !self.canonical_plan_digest.starts_with("sha256:")
        {
            return Err(Error::validation_invalid_argument(
                "recipe_ref",
                "Lab staging recipe reference requires attachment, plan id, and canonical plan digests",
                None,
                None,
            ));
        }
        Ok(())
    }
}

pub fn load_lab_staging_recipe(run_id: &str) -> Result<LabStagingRequest> {
    let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
        LabStagingRecipe,
    >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)?;
    let recipe = attachment.payload;
    recipe.validate()?;
    if recipe.run_id != run_id {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "Lab staging recipe run identity does not match its requested attempt",
            None,
            None,
        ));
    }
    let record = homeboy_agents::agent_task_lifecycle::status(run_id)?;
    if record.run_id != recipe.run_id {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "Lab staging recipe must bind to the closed agent-task attempt identity",
            None,
            None,
        ));
    }
    let durable_agent_task_plan =
        homeboy_agents::agent_task_lifecycle::load_controller_plan(run_id)?;
    if durable_agent_task_plan.plan_id != record.plan_id {
        return Err(Error::validation_invalid_argument(
            "plan_id",
            "Lab staging recipe durable plan does not match its agent-task attempt",
            None,
            None,
        ));
    }
    Ok(LabStagingRequest {
        recipe,
        durable_agent_task_plan,
    })
}

fn load_validated_staging_request(
    run_id: &str,
    runner_id: &str,
    recipe_ref: &LabStagingRecipeRef,
) -> Result<LabStagingExecutionRequest> {
    recipe_ref.validate()?;
    let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
        LabStagingRecipe,
    >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)?;
    let staging = load_lab_staging_recipe(run_id)?;
    if attachment.payload_digest != recipe_ref.attachment_digest
        || staging.recipe.runner_id != runner_id
        || staging.durable_agent_task_plan.plan_id != recipe_ref.durable_plan_id
        || canonical_digest(&staging.durable_agent_task_plan)? != recipe_ref.canonical_plan_digest
    {
        return Err(Error::validation_invalid_argument(
            "recipe_ref",
            "Lab staging recovery binding does not match the authoritative recipe attachment, runner, or controller plan",
            None,
            None,
        ));
    }
    Ok(staging.into())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LabStagingInputRef {
    /// Resolution reads the controller-owned agent-task attempt record and plan.
    /// It deliberately cannot name a URI, file, or raw replay payload.
    AgentTaskAttempt {
        run_id: String,
        recipe: LabStagingRecipeRef,
    },
}

impl LabStagingInputRef {
    fn run_id(&self) -> &str {
        match self {
            Self::AgentTaskAttempt { run_id, .. } => run_id,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.run_id().trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "input.run_id",
                "Lab staging input requires a non-empty durable agent-task attempt id",
                None,
                None,
            ));
        }
        match self {
            Self::AgentTaskAttempt { recipe, .. } => recipe.validate()?,
        }
        Ok(())
    }
}

/// Immutable recipe for one agent-task attempt's Lab staging and dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingDispatchEnvelope {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub input: LabStagingInputRef,
}

impl LabStagingDispatchEnvelope {
    pub fn new(
        run_id: impl Into<String>,
        runner_id: impl Into<String>,
        input: LabStagingInputRef,
    ) -> Self {
        Self {
            schema: LAB_STAGING_DISPATCH_SCHEMA.to_string(),
            run_id: run_id.into(),
            runner_id: runner_id.into(),
            input,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != LAB_STAGING_DISPATCH_SCHEMA
            || self.run_id.trim().is_empty()
            || self.runner_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "lab_staging_dispatch",
                "Lab staging dispatch requires its v1 schema, run_id, runner_id, and durable input reference",
                None,
                None,
            ));
        }
        self.input.validate()?;
        if self.input.run_id() != self.run_id {
            return Err(Error::validation_invalid_argument(
                "input.run_id",
                "Lab staging input attempt id must match the immutable envelope run_id",
                Some(self.input.run_id().to_string()),
                None,
            ));
        }
        Ok(())
    }

    fn public_projection(&self) -> Value {
        json!({
            "schema": LAB_STAGING_DISPATCH_SCHEMA,
            "run_id": self.run_id,
            "runner_id": self.runner_id,
            "state": "accepted",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LabStagingPhase {
    AcceptedMaterializeWorkspace,
    MaterializeRuntime,
    HydrateDependencies,
    DispatchRunner,
    ObserveRunner,
    Completed,
}

/// An operation is durably declared before its remote effect. Its fixed name is
/// also the idempotency/reconciliation key used after a controller restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct LabStagingStageIntent {
    pub stage: LabStagingPhase,
    pub operation_id: String,
}

/// Private recovery state. Identities are references rather than output blobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabStagingCheckpoint {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub input: LabStagingInputRef,
    pub phase: LabStagingPhase,
    pub source_snapshot_id: Option<String>,
    pub workspace_id: Option<String>,
    pub runtime_id: Option<String>,
    pub hydration_id: Option<String>,
    pub final_runner_job_id: Option<String>,
    pub stage_intent: Option<LabStagingStageIntent>,
}

impl LabStagingCheckpoint {
    fn initial(envelope: &LabStagingDispatchEnvelope) -> Self {
        Self {
            schema: LAB_STAGING_CHECKPOINT_SCHEMA.to_string(),
            run_id: envelope.run_id.clone(),
            runner_id: envelope.runner_id.clone(),
            input: envelope.input.clone(),
            phase: LabStagingPhase::AcceptedMaterializeWorkspace,
            source_snapshot_id: None,
            workspace_id: None,
            runtime_id: None,
            hydration_id: None,
            final_runner_job_id: None,
            stage_intent: None,
        }
    }

    fn public_projection(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "runner_id": self.runner_id,
            "phase": match self.phase {
                LabStagingPhase::AcceptedMaterializeWorkspace => "accepted_materialize_workspace",
                LabStagingPhase::MaterializeRuntime => "materialize_runtime",
                LabStagingPhase::HydrateDependencies => "hydrate_dependencies",
                LabStagingPhase::DispatchRunner => "dispatch_runner",
                LabStagingPhase::ObserveRunner => "observe_runner",
                LabStagingPhase::Completed => "completed",
            },
            "final_runner_job_id": self.final_runner_job_id,
        })
    }

    fn validate(&self) -> Result<()> {
        if self.schema != LAB_STAGING_CHECKPOINT_SCHEMA
            || self.run_id.trim().is_empty()
            || self.runner_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "checkpoint",
                "Lab staging checkpoint requires its v1 schema and immutable run/runner ids",
                None,
                None,
            ));
        }
        self.input.validate()?;
        if self.input.run_id() != self.run_id {
            return Err(Error::validation_invalid_argument(
                "checkpoint.input.run_id",
                "Lab staging checkpoint input attempt id must match run_id",
                Some(self.input.run_id().to_string()),
                None,
            ));
        }
        let non_empty = |name: &str, value: &Option<String>| -> Result<()> {
            if value
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(Error::validation_invalid_argument(
                    format!("checkpoint.{name}"),
                    "Lab staging identity values must be non-empty when present",
                    None,
                    None,
                ));
            }
            Ok(())
        };
        for (name, value) in [
            ("source_snapshot_id", &self.source_snapshot_id),
            ("workspace_id", &self.workspace_id),
            ("runtime_id", &self.runtime_id),
            ("hydration_id", &self.hydration_id),
            ("final_runner_job_id", &self.final_runner_job_id),
        ] {
            non_empty(name, value)?;
        }
        if let Some(intent) = &self.stage_intent {
            let expected = self.operation_id_for_phase();
            if self.phase == LabStagingPhase::Completed
                || intent.stage != self.phase
                || intent.operation_id != expected
            {
                return Err(Error::validation_invalid_argument(
                    "checkpoint.stage_intent",
                    format!("Lab staging intent must bind the current incomplete stage to its immutable operation id (intent={:?}:{}, phase={:?}, expected={expected})", intent.stage, intent.operation_id, self.phase),
                    None,
                    None,
                ));
            }
        }
        let has = |value: &Option<String>| value.is_some();
        let required = match self.phase {
            LabStagingPhase::AcceptedMaterializeWorkspace => 0,
            LabStagingPhase::MaterializeRuntime => 2,
            LabStagingPhase::HydrateDependencies => 3,
            LabStagingPhase::DispatchRunner => 4,
            LabStagingPhase::ObserveRunner | LabStagingPhase::Completed => 5,
        };
        let identities = [
            &self.source_snapshot_id,
            &self.workspace_id,
            &self.runtime_id,
            &self.hydration_id,
            &self.final_runner_job_id,
        ];
        if identities[..required].iter().any(|value| !has(value))
            || identities[required..].iter().any(|value| has(value))
        {
            return Err(Error::validation_invalid_argument(
                "checkpoint.phase",
                "Lab staging checkpoint fields must exactly represent completed side effects before its next action",
                None,
                None,
            ));
        }
        Ok(())
    }

    fn validate_identity_matches(&self, prior: &Self) -> Result<()> {
        if self.run_id != prior.run_id
            || self.runner_id != prior.runner_id
            || self.input != prior.input
        {
            return Err(Error::validation_invalid_argument(
                "checkpoint",
                "Lab staging execution cannot change immutable run, runner, or input identity",
                Some(self.run_id.clone()),
                None,
            ));
        }
        Ok(())
    }

    fn validated_transition(prior: &Self, next: Self) -> Result<Self> {
        next.validate()?;
        next.validate_identity_matches(prior)?;
        let rank = |phase: &LabStagingPhase| match phase {
            LabStagingPhase::AcceptedMaterializeWorkspace => 0,
            LabStagingPhase::MaterializeRuntime => 1,
            LabStagingPhase::HydrateDependencies => 2,
            LabStagingPhase::DispatchRunner => 3,
            LabStagingPhase::ObserveRunner => 4,
            LabStagingPhase::Completed => 5,
        };
        if rank(&next.phase) < rank(&prior.phase) {
            return Err(Error::validation_invalid_argument(
                "checkpoint.phase",
                "Lab staging checkpoint transition cannot regress",
                None,
                None,
            ));
        }
        for (name, before, after) in [
            (
                "source_snapshot_id",
                &prior.source_snapshot_id,
                &next.source_snapshot_id,
            ),
            ("workspace_id", &prior.workspace_id, &next.workspace_id),
            ("runtime_id", &prior.runtime_id, &next.runtime_id),
            ("hydration_id", &prior.hydration_id, &next.hydration_id),
            (
                "final_runner_job_id",
                &prior.final_runner_job_id,
                &next.final_runner_job_id,
            ),
        ] {
            if before.is_some() && before != after {
                return Err(Error::validation_invalid_argument(format!("checkpoint.{name}"), "Lab staging checkpoint transition cannot remove or change an authoritative identity", None, None));
            }
        }
        Ok(next)
    }

    /// The sole side effect that may run when resuming this checkpoint.
    fn next_action(&self) -> LabStagingPhase {
        self.phase.clone()
    }

    fn operation_id_for_phase(&self) -> String {
        let stage = match self.phase {
            LabStagingPhase::AcceptedMaterializeWorkspace => "workspace",
            LabStagingPhase::MaterializeRuntime => "runtime",
            LabStagingPhase::HydrateDependencies => "hydration",
            LabStagingPhase::DispatchRunner => "dispatch",
            LabStagingPhase::ObserveRunner => "observe",
            LabStagingPhase::Completed => "completed",
        };
        format!("{}:{stage}", self.run_id)
    }

    fn intent_for_current_action(&self) -> LabStagingStageIntent {
        LabStagingStageIntent {
            stage: self.phase.clone(),
            operation_id: self.operation_id_for_phase(),
        }
    }
}

#[derive(Clone, Default)]
pub struct LabStagingCancellationToken(Arc<std::sync::atomic::AtomicBool>);

impl LabStagingCancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Lab's narrow execution seam. Its production implementation will call the
/// existing offload stages; the durable driver owns only lifecycle mechanics.
pub(crate) trait LabStagingExecutionAdapter: Send + Sync {
    fn execute(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: LabStagingCheckpoint,
        cancellation: LabStagingCancellationToken,
        job: ControllerJobHandle,
    ) -> Result<LabStagingCheckpoint>;
    fn resume(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: LabStagingCheckpoint,
        cancellation: LabStagingCancellationToken,
        job: ControllerJobHandle,
    ) -> Result<LabStagingCheckpoint> {
        self.execute(request, checkpoint, cancellation, job)
    }
    /// Must prove all owned work, including a final runner job when present,
    /// has stopped before returning successfully.
    fn cancel(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()>;
}

/// Side-effect boundary for durable Lab staging. The production implementation
/// will delegate each operation to the corresponding synchronous Lab stage;
/// keeping this narrow makes recovery execute only the incomplete operation.
trait LabStagingStageOperations: Send + Sync {
    fn validate_recorded_identities(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()>;
    /// Reconcile an effect after its intent was persisted but before its
    /// receipt/checkpoint was durably recorded. `None` proves no completion;
    /// callers may then execute the one declared operation.
    fn recover_stage(
        &self,
        _request: &LabStagingExecutionRequest,
        _checkpoint: &LabStagingCheckpoint,
        _intent: &LabStagingStageIntent,
    ) -> Result<Option<LabStagingStageEffect>> {
        Ok(None)
    }
    /// This must run immediately before the first workspace side effect. It
    /// returns the pre-dispatch evidence persisted with the workspace receipt.
    fn validate_handoff_identity(
        &self,
        _request: &LabStagingExecutionRequest,
    ) -> Result<Option<LabHandoffHomeboyIdentity>> {
        Ok(None)
    }
    fn materialize_workspace(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        handoff_identity: Option<LabHandoffHomeboyIdentity>,
    ) -> Result<(String, String)>;
    fn materialize_runtime(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String>;
    fn hydrate_dependencies(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String>;
    fn dispatch_runner(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String>;
    fn observe_runner(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        runner_job_id: &str,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<()>;
    fn recover_admitted_runner_job(
        &self,
        request: &LabStagingExecutionRequest,
    ) -> Result<Option<String>>;
    fn cancel_active_staging_jobs(&self, request: &LabStagingExecutionRequest) -> Result<usize>;
    fn prove_no_runner_admission(&self, request: &LabStagingExecutionRequest) -> Result<()>;
    fn cancel_and_reconcile_runner_job(
        &self,
        request: &LabStagingExecutionRequest,
        runner_job_id: &str,
    ) -> Result<()>;
}

#[derive(Debug)]
enum LabStagingStageEffect {
    Workspace(String, String),
    Runtime(String),
    Hydration(String),
    Dispatch(String),
    Observe,
}

/// Production implementation of the durable staging boundaries. Each remote
/// effect persists an immutable receipt before the next phase can begin.
struct ProductionLabStagingOperations;

impl ProductionLabStagingOperations {
    /// Validate the immutable handoff binding immediately before the first
    /// workspace side effect. An explicit convergence policy may refresh an
    /// idle Direct SSH runner, but recovery never authorizes a downgrade.
    fn validate_handoff_homeboy_identity(
        request: &LabStagingExecutionRequest,
    ) -> Result<Option<LabHandoffHomeboyIdentity>> {
        let Some(requested) = request.recipe.required_homeboy_build_identity.as_deref() else {
            // Mixed-version recovery: old recipes had no immutable binding.
            return Ok(None);
        };
        let mut runner = crate::load(&request.recipe.runner_id)?;
        let mut command =
            crate::remote_runner_homeboy_path(&runner, "durable Lab handoff")?.to_string();
        let mut status = Self::status_for_recipe_transport(request)?;
        let configured = crate::configured_runner_homeboy_build_identity(&runner, &command)?;
        let daemon = status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone());
        let identities_match = |configured: Option<&str>, daemon: Option<&str>| {
            configured.is_some_and(|identity| {
                canonical_homeboy_identity(identity) == canonical_homeboy_identity(requested)
            }) && daemon.is_some_and(|identity| {
                canonical_homeboy_identity(identity) == canonical_homeboy_identity(requested)
            })
        };
        if !identities_match(configured.as_deref(), daemon.as_deref())
            && automatic_handoff_convergence_allowed(&runner)
            && status.active_jobs.is_empty()
            && status.active_job_state == crate::RunnerActiveJobState::Available
            && status
                .session
                .as_ref()
                .is_some_and(|session| session.mode == crate::RunnerTunnelMode::DirectSsh)
        {
            let mode = if configured.as_deref().is_some_and(|identity| {
                canonical_homeboy_identity(identity) == canonical_homeboy_identity(requested)
            }) {
                crate::HomeboyBinaryRefreshMode::Select {
                    binary_path: command.to_string(),
                }
            } else {
                crate::HomeboyBinaryRefreshMode::Materialize
            };
            let (report, exit_code) = crate::refresh_homeboy_binary(handoff_refresh_options(
                request.recipe.runner_id.clone(),
                mode,
                requested,
            ))?;
            if exit_code == 0 && report.daemon_refreshed {
                status = Self::status_for_recipe_transport(request)?;
                runner = crate::load(&request.recipe.runner_id)?;
                command =
                    crate::remote_runner_homeboy_path(&runner, "durable Lab handoff")?.to_string();
            }
        }
        let daemon = status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone());
        let configured = crate::configured_runner_homeboy_build_identity(&runner, &command)?;
        if !identities_match(configured.as_deref(), daemon.as_deref()) {
            return Err(handoff_identity_error(
                &request.recipe.runner_id,
                requested,
                configured.as_deref(),
                daemon.as_deref(),
            ));
        }
        Ok(Some(LabHandoffHomeboyIdentity {
            requested_build_identity: requested.to_string(),
            configured_command_build_identity: configured
                .clone()
                .expect("checked configured identity"),
            daemon_build_identity: daemon.expect("checked daemon identity"),
            executed_command_build_identity: None,
        }))
    }

    fn durable_workspace(
        request: &LabStagingExecutionRequest,
    ) -> Result<homeboy_agents::agent_task_lifecycle::PrivateRunAttachment<DurableLabWorkspaceStage>>
    {
        homeboy_agents::agent_task_lifecycle::load_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_WORKSPACE_STAGE_ATTACHMENT_KIND,
        )
    }

    fn durable_runtime(
        request: &LabStagingExecutionRequest,
    ) -> Result<homeboy_agents::agent_task_lifecycle::PrivateRunAttachment<DurableLabRuntimeStage>>
    {
        homeboy_agents::agent_task_lifecycle::load_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_RUNTIME_STAGE_ATTACHMENT_KIND,
        )
    }

    fn durable_hydration(
        request: &LabStagingExecutionRequest,
    ) -> Result<homeboy_agents::agent_task_lifecycle::PrivateRunAttachment<DurableLabHydrationStage>>
    {
        homeboy_agents::agent_task_lifecycle::load_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_HYDRATION_STAGE_ATTACHMENT_KIND,
        )
    }

    /// The lifecycle's accepted identity is authoritative once daemon admission
    /// has completed. A controller crash can occur after that atomic bind but
    /// before this staging receipt is written; rebuild the receipt from the
    /// immutable stage attachments instead of submitting the provider again.
    fn recover_dispatch_receipt(
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<Option<DurableLabDispatchReceipt>> {
        if let Ok(receipt) = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
            DurableLabDispatchReceipt,
        >(
            &request.recipe.run_id,
            DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND,
        ) {
            receipt.payload.validate(request, checkpoint)?;
            return Ok(Some(receipt.payload));
        }
        let Some(identity) =
            homeboy_agents::agent_task_lifecycle::accepted_lab_runner_job_identity(
                &request.recipe.run_id,
            )?
        else {
            return Ok(None);
        };
        if identity.run_id != request.recipe.run_id
            || identity.runner_id != request.recipe.runner_id
        {
            return Err(Error::validation_invalid_argument(
                "runner_job_identity",
                "accepted Lab runner identity does not match its staging recipe",
                Some(identity.describe()),
                None,
            ));
        }
        let workspace = Self::durable_workspace(request)?;
        let runtime = Self::durable_runtime(request)?;
        let hydration = Self::durable_hydration(request)?;
        let remote_command =
            serde_json::from_value(runtime.payload.output.get("command").cloned().ok_or_else(
                || Error::internal_unexpected("durable runtime has no final command"),
            )?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_runtime_stage",
                    "Lab staging final command is invalid",
                    None,
                    None,
                )
            })?;
        let receipt = DurableLabDispatchReceipt {
            schema: "homeboy/durable-lab-dispatch-receipt/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe)?,
            workspace_attachment_digest: workspace.payload_digest,
            runtime_attachment_digest: runtime.payload_digest,
            hydration_attachment_digest: hydration.payload_digest,
            // Lease and reservation evidence authorize submission only. The
            // accepted lifecycle identity is the recovery authority.
            daemon_lease_id: None,
            reservation_job_id: None,
            reverse_submission_key: (request.recipe.tunnel_mode
                == crate::RunnerTunnelMode::Reverse)
                .then(|| {
                    crate::execution::reverse_broker_submission_key(
                        &request.recipe.runner_id,
                        &request.recipe.run_id,
                    )
                }),
            runner_job_id: identity.runner_job_id,
            remote_workspace: workspace.payload.remote_cwd,
            remote_command,
            workload_identity: canonical_digest(&hydration.payload.hydration_metadata)?,
            homeboy_handoff_execution_evidence: None,
        };
        receipt.validate(request, checkpoint)?;
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND,
            &receipt,
        )?;
        Ok(Some(receipt))
    }

    fn cancellation_error() -> Error {
        Error::internal_unexpected("Lab staging was cancelled during runtime materialization")
    }

    fn check_cancelled(cancellation: &LabStagingCancellationToken) -> Result<()> {
        (!cancellation.is_cancelled())
            .then_some(())
            .ok_or_else(Self::cancellation_error)
    }

    fn status_for_recipe_transport(
        request: &LabStagingExecutionRequest,
    ) -> Result<crate::RunnerStatusReport> {
        let status = crate::status_for_admission(&request.recipe.runner_id)?;
        let session = status.session.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "durable Lab staging requires an accepted runner session",
                Some(request.recipe.runner_id.clone()),
                None,
            )
        })?;
        if session.mode != request.recipe.tunnel_mode {
            return Err(Error::validation_invalid_argument(
                "runner",
                format!(
                    "durable Lab staging recipe requires `{}` transport, but the current session is `{}`",
                    request.recipe.tunnel_mode.metadata_value(),
                    session.mode.metadata_value(),
                ),
                Some(request.recipe.runner_id.clone()),
                None,
            ));
        }
        if session.mode == crate::RunnerTunnelMode::Reverse && session.broker_url.is_none() {
            return Err(Error::validation_invalid_argument(
                "runner",
                "durable reverse Lab staging requires its accepted broker endpoint",
                Some(request.recipe.runner_id.clone()),
                None,
            ));
        }
        Ok(status)
    }

    fn observe_runner_job_until_terminal(
        request: &LabStagingExecutionRequest,
        runner_job_id: &str,
        cancellation: Option<&LabStagingCancellationToken>,
    ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        if request.recipe.tunnel_mode == crate::RunnerTunnelMode::DirectSsh {
            let observed = crate::execution::observe_daemon_job_until_terminal(
                &request.recipe.runner_id,
                runner_job_id,
                None,
            )?;
            return Ok(homeboy_core::api_jobs::RunnerJobLogSnapshot {
                job: observed.job,
                events: observed.events,
            });
        }
        loop {
            if cancellation.is_some_and(LabStagingCancellationToken::is_cancelled) {
                return Err(Self::cancellation_error());
            }
            match crate::runner_job_log_snapshot(&request.recipe.runner_id, runner_job_id) {
                Ok(snapshot) if snapshot.job.status.is_terminal() => return Ok(snapshot),
                Ok(_) => std::thread::sleep(Duration::from_millis(200)),
                Err(mut error) => {
                    error.retryable = error.retryable.or(Some(true));
                    return Err(error);
                }
            }
        }
    }

    fn ambiguous_intent(stage: &str) -> Error {
        Error::validation_invalid_argument(
            "checkpoint.stage_intent",
            format!("Lab staging {stage} intent has no authoritative completion receipt; recovery is ambiguous and fails closed"),
            None,
            None,
        )
    }
}

impl LabStagingStageOperations for ProductionLabStagingOperations {
    fn validate_recorded_identities(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        Self::status_for_recipe_transport(request)?;
        if checkpoint.phase == LabStagingPhase::AcceptedMaterializeWorkspace {
            return Ok(());
        }
        // A recorded workspace is authoritative. A missing or tampered durable
        // attachment is a recovery failure, never permission to sync again.
        Self::durable_workspace(request)?
            .payload
            .validate(request, checkpoint)?;
        if checkpoint.runtime_id.is_some() {
            Self::durable_runtime(request)?
                .payload
                .validate(request, checkpoint)?;
        }
        if checkpoint.hydration_id.is_some() {
            let workspace = Self::durable_workspace(request)?.payload;
            let runtime = Self::durable_runtime(request)?.payload;
            Self::durable_hydration(request)?
                .payload
                .validate(request, checkpoint, &workspace, &runtime)?;
        }
        if checkpoint.final_runner_job_id.is_some() {
            Self::recover_dispatch_receipt(request, checkpoint)?
                .ok_or_else(|| Self::ambiguous_intent("dispatch"))?;
        }
        if checkpoint.phase == LabStagingPhase::Completed {
            homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                DurableLabTerminalReceipt,
            >(
                &request.recipe.run_id,
                DURABLE_LAB_TERMINAL_RECEIPT_ATTACHMENT_KIND,
            )?
            .payload
            .validate(request, checkpoint)?;
        }
        Ok(())
    }

    fn recover_stage(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        intent: &LabStagingStageIntent,
    ) -> Result<Option<LabStagingStageEffect>> {
        if intent.stage != checkpoint.phase
            || intent.operation_id != checkpoint.operation_id_for_phase()
        {
            return Err(Error::validation_invalid_argument(
                "checkpoint.stage_intent",
                "Lab staging recovery intent does not match the incomplete operation",
                None,
                None,
            ));
        }
        match checkpoint.phase {
            LabStagingPhase::AcceptedMaterializeWorkspace => {
                let workspace = Self::durable_workspace(request)
                    .map_err(|_| Self::ambiguous_intent("workspace"))?;
                let mut completed = checkpoint.clone();
                completed.phase = LabStagingPhase::MaterializeRuntime;
                completed.stage_intent = None;
                completed.source_snapshot_id = Some(workspace.payload.source_snapshot_id.clone());
                completed.workspace_id = Some(workspace.payload.workspace_id.clone());
                workspace.payload.validate(request, &completed)?;
                Ok(Some(LabStagingStageEffect::Workspace(
                    workspace.payload.source_snapshot_id,
                    workspace.payload.workspace_id,
                )))
            }
            LabStagingPhase::MaterializeRuntime => {
                let runtime = Self::durable_runtime(request)
                    .map_err(|_| Self::ambiguous_intent("runtime"))?;
                let mut completed = checkpoint.clone();
                completed.phase = LabStagingPhase::HydrateDependencies;
                completed.stage_intent = None;
                completed.runtime_id = Some(runtime.payload.runtime_id.clone());
                runtime.payload.validate(request, &completed)?;
                Ok(Some(LabStagingStageEffect::Runtime(
                    runtime.payload.runtime_id,
                )))
            }
            LabStagingPhase::HydrateDependencies => {
                let hydration = Self::durable_hydration(request)
                    .map_err(|_| Self::ambiguous_intent("hydration"))?;
                let workspace = Self::durable_workspace(request)?.payload;
                let runtime = Self::durable_runtime(request)?.payload;
                let mut completed = checkpoint.clone();
                completed.phase = LabStagingPhase::DispatchRunner;
                completed.stage_intent = None;
                completed.hydration_id = Some(hydration.payload.hydration_id.clone());
                hydration
                    .payload
                    .validate(request, &completed, &workspace, &runtime)?;
                Ok(Some(LabStagingStageEffect::Hydration(
                    hydration.payload.hydration_id,
                )))
            }
            LabStagingPhase::DispatchRunner => {
                let dispatch = Self::recover_dispatch_receipt(request, checkpoint)?
                    .ok_or_else(|| Self::ambiguous_intent("dispatch"))?;
                let mut completed = checkpoint.clone();
                completed.phase = LabStagingPhase::ObserveRunner;
                completed.stage_intent = None;
                completed.final_runner_job_id = Some(dispatch.runner_job_id.clone());
                dispatch.validate(request, &completed)?;
                Ok(Some(LabStagingStageEffect::Dispatch(
                    dispatch.runner_job_id,
                )))
            }
            LabStagingPhase::ObserveRunner => {
                let terminal = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                    DurableLabTerminalReceipt,
                >(
                    &request.recipe.run_id,
                    DURABLE_LAB_TERMINAL_RECEIPT_ATTACHMENT_KIND,
                )
                .map_err(|_| Self::ambiguous_intent("observe"))?;
                let mut completed = checkpoint.clone();
                completed.phase = LabStagingPhase::Completed;
                completed.stage_intent = None;
                terminal.payload.validate(request, &completed)?;
                Ok(Some(LabStagingStageEffect::Observe))
            }
            LabStagingPhase::Completed => Ok(Some(LabStagingStageEffect::Observe)),
        }
    }

    fn validate_handoff_identity(
        &self,
        request: &LabStagingExecutionRequest,
    ) -> Result<Option<LabHandoffHomeboyIdentity>> {
        Self::validate_handoff_homeboy_identity(request)
    }

    fn materialize_workspace(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        homeboy_handoff_identity: Option<LabHandoffHomeboyIdentity>,
    ) -> Result<(String, String)> {
        if checkpoint.phase != LabStagingPhase::AcceptedMaterializeWorkspace {
            return Err(Error::internal_unexpected(
                "Lab staging refused to re-materialize a recorded workspace",
            ));
        }
        if Self::durable_workspace(request).is_ok() {
            return Err(Error::validation_invalid_argument("durable_workspace_stage", "Lab staging already has a durable workspace attachment but no completed checkpoint", None, None));
        }
        let source_path = request.recipe.source_path.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "source_path",
                "Durable Lab workspace staging requires the recorded source path",
                None,
                None,
            )
        })?;
        if request.recipe.command.workspace_mode_policy == LabWorkspaceModePolicy::RunnerResident {
            return Err(Error::validation_invalid_argument(
                "workspace_mode_policy",
                "Runner-resident Lab commands are not implemented by durable workspace staging",
                None,
                None,
            ));
        }
        let offload_request = LabOffloadRequest {
            command: None,
            normalized_args: &request.recipe.normalized_args,
            explicit_runner: Some(&request.recipe.runner_id),
            placement: request.recipe.placement,
            allow_local_fallback: request.recipe.allow_local_fallback,
            allow_dirty_lab_workspace: request.recipe.allow_dirty_lab_workspace,
            skip_deps_hydration: request.recipe.skip_deps_hydration,
            capture_patch: request.recipe.capture_patch,
            mutation_flag: request.recipe.mutation_flag.as_deref(),
            detach_after_handoff: request.recipe.detach_after_handoff,
            output_file_requested: request.recipe.output_file_requested,
            read_only_polling: request.recipe.read_only_polling,
            local_output_file: None,
            durable_agent_task_plan: Some(&request.durable_agent_task_plan),
            source_path: Some(source_path),
            verified_cook_baseline: request.recipe.verified_cook_baseline.as_ref(),
            require_controller_git_bundle: request.recipe.require_controller_git_bundle,
            reuse_compatible_snapshot: request.recipe.reuse_compatible_snapshot,
            job_overrides: homeboy_core::lab_offload::LabJobOverrides {
                env: request.recipe.job_override_env.clone(),
                secret_env_names: request.recipe.secret_env_names.clone(),
                workspace_root: request.recipe.workspace_root.clone(),
            },
        };
        let workspace_root = offload_request.job_overrides.workspace_root.as_deref();
        let stage = crate::lab::offload::workspace_stage::prepare_lab_offload_workspace_stage(
            &offload_request,
            crate::lab::offload::workspace_stage::LabWorkspaceStageCommand::from(
                &request.recipe.command,
            ),
            crate::lab_plan::base_lab_plan(None),
            &request.recipe.runner_id,
            source_path,
            &["homeboy".to_string()],
            workspace_root,
            Some(request.recipe.run_id.clone()),
            &request.recipe.normalized_args,
            Some(&request.recipe.run_id),
        )?;
        let workspace_mapping =
            serde_json::to_value(&stage.workspace_mapping).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize durable Lab workspace mapping".to_string()),
                )
            })?;
        let mut lab_metadata = crate::lab_offload_metadata_with_workspace_mapping(
            &stage.plan,
            "durable_controller",
            Some(&request.recipe.runner_id),
            Some(request.recipe.tunnel_mode.metadata_value()),
            "offloaded",
            Some(&stage.remote_cwd),
            None,
            Some(&workspace_mapping),
        );
        crate::lab::offload::attach_lab_workspace_metadata(
            &mut lab_metadata,
            crate::lab::offload::LabWorkspaceMetadataInputs {
                source_snapshot: &stage.source_snapshot,
                legacy_path_materialization_plan: &stage.path_materialization_plan,
                primary_synced_workspace: &stage.synced,
            },
        )?;
        lab_metadata["workspace_cleanliness"] = json!({
            "allow_dirty_lab_workspace": request.recipe.allow_dirty_lab_workspace,
        });
        let mut projection =
            crate::lab::offload::workspace_stage::durable_workspace_stage_projection(&stage);
        projection["lab_dispatch_metadata"] = lab_metadata;
        projection["homeboy_handoff_identity"] =
            serde_json::to_value(&homeboy_handoff_identity).unwrap_or(serde_json::Value::Null);
        let source_snapshot_id = projection
            .pointer("/source_snapshot/workspace_snapshot_identity")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                Error::internal_unexpected(
                    "materialized Lab workspace did not report a source snapshot identity",
                )
            })?
            .to_string();
        let remote_cwd = projection
            .get("remote_cwd")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                Error::internal_unexpected("materialized Lab workspace did not report a remote cwd")
            })?
            .to_string();
        let durable = DurableLabWorkspaceStage {
            schema: "homeboy/durable-lab-workspace-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe)?,
            plan_digest: canonical_digest(&request.durable_agent_task_plan)?,
            source_snapshot_id: source_snapshot_id.clone(),
            workspace_id: remote_cwd.clone(),
            remote_cwd,
            homeboy_handoff_identity,
            stage: projection,
        };
        durable.validate(request, checkpoint)?;
        // This must succeed before the driver records a completed checkpoint.
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_WORKSPACE_STAGE_ATTACHMENT_KIND,
            &durable,
        )?;
        Ok((source_snapshot_id, durable.workspace_id))
    }

    fn materialize_runtime(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String> {
        if checkpoint.phase != LabStagingPhase::MaterializeRuntime {
            return Err(Error::internal_unexpected(
                "Lab staging refused to re-materialize a recorded runtime",
            ));
        }
        if Self::durable_runtime(request).is_ok() {
            return Err(Error::validation_invalid_argument(
                "durable_runtime_stage",
                "Lab staging already has a durable runtime attachment but no completed checkpoint",
                None,
                None,
            ));
        }
        let workspace = Self::durable_workspace(request)?.payload;
        workspace.validate(request, checkpoint)?;
        Self::check_cancelled(cancellation)?;
        let field = |name: &str| {
            workspace.stage.get(name).cloned().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    format!("Lab staging workspace state has no {name}"),
                    None,
                    None,
                )
            })
        };
        let runtime_input: crate::lab::offload::DurableLabRuntimeWorkspaceInput =
            serde_json::from_value(field("runtime_input")?).map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging runtime workspace input is invalid",
                    None,
                    None,
                )
            })?;
        let changed_args: Vec<String> = serde_json::from_value(
            workspace
                .stage
                .pointer("/changed_since/args")
                .cloned()
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "durable_workspace_stage",
                        "Lab staging workspace state has no changed arguments",
                        None,
                        None,
                    )
                })?,
        )
        .map_err(|_| {
            Error::validation_invalid_argument(
                "durable_workspace_stage",
                "Lab staging changed arguments are invalid",
                None,
                None,
            )
        })?;
        let required_extensions: Vec<String> =
            serde_json::from_value(field("runner_required_extensions")?).map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging required extensions are invalid",
                    None,
                    None,
                )
            })?;
        let remapped_args: Vec<String> =
            serde_json::from_value(field("remapped_args")?).map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging remapped arguments are invalid",
                    None,
                    None,
                )
            })?;
        let command: Vec<String> = serde_json::from_value(field("command")?).map_err(|_| {
            Error::validation_invalid_argument(
                "durable_workspace_stage",
                "Lab staging command is invalid",
                None,
                None,
            )
        })?;
        let remote_command: Vec<String> = serde_json::from_value(field("remote_command")?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging remote command is invalid",
                    None,
                    None,
                )
            })?;
        let agent_task_run_id: Option<String> = serde_json::from_value(field("agent_task_run_id")?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging agent-task identity is invalid",
                    None,
                    None,
                )
            })?;
        let plan: homeboy_core::plan::HomeboyPlan = serde_json::from_value(field("plan")?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging plan is invalid",
                    None,
                    None,
                )
            })?;
        let runner = crate::load(&request.recipe.runner_id)?;
        let homeboy_path =
            crate::remote_runner_homeboy_path(&runner, "durable Lab runtime materialization")?;
        Self::check_cancelled(cancellation)?;
        let output = crate::lab::offload::materialize_lab_runtime(
            &runner,
            homeboy_path,
            &workspace.remote_cwd,
            &changed_args,
            &runtime_input.synced_local_path,
            &runtime_input.source_snapshot,
            &runtime_input.workspace_snapshot_identity,
            &runtime_input.workspace_remaps,
            &required_extensions,
            agent_task_run_id.as_deref(),
            plan,
            remapped_args,
            command,
            remote_command,
            || Self::check_cancelled(cancellation),
        )?;
        Self::check_cancelled(cancellation)?;
        let runtime_id = output
            .runtime_generation
            .as_ref()
            .map(|generation| generation.generation_identity.clone())
            .unwrap_or_else(|| format!("runtime:workspace:{}", workspace.workspace_id));
        let durable = DurableLabRuntimeStage {
            schema: "homeboy/durable-lab-runtime-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe)?,
            plan_digest: canonical_digest(&request.durable_agent_task_plan)?,
            workspace_id: workspace.workspace_id,
            runtime_id: runtime_id.clone(),
            output: serde_json::to_value(&output).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize durable Lab runtime state".to_string()),
                )
            })?,
        };
        durable.validate(request, checkpoint)?;
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_RUNTIME_STAGE_ATTACHMENT_KIND,
            &durable,
        )?;
        Ok(runtime_id)
    }
    fn hydrate_dependencies(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String> {
        if checkpoint.phase != LabStagingPhase::HydrateDependencies {
            return Err(Error::internal_unexpected(
                "Lab staging refused to re-hydrate a recorded workspace",
            ));
        }
        if Self::durable_hydration(request).is_ok() {
            return Err(Error::validation_invalid_argument(
                "durable_hydration_stage",
                "Lab staging already has a durable hydration attachment but no completed checkpoint",
                None,
                None,
            ));
        }
        let workspace = Self::durable_workspace(request)?.payload;
        let runtime = Self::durable_runtime(request)?.payload;
        workspace.validate(request, checkpoint)?;
        runtime.validate(request, checkpoint)?;
        let local_path = workspace
            .stage
            .pointer("/runtime_input/synced_local_path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "durable_workspace_stage",
                    "Lab staging hydration requires the recorded local workspace path",
                    None,
                    None,
                )
            })?;
        let plan: homeboy_core::plan::HomeboyPlan =
            serde_json::from_value(runtime.output.get("plan").cloned().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "durable_runtime_stage",
                    "Lab staging hydration requires the recorded runtime plan",
                    None,
                    None,
                )
            })?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_runtime_stage",
                    "Lab staging recorded runtime plan is invalid",
                    None,
                    None,
                )
            })?;
        Self::check_cancelled(cancellation)?;
        let recorded = crate::lab::offload::hydrate_for_lab_workspace_exec_with_lifecycle(
            request.recipe.skip_deps_hydration,
            &request.recipe.runner_id,
            local_path,
            &workspace.remote_cwd,
            plan,
            Some(&request.recipe.run_id),
        )?;
        Self::check_cancelled(cancellation)?;
        let plan = serde_json::to_value(&recorded.hydration.plan).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize durable Lab hydration plan".to_string()),
            )
        })?;
        let hydration_record =
            serde_json::to_value(&recorded.hydration.record).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize durable Lab hydration record".to_string()),
                )
            })?;
        let hydration_metadata =
            crate::lab::offload::dependency_hydration_metadata(&recorded.hydration.record);
        let hydration_id = canonical_digest(&hydration_record)?;
        let durable = DurableLabHydrationStage {
            schema: "homeboy/durable-lab-hydration-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe)?,
            plan_digest: canonical_digest(&request.durable_agent_task_plan)?,
            workspace_id: workspace.workspace_id.clone(),
            runtime_id: runtime.runtime_id.clone(),
            hydration_id: hydration_id.clone(),
            plan: plan.clone(),
            hydration_record,
            hydration_metadata,
            execution_ids: recorded.execution_ids,
            dispatch_inputs: json!({
                "workspace_stage": workspace.stage,
                "runtime_output": runtime.output,
                "plan": plan,
            }),
        };
        // The immutable receipt must exist before the checkpoint advances.
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_HYDRATION_STAGE_ATTACHMENT_KIND,
            &durable,
        )?;
        Ok(hydration_id)
    }
    fn dispatch_runner(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<String> {
        if checkpoint.phase != LabStagingPhase::DispatchRunner {
            return Err(Error::internal_unexpected(
                "Lab staging refused to re-dispatch a recorded runner job",
            ));
        }
        let workspace = Self::durable_workspace(request)?;
        let runtime = Self::durable_runtime(request)?;
        let hydration = Self::durable_hydration(request)?;
        workspace.payload.validate(request, checkpoint)?;
        runtime.payload.validate(request, checkpoint)?;
        hydration
            .payload
            .validate(request, checkpoint, &workspace.payload, &runtime.payload)?;
        let command: Vec<String> =
            serde_json::from_value(runtime.payload.output.get("command").cloned().ok_or_else(
                || Error::internal_unexpected("durable runtime has no final command"),
            )?)
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "durable_runtime_stage",
                    "Lab staging final command is invalid",
                    None,
                    None,
                )
            })?;
        let source_snapshot = serde_json::from_value(
            workspace
                .payload
                .stage
                .get("source_snapshot")
                .cloned()
                .ok_or_else(|| {
                    Error::internal_unexpected("durable workspace has no source snapshot")
                })?,
        )
        .map_err(|_| {
            Error::validation_invalid_argument(
                "durable_workspace_stage",
                "Lab staging source snapshot is invalid",
                None,
                None,
            )
        })?;
        let path_materialization_plan: PathMaterializationPlan = serde_json::from_value(
            workspace
                .payload
                .stage
                .get("path_materialization_plan")
                .cloned()
                .ok_or_else(|| {
                    Error::internal_unexpected("durable workspace has no path materialization plan")
                })?,
        )
        .map_err(|_| {
            Error::validation_invalid_argument(
                "durable_workspace_stage",
                "Lab staging path materialization plan is invalid",
                None,
                None,
            )
        })?;
        let mut lab_metadata = workspace
            .payload
            .stage
            .get("lab_dispatch_metadata")
            .cloned()
            .ok_or_else(|| {
                Error::internal_unexpected("durable workspace has no Lab dispatch metadata")
            })?;
        lab_metadata["dependency_hydration"] = hydration.payload.hydration_metadata.clone();
        let runtime_env: Vec<(String, String)> = serde_json::from_value(
            runtime
                .payload
                .output
                .get("runtime_env")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| {
            Error::validation_invalid_argument(
                "durable_runtime_stage",
                "Lab staging runtime environment is invalid",
                None,
                None,
            )
        })?;
        let mut public_env = request.recipe.job_override_env.clone();
        public_env.extend(runtime_env);
        public_env.extend(crate::lab_env::build_lab_offload_env(&lab_metadata));
        let secret_handoff = crate::lab::secrets::build_lab_secret_env_handoff_plan(
            &request.recipe.command.secret_env_sources,
            &request.recipe.normalized_args,
            public_env,
        )?;
        let mut env = secret_handoff.env_delta.clone();
        env.insert(
            "HOMEBOY_RUNNER_PLACEMENT_RESOLVED".to_string(),
            "1".to_string(),
        );
        env.insert(
            homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV.to_string(),
            request.recipe.runner_id.clone(),
        );
        let runner_status = Self::status_for_recipe_transport(request)?;
        let session = runner_status.session.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "durable Lab dispatch requires its accepted runner session",
                Some(request.recipe.runner_id.clone()),
                None,
            )
        })?;
        Self::check_cancelled(cancellation)?;
        let (admission, daemon_authority, reverse_submission_key) = match session.mode {
            crate::RunnerTunnelMode::DirectSsh => {
                let local_url = session.local_url.as_deref().ok_or_else(|| {
                    Error::internal_unexpected("durable Lab dispatch has no daemon endpoint")
                })?;
                let lease_id = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
                    Error::internal_unexpected("durable Lab dispatch has no daemon lease")
                })?;
                let admission = crate::execution::reserve_daemon_admission(
                    &request.recipe.runner_id,
                    local_url,
                    "lab.staging-dispatch",
                    lease_id,
                    Some(&request.recipe.run_id),
                    crate::execution::DaemonAdmissionPolicy::DurableLeaseRequired,
                )?;
                let authority = admission.authority();
                authority.prove_server_owned_expiry_or_cancellation_authority()?;
                (Some(admission), Some(authority), None)
            }
            crate::RunnerTunnelMode::Reverse => (
                None,
                None,
                Some(crate::execution::reverse_broker_submission_key(
                    &request.recipe.runner_id,
                    &request.recipe.run_id,
                )),
            ),
        };
        Self::check_cancelled(cancellation)?;
        let submitted = crate::exec_with_status_snapshot(
            &request.recipe.runner_id,
            crate::RunnerExecOptions {
                cwd: Some(workspace.payload.remote_cwd.clone()),
                command: command.clone(),
                env,
                secret_env_names: secret_handoff.secret_env_names.clone(),
                secret_env_plan: Some(secret_handoff.secret_env_plan),
                capture_patch: request.recipe.capture_patch,
                source_snapshot: Some(source_snapshot),
                path_materialization_plan: Some(path_materialization_plan),
                required_extensions: request.recipe.command.required_extensions.clone(),
                run_id: Some(request.recipe.run_id.clone()),
                detach_after_handoff: true,
                mirror_evidence: true,
                ..Default::default()
            },
            Some(runner_status),
        );
        // Both daemon and broker submissions are idempotent on the immutable run
        // id. A dropped response is reconciled from active durable-job state.
        let (runner_job_id, handoff_execution_evidence) = match submitted {
            Ok((output, _)) => {
                let runner_job_id = output.job_id.clone().ok_or_else(|| {
                    Error::internal_unexpected(
                        "durable Lab acceptance returned no runner job identity",
                    )
                })?;
                let evidence = handoff_execution_evidence(&output, &runner_job_id);
                (runner_job_id, evidence)
            }
            Err(error) => crate::status(&request.recipe.runner_id)
                .ok()
                .and_then(|status| {
                    status
                        .active_runner_jobs
                        .into_iter()
                        .find(|job| job.durable_run_id.as_deref() == Some(&request.recipe.run_id))
                        .map(|job| job.job_id)
                })
                .map(|runner_job_id| (runner_job_id, None))
                .ok_or(error)?,
        };
        // Direct-daemon authority can expire while `/exec` is in flight. Reverse
        // authority is the broker's durable submission-key record itself.
        if let Some(authority) = daemon_authority.as_ref() {
            if let Err(error) = authority.prove_server_owned_expiry_or_cancellation_authority() {
                self.cancel_and_reconcile_runner_job(request, &runner_job_id)?;
                return Err(error);
            }
        }
        if cancellation.is_cancelled() {
            self.cancel_and_reconcile_runner_job(request, &runner_job_id)?;
            return Err(Self::cancellation_error());
        }
        // Lifecycle is bound before the receipt and controller checkpoint expose
        // the accepted job to recovery.
        homeboy_agents::agent_task_lifecycle::bind_accepted_lab_runner_job(
            &homeboy_core::lab_contract::RunnerJobIdentity::new(
                &request.recipe.run_id,
                &request.recipe.runner_id,
                &runner_job_id,
            ),
            &workspace.payload.remote_cwd,
            &command,
        )?;
        let receipt = DurableLabDispatchReceipt {
            schema: "homeboy/durable-lab-dispatch-receipt/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe)?,
            workspace_attachment_digest: workspace.payload_digest,
            runtime_attachment_digest: runtime.payload_digest,
            hydration_attachment_digest: hydration.payload_digest,
            daemon_lease_id: daemon_authority
                .as_ref()
                .map(|authority| authority.daemon_lease_id().to_string()),
            reservation_job_id: daemon_authority
                .as_ref()
                .map(|authority| authority.reservation_job_id().to_string()),
            reverse_submission_key,
            runner_job_id: runner_job_id.clone(),
            remote_workspace: workspace.payload.remote_cwd,
            remote_command: command,
            workload_identity: canonical_digest(&hydration.payload.hydration_metadata)?,
            homeboy_handoff_execution_evidence: handoff_execution_evidence,
        };
        receipt.validate(request, checkpoint)?;
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND,
            &receipt,
        )?;
        drop(admission);
        Ok(runner_job_id)
    }
    fn observe_runner(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
        runner_job_id: &str,
        cancellation: &LabStagingCancellationToken,
    ) -> Result<()> {
        let dispatch = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
            DurableLabDispatchReceipt,
        >(
            &request.recipe.run_id,
            DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND,
        )?;
        dispatch.payload.validate(request, checkpoint)?;
        if dispatch.payload.runner_job_id != runner_job_id {
            return Err(Error::validation_invalid_argument(
                "runner_job_id",
                "Lab staging observer job identity does not match its dispatch receipt",
                None,
                None,
            ));
        }
        // A completed receipt makes recovery idempotent and prevents a resumed
        // controller from polling or submitting a replacement child.
        if let Ok(receipt) = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
            DurableLabTerminalReceipt,
        >(
            &request.recipe.run_id,
            DURABLE_LAB_TERMINAL_RECEIPT_ATTACHMENT_KIND,
        ) {
            return receipt.payload.validate(request, checkpoint);
        }
        Self::check_cancelled(cancellation)?;
        let observed =
            Self::observe_runner_job_until_terminal(request, runner_job_id, Some(cancellation))?;
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(
            &request.recipe.run_id,
            &observed,
        )?;
        let receipt = DurableLabTerminalReceipt {
            schema: "homeboy/durable-lab-terminal-receipt/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            runner_job_id: runner_job_id.to_string(),
            status: observed.job.status.daemon_status_label().to_string(),
        };
        receipt.validate(request, checkpoint)?;
        homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
            &request.recipe.run_id,
            DURABLE_LAB_TERMINAL_RECEIPT_ATTACHMENT_KIND,
            &receipt,
        )?;
        Ok(())
    }
    fn recover_admitted_runner_job(
        &self,
        request: &LabStagingExecutionRequest,
    ) -> Result<Option<String>> {
        let Some((runner_id, runner_job_id)) =
            homeboy_agents::agent_task_lifecycle::recorded_runner_job_identity(
                &request.recipe.run_id,
            )?
        else {
            return Ok(None);
        };
        if runner_id != request.recipe.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "accepted Lab runner binding does not match its staging recipe",
                Some(runner_id),
                None,
            ));
        }
        Ok(Some(runner_job_id))
    }
    fn cancel_active_staging_jobs(&self, request: &LabStagingExecutionRequest) -> Result<usize> {
        let active_jobs = crate::status(&request.recipe.runner_id)?
            .active_runner_jobs
            .into_iter()
            .filter_map(|job| {
                job.durable_run_id.clone().and_then(|run_id| {
                    (run_id == request.recipe.run_id
                        || crate::lab::offload::is_hydration_execution_run_id(
                            &request.recipe.run_id,
                            &run_id,
                        ))
                    .then_some((job.job_id, run_id))
                })
            })
            .collect::<Vec<_>>();
        for (job_id, durable_run_id) in &active_jobs {
            if durable_run_id == &request.recipe.run_id {
                self.cancel_and_reconcile_runner_job(request, job_id)?;
                continue;
            }
            crate::execution::runner_job_cancel_projection(
                &request.recipe.runner_id,
                job_id,
                durable_run_id,
            )?;
            let observed = Self::observe_runner_job_until_terminal(request, job_id, None)?;
            if observed.job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
                return Err(Error::internal_unexpected(format!(
                    "Lab staging child cancellation for runner job `{job_id}` was not authoritative: observed `{}`",
                    observed.job.status.daemon_status_label(),
                )));
            }
        }
        Ok(active_jobs.len())
    }
    fn prove_no_runner_admission(&self, request: &LabStagingExecutionRequest) -> Result<()> {
        let status = crate::status(&request.recipe.runner_id)?;
        if status
            .active_runner_jobs
            .into_iter()
            .any(|job| job.durable_run_id.as_deref() == Some(&request.recipe.run_id))
        {
            return Err(Error::internal_unexpected("Lab staging cancellation cannot prove the strict runner admission is absent or released"));
        }
        Ok(())
    }
    fn cancel_and_reconcile_runner_job(
        &self,
        request: &LabStagingExecutionRequest,
        runner_job_id: &str,
    ) -> Result<()> {
        let (job, _) = crate::execution::runner_job_cancel_projection(
            &request.recipe.runner_id,
            runner_job_id,
            &request.recipe.run_id,
        )?;
        let observed = Self::observe_runner_job_until_terminal(request, runner_job_id, None)?;
        if observed.job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
            return Err(Error::internal_unexpected(format!(
                "Lab staging cancellation for runner job `{runner_job_id}` was not authoritative: observed `{}`",
                observed.job.status.daemon_status_label(),
            )));
        }
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(
            &request.recipe.run_id,
            &homeboy_core::api_jobs::RunnerJobLogSnapshot {
                job: observed.job,
                events: observed.events,
            },
        )?;
        // `cancel-projection` may return an already terminal cancellation. Keep
        // the result consumed so future API changes cannot silently skip it.
        if !job.status.is_terminal() {
            return Err(Error::internal_unexpected(
                "runner cancellation response was not terminal",
            ));
        }
        Ok(())
    }
}

/// Executes the durable phase machine. It owns lifecycle mechanics only: the
/// supplied operations own materialization and runner transport.
struct StageExecutionAdapter {
    operations: Arc<dyn LabStagingStageOperations>,
}

impl StageExecutionAdapter {
    fn new(operations: Arc<dyn LabStagingStageOperations>) -> Self {
        Self { operations }
    }

    fn cancellation_error() -> Error {
        Error::internal_unexpected("Lab staging was cancelled before the next durable stage")
    }

    fn execute_with_checkpoint<F>(
        &self,
        request: &LabStagingExecutionRequest,
        mut checkpoint: LabStagingCheckpoint,
        cancellation: &LabStagingCancellationToken,
        mut persist: F,
    ) -> Result<LabStagingCheckpoint>
    where
        F: FnMut(&LabStagingCheckpoint) -> Result<()>,
    {
        self.operations
            .validate_recorded_identities(request, &checkpoint)?;
        loop {
            if cancellation.is_cancelled() {
                return Err(Self::cancellation_error());
            }
            if checkpoint.phase == LabStagingPhase::Completed {
                return Ok(checkpoint);
            }
            let prior = checkpoint.clone();
            let recovering = checkpoint.stage_intent.is_some();
            let intent = checkpoint
                .stage_intent
                .clone()
                .unwrap_or_else(|| checkpoint.intent_for_current_action());
            if !recovering {
                checkpoint.stage_intent = Some(intent.clone());
                checkpoint = LabStagingCheckpoint::validated_transition(&prior, checkpoint)?;
                // Intent must survive a crash before the effect can begin.
                persist(&checkpoint)?;
            }
            let recovered = recovering
                .then(|| self.operations.recover_stage(request, &checkpoint, &intent))
                .transpose()?
                .flatten();
            let effect = match recovered {
                Some(effect) => effect,
                None => match checkpoint.next_action() {
                    LabStagingPhase::AcceptedMaterializeWorkspace => {
                        let handoff_identity =
                            self.operations.validate_handoff_identity(request)?;
                        let (source_snapshot_id, workspace_id) = self
                            .operations
                            .materialize_workspace(request, &checkpoint, handoff_identity)?;
                        LabStagingStageEffect::Workspace(source_snapshot_id, workspace_id)
                    }
                    LabStagingPhase::MaterializeRuntime => LabStagingStageEffect::Runtime(
                        self.operations
                            .materialize_runtime(request, &checkpoint, cancellation)?,
                    ),
                    LabStagingPhase::HydrateDependencies => LabStagingStageEffect::Hydration(
                        self.operations
                            .hydrate_dependencies(request, &checkpoint, cancellation)?,
                    ),
                    LabStagingPhase::DispatchRunner => LabStagingStageEffect::Dispatch(
                        self.operations
                            .dispatch_runner(request, &checkpoint, cancellation)?,
                    ),
                    LabStagingPhase::ObserveRunner => {
                        let runner_job_id =
                            checkpoint.final_runner_job_id.as_deref().ok_or_else(|| {
                                Error::internal_unexpected(
                                    "Lab staging observation has no final runner job identity",
                                )
                            })?;
                        loop {
                            if cancellation.is_cancelled() {
                                return Err(Self::cancellation_error());
                            }
                            match self.operations.observe_runner(
                                request,
                                &checkpoint,
                                runner_job_id,
                                cancellation,
                            ) {
                                Ok(()) => break,
                                Err(error) if error.retryable == Some(true) => {
                                    std::thread::sleep(Duration::from_millis(200));
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        LabStagingStageEffect::Observe
                    }
                    LabStagingPhase::Completed => return Ok(checkpoint),
                },
            };
            checkpoint = match effect {
                LabStagingStageEffect::Workspace(source_snapshot_id, workspace_id) => {
                    LabStagingCheckpoint {
                        phase: LabStagingPhase::MaterializeRuntime,
                        source_snapshot_id: Some(source_snapshot_id),
                        workspace_id: Some(workspace_id),
                        stage_intent: None,
                        ..checkpoint
                    }
                }
                LabStagingStageEffect::Runtime(runtime_id) => LabStagingCheckpoint {
                    phase: LabStagingPhase::HydrateDependencies,
                    runtime_id: Some(runtime_id),
                    stage_intent: None,
                    ..checkpoint
                },
                LabStagingStageEffect::Hydration(hydration_id) => LabStagingCheckpoint {
                    phase: LabStagingPhase::DispatchRunner,
                    hydration_id: Some(hydration_id),
                    stage_intent: None,
                    ..checkpoint
                },
                LabStagingStageEffect::Dispatch(runner_job_id) => LabStagingCheckpoint {
                    phase: LabStagingPhase::ObserveRunner,
                    final_runner_job_id: Some(runner_job_id),
                    stage_intent: None,
                    ..checkpoint
                },
                LabStagingStageEffect::Observe => LabStagingCheckpoint {
                    phase: LabStagingPhase::Completed,
                    stage_intent: None,
                    ..checkpoint
                },
            };
            if cancellation.is_cancelled() {
                return Err(Self::cancellation_error());
            }
            checkpoint = LabStagingCheckpoint::validated_transition(&prior, checkpoint)?;
            // Persist before beginning another effect, so recovery cannot repeat
            // an already admitted remote operation.
            persist(&checkpoint)?;
        }
    }
}

impl LabStagingExecutionAdapter for StageExecutionAdapter {
    fn execute(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: LabStagingCheckpoint,
        cancellation: LabStagingCancellationToken,
        job: ControllerJobHandle,
    ) -> Result<LabStagingCheckpoint> {
        self.execute_with_checkpoint(request, checkpoint, &cancellation, |checkpoint| {
            job.checkpoint(serde_json::to_value(checkpoint).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize Lab staging checkpoint".to_string()),
                )
            })?)
        })
    }

    fn cancel(
        &self,
        request: &LabStagingExecutionRequest,
        checkpoint: &LabStagingCheckpoint,
    ) -> Result<()> {
        self.operations
            .validate_recorded_identities(request, checkpoint)?;
        let runner_job_id = match checkpoint.final_runner_job_id.clone() {
            Some(job_id) => Some(job_id),
            None => self.operations.recover_admitted_runner_job(request)?,
        };
        match runner_job_id {
            Some(job_id) => self
                .operations
                .cancel_and_reconcile_runner_job(request, &job_id),
            None => {
                if self.operations.cancel_active_staging_jobs(request)? > 0 {
                    Ok(())
                } else if checkpoint.phase == LabStagingPhase::DispatchRunner
                    && checkpoint.stage_intent.is_some()
                {
                    Err(Error::internal_unexpected(
                        "Lab staging cancellation cannot prove that an in-flight dispatch was not accepted; keep the lifecycle uncertain until its immutable run identity is reconciled",
                    ))
                } else {
                    self.operations.prove_no_runner_admission(request)
                }
            }
        }
    }
}

fn adapter_slot() -> &'static Mutex<Option<Arc<dyn LabStagingExecutionAdapter>>> {
    static ADAPTER: OnceLock<Mutex<Option<Arc<dyn LabStagingExecutionAdapter>>>> = OnceLock::new();
    ADAPTER.get_or_init(|| Mutex::new(None))
}

/// Install an execution adapter after its owner has registered the driver.
pub(crate) fn install_production_execution_adapter(adapter: Arc<dyn LabStagingExecutionAdapter>) {
    *adapter_slot().lock().expect("Lab staging adapter lock") = Some(adapter);
}

pub(crate) fn require_production_adapter() -> Result<Arc<dyn LabStagingExecutionAdapter>> {
    adapter_slot()
        .lock()
        .expect("Lab staging adapter lock")
        .clone()
        .ok_or_else(|| {
            Error::internal_unexpected(
                "Lab staging controller is registered but its production execution adapter is unavailable",
            )
        })
}

/// Registration makes the durable driver available to its owner. Traffic stays
/// disabled until every subsequent stage has a production implementation.
pub(crate) fn routing_ready() -> bool {
    adapter_slot()
        .lock()
        .expect("Lab staging adapter lock")
        .is_some()
}

/// Enable the production staging implementation after generic driver
/// registration. Keeping this explicit prevents partial embedders from routing
/// detached work before they initialize the Lab runtime.
pub fn enable_production_routing() {
    install_production_execution_adapter(Arc::new(StageExecutionAdapter::new(Arc::new(
        ProductionLabStagingOperations,
    ))));
}

fn cancellations() -> &'static Mutex<HashMap<String, LabStagingCancellationToken>> {
    static CANCELLATIONS: OnceLock<Mutex<HashMap<String, LabStagingCancellationToken>>> =
        OnceLock::new();
    CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CancellationGuard(String);

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        cancellations()
            .lock()
            .expect("Lab staging cancellation lock")
            .remove(&self.0);
    }
}

pub struct LabStagingDispatchDriver;

impl LabStagingDispatchDriver {
    fn envelope(value: &Value) -> Result<LabStagingDispatchEnvelope> {
        let envelope: LabStagingDispatchEnvelope =
            serde_json::from_value(value.clone()).map_err(|error| {
                Error::validation_invalid_argument(
                    "request",
                    format!("invalid Lab staging dispatch envelope: {error}"),
                    None,
                    None,
                )
            })?;
        envelope.validate()?;
        Ok(envelope)
    }

    fn checkpoint(value: Value) -> Result<LabStagingCheckpoint> {
        let checkpoint: LabStagingCheckpoint = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "checkpoint",
                format!("invalid Lab staging checkpoint: {error}"),
                None,
                None,
            )
        })?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn execute_checkpoint(
        &self,
        checkpoint: LabStagingCheckpoint,
        job: ControllerJobHandle,
        resume: bool,
    ) -> Result<Value> {
        checkpoint.validate()?;
        let adapter = require_production_adapter()?;
        let request = match &checkpoint.input {
            LabStagingInputRef::AgentTaskAttempt { recipe, .. } => {
                load_validated_staging_request(&checkpoint.run_id, &checkpoint.runner_id, recipe)?
            }
        };
        let expected_identity = checkpoint.clone();
        let run_id = checkpoint.run_id.clone();
        let token = cancellations()
            .lock()
            .expect("Lab staging cancellation lock")
            .entry(run_id.clone())
            .or_default()
            .clone();
        let _cleanup = CancellationGuard(run_id);
        let result = if resume {
            adapter.resume(&request, checkpoint, token, job.clone())
        } else {
            adapter.execute(&request, checkpoint, token, job.clone())
        };
        let checkpoint = result.map_err(|error| {
            if !job.is_cancelled() {
                if homeboy_agents::agent_task_lifecycle::record_pre_execution_failure(
                    &request.recipe.run_id,
                    &request.durable_agent_task_plan,
                    "lab_staging_controller",
                    &error,
                )
                .is_ok()
                {
                    let _ =
                        homeboy_agents::agent_task_lifecycle::record_lab_staging_controller_failure(
                            &request.recipe.run_id,
                            &format!("{:?}", expected_identity.phase),
                            &job.job_id(),
                        );
                }
            }
            error
        })?;
        checkpoint.validate()?;
        let checkpoint =
            LabStagingCheckpoint::validated_transition(&expected_identity, checkpoint)?;
        job.checkpoint(serde_json::to_value(&checkpoint).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab staging checkpoint".to_string()),
            )
        })?)?;
        Ok(serde_json::to_value(checkpoint).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab staging result".to_string()),
            )
        })?)
    }
}

impl ControllerJobDriver for LabStagingDispatchDriver {
    fn job_type(&self) -> &'static str {
        LAB_STAGING_DISPATCH_JOB_TYPE
    }
    fn version(&self) -> u32 {
        LAB_STAGING_DISPATCH_VERSION
    }
    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(Self::envelope(request)?.public_projection())
    }
    fn public_progress(&self, progress: &Value) -> Result<Value> {
        let checkpoint = Self::checkpoint(progress.clone())?;
        Ok(checkpoint.public_projection())
    }
    fn public_result(&self, result: &Value) -> Result<Value> {
        let checkpoint = Self::checkpoint(result.clone())?;
        Ok(checkpoint.public_projection())
    }
    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: "Lab staging and dispatch failed".to_string(),
            data: json!({ "classification": "lab_staging", "code": format!("{:?}", error.code) }),
        }
    }
    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        Self::envelope(request).map(|_| ())
    }
    fn prepare(&self, request: Value) -> Result<Value> {
        let envelope = Self::envelope(&request)?;
        // Cancellation ownership is durable before prepare can race with a
        // caller cancelling a queued controller job.
        cancellations()
            .lock()
            .expect("Lab staging cancellation lock")
            .entry(envelope.run_id.clone())
            .or_default();
        // Resolve only durable, owner-controlled input before the daemon can
        // accept the job. Stage execution remains intentionally uninstalled.
        let recipe = match &envelope.input {
            LabStagingInputRef::AgentTaskAttempt { recipe, .. } => recipe,
        };
        if let Err(error) =
            load_validated_staging_request(&envelope.run_id, &envelope.runner_id, recipe)
        {
            // Prepare runs before any Lab side effect. Project malformed recipe,
            // durable-plan, and readiness failures onto the parent attempt so
            // callers receive terminal retry guidance rather than a stranded
            // controller job with an otherwise queued parent.
            if let Ok(plan) =
                homeboy_agents::agent_task_lifecycle::load_controller_plan(&envelope.run_id)
            {
                let _ = homeboy_agents::agent_task_lifecycle::record_pre_execution_failure(
                    &envelope.run_id,
                    &plan,
                    "lab_staging_prepare",
                    &error,
                );
            }
            return Err(error);
        }
        serde_json::to_value(LabStagingCheckpoint::initial(&envelope)).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab staging checkpoint".to_string()),
            )
        })
    }
    fn execute(&self, prepared: Value, job: ControllerJobHandle) -> Result<Value> {
        self.execute_checkpoint(Self::checkpoint(prepared)?, job, false)
    }
    fn resume(&self, checkpoint: Value, job: ControllerJobHandle) -> Result<Value> {
        self.execute_checkpoint(Self::checkpoint(checkpoint)?, job, true)
    }
    fn cancel(&self, prepared: &Value) -> Result<()> {
        let checkpoint = Self::checkpoint(prepared.clone())?;
        let request = match &checkpoint.input {
            LabStagingInputRef::AgentTaskAttempt { recipe, .. } => {
                load_validated_staging_request(&checkpoint.run_id, &checkpoint.runner_id, recipe)?
            }
        };
        let token = cancellations()
            .lock()
            .expect("Lab staging cancellation lock")
            .entry(checkpoint.run_id.clone())
            .or_default()
            .clone();
        let adapter = require_production_adapter()?;
        token.cancel();
        adapter.cancel(&request, &checkpoint)
    }
}

/// Registers the Lab driver once for daemon and CLI processes.
pub fn register() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    // Driver registration is intentionally independent of traffic enablement.
    // The production adapter stays uninstalled until the offload gate is
    // explicitly approved in a later change.
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(Arc::new(LabStagingDispatchDriver))
            .expect("register Lab staging controller driver");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::lab_contract::LabCommandContract;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn recipe_command() -> LabOffloadCommand {
        LabOffloadCommand {
            command: LabCommandContract::portable("agent-task run-plan", None, false, &[]),
            required_extensions: vec!["agent-runtime".to_string()],
            required_capabilities: vec![LabRunnerWorkloadCapability {
                name: "agent-task".to_string(),
                required: true,
            }],
            workload: None,
        }
    }

    fn recipe_request<'a>(
        command: Option<LabOffloadCommand>,
        args: &'a [String],
        env: HashMap<String, String>,
    ) -> LabOffloadRequest<'a> {
        LabOffloadRequest {
            command,
            normalized_args: args,
            explicit_runner: None,
            placement: homeboy_cli_contract::Placement::Lab,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: true,
            output_file_requested: false,
            read_only_polling: false,
            local_output_file: None,
            durable_agent_task_plan: None,
            source_path: Some(std::path::Path::new("/work/source")),
            verified_cook_baseline: None,
            require_controller_git_bundle: true,
            reuse_compatible_snapshot: true,
            job_overrides: homeboy_core::lab_offload::LabJobOverrides {
                env,
                secret_env_names: vec!["AGENT_TOKEN".to_string()],
                workspace_root: Some("/runner/workspaces".to_string()),
            },
        }
    }

    fn submit_recipe_run(run_id: &str) {
        homeboy_agents::agent_task_lifecycle::submit_plan(
            &homeboy_agents::agent_task_scheduler::AgentTaskPlan::new("recipe-plan", Vec::new()),
            Some(run_id),
        )
        .expect("submit durable attempt");
    }

    fn envelope() -> LabStagingDispatchEnvelope {
        LabStagingDispatchEnvelope::new(
            "attempt-1",
            "lab-1",
            LabStagingInputRef::AgentTaskAttempt {
                run_id: "attempt-1".to_string(),
                recipe: recipe_ref(),
            },
        )
    }

    fn recipe_ref() -> LabStagingRecipeRef {
        LabStagingRecipeRef {
            attachment_digest: "sha256:attachment".to_string(),
            durable_plan_id: "recipe-plan".to_string(),
            canonical_plan_digest: "sha256:plan".to_string(),
        }
    }

    fn global_state_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn envelope_and_checkpoint_round_trip_without_secret_projection() {
        let envelope = envelope();
        let encoded = serde_json::to_value(&envelope).expect("serialize");
        assert_eq!(
            LabStagingDispatchDriver::envelope(&encoded).expect("parse"),
            envelope
        );
        let public = envelope.public_projection();
        assert!(public.get("input").is_none());
        assert_eq!(
            LabStagingCheckpoint::initial(&envelope).phase,
            LabStagingPhase::AcceptedMaterializeWorkspace
        );
    }

    #[test]
    fn registration_is_idempotent_for_cli_and_daemon_initialization() {
        register();
        register();
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        let mut request = Vec::new();
        let mut content_length = None;
        loop {
            let mut chunk = [0_u8; 4096];
            let count = stream.read(&mut chunk).expect("read request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
            if content_length.is_none() {
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    content_length = Some(
                        headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length: ")
                                    .or_else(|| line.strip_prefix("Content-Length: "))
                            })
                            .map(|value| value.parse::<usize>().expect("content length"))
                            .unwrap_or(0),
                    );
                }
            }
            if let (Some(header_end), Some(content_length)) = (
                request.windows(4).position(|part| part == b"\r\n\r\n"),
                content_length,
            ) {
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8(request).expect("UTF-8 request")
    }

    fn controller_job(
        id: uuid::Uuid,
        status: homeboy_core::api_jobs::JobStatus,
    ) -> homeboy_core::api_jobs::Job {
        homeboy_core::api_jobs::Job {
            id,
            operation: format!("controller.{LAB_STAGING_DISPATCH_JOB_TYPE}"),
            status,
            created_at_ms: 1,
            updated_at_ms: 1,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        }
    }

    #[test]
    fn controller_submission_recovers_create_and_start_after_accepted_response_loss() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let job_id = uuid::Uuid::new_v4();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..4 {
                let (mut stream, _) = listener.accept().expect("accept request");
                requests.push(read_http_request(&mut stream));
                if index == 0 || index == 2 {
                    continue;
                }
                let status = if index == 1 {
                    homeboy_core::api_jobs::JobStatus::Queued
                } else {
                    homeboy_core::api_jobs::JobStatus::Running
                };
                let body = json!({
                    "success": true,
                    "data": { "body": { "job": controller_job(job_id, status) } },
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
            requests
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");
        let create_body = json!({
            "type": LAB_STAGING_DISPATCH_JOB_TYPE,
            "version": LAB_STAGING_DISPATCH_VERSION,
            "request": envelope(),
            "idempotency_key": "attempt-1",
        });

        let created = post_controller_job_with_retries(
            &client,
            format!("{endpoint}/controller/jobs"),
            Some(create_body),
            "create test controller job",
        )
        .expect("recover create response");
        let started = post_controller_job_with_retries(
            &client,
            format!("{endpoint}/controller/jobs/{job_id}/start"),
            None,
            "start test controller job",
        )
        .expect("recover start response");
        let requests = server.join().expect("server");

        assert_eq!(created.id, job_id);
        assert_eq!(created.status, homeboy_core::api_jobs::JobStatus::Queued);
        assert_eq!(started.id, job_id);
        assert_eq!(started.status, homeboy_core::api_jobs::JobStatus::Running);
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(requests[2], requests[3]);
        assert!(requests[0].starts_with("POST /controller/jobs HTTP/1.1"));
        assert!(requests[2].starts_with(&format!("POST /controller/jobs/{job_id}/start HTTP/1.1")));
    }

    #[test]
    fn controller_submission_exhaustion_reports_uncertain_retryable_acceptance() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let server = std::thread::spawn(move || {
            for _ in 0..CONTROLLER_SUBMISSION_ATTEMPTS {
                let (mut stream, _) = listener.accept().expect("accept request");
                let _ = read_http_request(&mut stream);
            }
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");

        let error = post_controller_job_with_retries(
            &client,
            format!("{endpoint}/controller/jobs"),
            Some(json!({ "idempotency_key": "attempt-1" })),
            "create test controller job",
        )
        .expect_err("all accepted responses are lost");
        server.join().expect("server");

        assert_eq!(error.retryable, Some(true));
        assert!(error.message.contains("may have been accepted"));
        assert!(error.message.contains("same run identity"));
    }

    #[test]
    fn progress_and_result_projections_are_safe() {
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::Completed;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        checkpoint.hydration_id = Some("hydration-1".to_string());
        checkpoint.final_runner_job_id = Some("runner-job-1".to_string());
        let value = serde_json::to_value(checkpoint).expect("serialize");
        let public = LabStagingDispatchDriver
            .public_result(&value)
            .expect("project");
        assert_eq!(public["phase"], "completed");
        assert!(public.get("input").is_none());
    }

    #[test]
    fn completed_checkpoint_is_the_resume_recipe() {
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::MaterializeRuntime;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());

        let recovered = LabStagingDispatchDriver::checkpoint(
            serde_json::to_value(&checkpoint).expect("serialize checkpoint"),
        )
        .expect("recover checkpoint");

        assert_eq!(recovered, checkpoint);
        assert_eq!(recovered.phase, LabStagingPhase::MaterializeRuntime);
        assert_eq!(recovered.workspace_id.as_deref(), Some("workspace-1"));
    }

    #[derive(Default)]
    struct RecordedStageOperations {
        calls: Mutex<Vec<&'static str>>,
        reject_handoff_identity: Mutex<bool>,
        fail_hydration: Mutex<bool>,
        cancel_during_hydration: Mutex<bool>,
        recovered_runner_job_id: Mutex<Option<String>>,
        active_staging_jobs: AtomicUsize,
        fail_observation: Mutex<bool>,
    }

    impl RecordedStageOperations {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("calls lock").clone()
        }

        fn record(&self, name: &'static str) {
            self.calls.lock().expect("calls lock").push(name);
        }

        fn fail_hydration_once(&self) {
            *self.fail_hydration.lock().expect("failure lock") = true;
        }

        fn reject_handoff_identity_once(&self) {
            *self
                .reject_handoff_identity
                .lock()
                .expect("handoff identity lock") = true;
        }

        fn cancel_during_hydration_once(&self) {
            *self
                .cancel_during_hydration
                .lock()
                .expect("cancellation lock") = true;
        }

        fn recover_runner_job(&self, runner_job_id: &str) {
            *self
                .recovered_runner_job_id
                .lock()
                .expect("runner job lock") = Some(runner_job_id.to_string());
        }

        fn set_active_staging_jobs(&self, count: usize) {
            self.active_staging_jobs.store(count, Ordering::SeqCst);
        }

        fn fail_observation_once(&self) {
            *self.fail_observation.lock().expect("observation lock") = true;
        }
    }

    impl LabStagingStageOperations for RecordedStageOperations {
        fn validate_recorded_identities(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
        ) -> Result<()> {
            self.record("validate");
            Ok(())
        }

        fn recover_stage(
            &self,
            _request: &LabStagingExecutionRequest,
            checkpoint: &LabStagingCheckpoint,
            intent: &LabStagingStageIntent,
        ) -> Result<Option<LabStagingStageEffect>> {
            assert_eq!(intent.operation_id, checkpoint.operation_id_for_phase());
            let calls = self.calls();
            let recovered = match checkpoint.phase {
                LabStagingPhase::AcceptedMaterializeWorkspace if calls.contains(&"workspace") => {
                    Some(LabStagingStageEffect::Workspace(
                        "snapshot-1".to_string(),
                        "workspace-1".to_string(),
                    ))
                }
                LabStagingPhase::MaterializeRuntime if calls.contains(&"runtime") => {
                    Some(LabStagingStageEffect::Runtime("runtime-1".to_string()))
                }
                LabStagingPhase::HydrateDependencies if calls.contains(&"hydration") => {
                    Some(LabStagingStageEffect::Hydration("hydration-1".to_string()))
                }
                LabStagingPhase::DispatchRunner if calls.contains(&"dispatch") => {
                    Some(LabStagingStageEffect::Dispatch("runner-job-1".to_string()))
                }
                LabStagingPhase::ObserveRunner if calls.contains(&"observe") => {
                    Some(LabStagingStageEffect::Observe)
                }
                _ => None,
            };
            Ok(recovered)
        }

        fn validate_handoff_identity(
            &self,
            _request: &LabStagingExecutionRequest,
        ) -> Result<Option<LabHandoffHomeboyIdentity>> {
            self.record("handoff-identity");
            if std::mem::take(
                &mut *self
                    .reject_handoff_identity
                    .lock()
                    .expect("handoff identity lock"),
            ) {
                return Err(handoff_identity_error(
                    "lab-1",
                    "homeboy 1.2.3+required",
                    Some("homeboy 1.2.2+stale"),
                    Some("homeboy 1.2.2+stale"),
                ));
            }
            Ok(None)
        }

        fn materialize_workspace(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
            _handoff_identity: Option<LabHandoffHomeboyIdentity>,
        ) -> Result<(String, String)> {
            self.record("workspace");
            Ok(("snapshot-1".to_string(), "workspace-1".to_string()))
        }

        fn materialize_runtime(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
            _cancellation: &LabStagingCancellationToken,
        ) -> Result<String> {
            self.record("runtime");
            Ok("runtime-1".to_string())
        }

        fn hydrate_dependencies(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
            cancellation: &LabStagingCancellationToken,
        ) -> Result<String> {
            self.record("hydration");
            if std::mem::take(&mut *self.fail_hydration.lock().expect("failure lock")) {
                return Err(Error::internal_unexpected(
                    "simulated partial hydration failure",
                ));
            }
            if std::mem::take(
                &mut *self
                    .cancel_during_hydration
                    .lock()
                    .expect("cancellation lock"),
            ) {
                cancellation.cancel();
            }
            Ok("hydration-1".to_string())
        }

        fn dispatch_runner(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
            _cancellation: &LabStagingCancellationToken,
        ) -> Result<String> {
            self.record("dispatch");
            Ok("runner-job-1".to_string())
        }

        fn observe_runner(
            &self,
            _request: &LabStagingExecutionRequest,
            _checkpoint: &LabStagingCheckpoint,
            runner_job_id: &str,
            _cancellation: &LabStagingCancellationToken,
        ) -> Result<()> {
            assert_eq!(runner_job_id, "runner-job-1");
            self.record("observe");
            if std::mem::take(&mut *self.fail_observation.lock().expect("observation lock")) {
                let mut error = Error::internal_unexpected("simulated daemon observation outage");
                error.retryable = Some(true);
                return Err(error);
            }
            Ok(())
        }

        fn prove_no_runner_admission(&self, _request: &LabStagingExecutionRequest) -> Result<()> {
            self.record("prove-no-admission");
            Ok(())
        }

        fn recover_admitted_runner_job(
            &self,
            _request: &LabStagingExecutionRequest,
        ) -> Result<Option<String>> {
            self.record("recover-runner-job");
            Ok(self
                .recovered_runner_job_id
                .lock()
                .expect("runner job lock")
                .clone())
        }

        fn cancel_active_staging_jobs(
            &self,
            _request: &LabStagingExecutionRequest,
        ) -> Result<usize> {
            self.record("cancel-active-staging-jobs");
            Ok(self.active_staging_jobs.swap(0, Ordering::SeqCst))
        }

        fn cancel_and_reconcile_runner_job(
            &self,
            _request: &LabStagingExecutionRequest,
            runner_job_id: &str,
        ) -> Result<()> {
            assert_eq!(runner_job_id, "runner-job-1");
            self.record("cancel-runner-job");
            Ok(())
        }
    }

    fn execution_request() -> LabStagingExecutionRequest {
        let args = vec!["agent-task".to_string(), "run-plan".to_string()];
        LabStagingExecutionRequest {
            recipe: LabStagingRecipe::from_request(
                "attempt-1",
                "lab-1",
                &recipe_request(Some(recipe_command()), &args, HashMap::new()),
            )
            .expect("recipe"),
            durable_agent_task_plan: homeboy_agents::agent_task_scheduler::AgentTaskPlan::new(
                "recipe-plan",
                Vec::new(),
            ),
        }
    }

    #[test]
    fn new_handoff_recipe_pins_the_controller_homeboy_build_identity() {
        let args = vec!["agent-task".to_string(), "run-plan".to_string()];
        let recipe = LabStagingRecipe::from_request(
            "attempt-identity",
            "lab-identity",
            &recipe_request(Some(recipe_command()), &args, HashMap::new()),
        )
        .expect("recipe");

        assert_eq!(
            recipe.required_homeboy_build_identity.as_deref(),
            Some(homeboy_product_identity::build_identity().display.as_str())
        );
    }

    #[test]
    fn legacy_handoff_recipe_without_build_identity_remains_readable() {
        let mut recipe = serde_json::to_value(execution_request().recipe).expect("recipe JSON");
        recipe
            .as_object_mut()
            .expect("recipe object")
            .remove("required_homeboy_build_identity");

        let recipe: LabStagingRecipe = serde_json::from_value(recipe).expect("legacy recipe");
        assert_eq!(recipe.required_homeboy_build_identity, None);
    }

    #[test]
    fn handoff_identity_keeps_execution_identity_null_before_dispatch() {
        let identity = LabHandoffHomeboyIdentity {
            requested_build_identity: "homeboy 1.2.3+required".to_string(),
            configured_command_build_identity: "homeboy 1.2.3+required".to_string(),
            daemon_build_identity: "homeboy 1.2.3+required".to_string(),
            executed_command_build_identity: None,
        };

        assert_eq!(
            serde_json::to_value(identity).expect("serialize")["executed_command_build_identity"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn post_dispatch_execution_evidence_links_the_authoritative_record() {
        use homeboy_core::runner_execution_envelope::{
            BinaryProvenance, OrchestrationTargetProvenance, RunnerExecutionRecord,
        };

        let binary = |owner: &str| BinaryProvenance {
            owner: owner.to_string(),
            path: None,
            version: None,
            build_identity: Some("homeboy 1.2.3+required".to_string()),
        };
        let record = RunnerExecutionRecord::in_flight("execution-1", "lab-1", "daemon")
            .with_job_id("runner-job-1")
            .with_orchestration_provenance(Some(OrchestrationTargetProvenance::new(
                "lab-1",
                binary("controller"),
                binary("daemon"),
                binary("command"),
            )));
        let evidence = handoff_execution_evidence_from_record(&record, "runner-job-1")
            .expect("matching admitted execution record");

        let value = serde_json::to_value(evidence).expect("serialize");
        assert_eq!(value["execution_record_id"], "execution-1");
        assert_eq!(value["command_build_identity"], "homeboy 1.2.3+required");
    }

    #[test]
    fn automatic_convergence_requires_its_own_explicit_policy() {
        let mut runner: crate::Runner =
            serde_json::from_value(json!({ "kind": "local" })).expect("runner");
        runner.policy.allow_raw_exec = Some(true);
        assert!(!automatic_handoff_convergence_allowed(&runner));

        runner.policy.allow_homeboy_convergence = Some(true);
        assert!(automatic_handoff_convergence_allowed(&runner));
    }

    #[test]
    fn automatic_handoff_refresh_never_allows_a_downgrade() {
        let options = handoff_refresh_options(
            "lab-identity".to_string(),
            crate::HomeboyBinaryRefreshMode::Materialize,
            "homeboy 1.2.3+required",
        );

        assert!(!options.allow_downgrade);
        assert_eq!(options.git_ref.as_deref(), Some("required"));
    }

    #[test]
    fn stale_or_incompatible_runner_identity_fails_closed_with_one_immutable_recovery() {
        let error = handoff_identity_error(
            "lab-identity",
            "homeboy 1.2.3+required",
            Some("homeboy 1.2.2+stale"),
            Some("homeboy 1.2.2+stale"),
        );

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("before workspace materialization"));
        assert!(!error.message.contains("warning"));
        let recovery = error.details["tried"]
            .as_array()
            .expect("one recovery command");
        assert_eq!(recovery.len(), 1);
        assert_eq!(
            recovery[0],
            "homeboy runner refresh-homeboy lab-identity --ref required --reconnect"
        );
        assert_eq!(
            error.details["homeboy_handoff_identity"]["configured_command_build_identity"],
            "homeboy 1.2.2+stale"
        );
        assert_eq!(
            error.details["homeboy_handoff_identity"]["daemon_build_identity"],
            "homeboy 1.2.2+stale"
        );
        assert_eq!(
            error.details["homeboy_handoff_identity"]["executed_command_build_identity"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn incompatible_handoff_identity_is_rejected_before_workspace_materialization() {
        let operations = Arc::new(RecordedStageOperations::default());
        operations.reject_handoff_identity_once();
        let adapter = StageExecutionAdapter::new(operations.clone());
        let error = adapter
            .execute_with_checkpoint(
                &execution_request(),
                LabStagingCheckpoint::initial(&envelope()),
                &LabStagingCancellationToken::default(),
                |_| Ok(()),
            )
            .expect_err("stale runner identity must reject the staging operation");

        assert!(error.message.contains("before workspace materialization"));
        assert_eq!(operations.calls(), ["validate", "handoff-identity"]);
    }

    #[test]
    fn adapter_recovers_after_each_effect_before_completion_without_repeating_side_effects() {
        // Odd persistence calls store intent. Even calls store the completion
        // checkpoint, so crashing on each even call exercises every exact-once
        // window after an injected side effect.
        for crash_after in [2, 4, 6, 8, 10] {
            let operations = Arc::new(RecordedStageOperations::default());
            let adapter = StageExecutionAdapter::new(operations.clone());
            let request = execution_request();
            let token = LabStagingCancellationToken::default();
            let mut persisted = Vec::new();
            let interrupted = adapter.execute_with_checkpoint(
                &request,
                LabStagingCheckpoint::initial(&envelope()),
                &token,
                |checkpoint| {
                    if persisted.len() + 1 == crash_after {
                        Err(Error::internal_unexpected("simulated controller crash"))
                    } else {
                        persisted.push(checkpoint.clone());
                        Ok(())
                    }
                },
            );
            assert!(
                interrupted.is_err(),
                "crash point {crash_after} must interrupt"
            );
            let checkpoint = persisted.last().expect("checkpoint before crash").clone();
            let completed = adapter
                .execute_with_checkpoint(&request, checkpoint, &token, |checkpoint| {
                    persisted.push(checkpoint.clone());
                    Ok(())
                })
                .unwrap_or_else(|error| panic!("resume after crash {crash_after}: {error:?}"));

            assert_eq!(completed.phase, LabStagingPhase::Completed);
            assert_eq!(
                operations
                    .calls()
                    .into_iter()
                    .filter(|call| !matches!(*call, "validate" | "handoff-identity"))
                    .collect::<Vec<_>>(),
                ["workspace", "runtime", "hydration", "dispatch", "observe"]
            );
            assert_eq!(
                persisted
                    .iter()
                    .map(|checkpoint| checkpoint.phase.clone())
                    .collect::<Vec<_>>(),
                [
                    LabStagingPhase::AcceptedMaterializeWorkspace,
                    LabStagingPhase::MaterializeRuntime,
                    LabStagingPhase::MaterializeRuntime,
                    LabStagingPhase::HydrateDependencies,
                    LabStagingPhase::HydrateDependencies,
                    LabStagingPhase::DispatchRunner,
                    LabStagingPhase::DispatchRunner,
                    LabStagingPhase::ObserveRunner,
                    LabStagingPhase::ObserveRunner,
                    LabStagingPhase::Completed,
                ]
            );
        }
    }

    #[test]
    fn adapter_cancellation_proves_no_admission_before_dispatch_and_reconciles_after_acceptance() {
        let operations = Arc::new(RecordedStageOperations::default());
        let adapter = StageExecutionAdapter::new(operations.clone());
        let request = execution_request();
        let initial = LabStagingCheckpoint::initial(&envelope());
        adapter
            .cancel(&request, &initial)
            .expect("pre-dispatch cancellation");

        let mut accepted = initial;
        accepted.phase = LabStagingPhase::ObserveRunner;
        accepted.source_snapshot_id = Some("snapshot-1".to_string());
        accepted.workspace_id = Some("workspace-1".to_string());
        accepted.runtime_id = Some("runtime-1".to_string());
        accepted.hydration_id = Some("hydration-1".to_string());
        accepted.final_runner_job_id = Some("runner-job-1".to_string());
        adapter
            .cancel(&request, &accepted)
            .expect("post-acceptance cancellation");

        assert_eq!(
            operations.calls(),
            [
                "validate",
                "recover-runner-job",
                "cancel-active-staging-jobs",
                "prove-no-admission",
                "validate",
                "cancel-runner-job"
            ]
        );
    }

    #[test]
    fn adapter_cancellation_recovers_child_accepted_before_checkpoint_binding() {
        let operations = Arc::new(RecordedStageOperations::default());
        operations.recover_runner_job("runner-job-1");
        let adapter = StageExecutionAdapter::new(operations.clone());
        let request = execution_request();
        let mut dispatching = LabStagingCheckpoint::initial(&envelope());
        dispatching.phase = LabStagingPhase::DispatchRunner;
        dispatching.source_snapshot_id = Some("snapshot-1".to_string());
        dispatching.workspace_id = Some("workspace-1".to_string());
        dispatching.runtime_id = Some("runtime-1".to_string());
        dispatching.hydration_id = Some("hydration-1".to_string());

        adapter
            .cancel(&request, &dispatching)
            .expect("cancel accepted child before checkpoint binding");

        assert_eq!(
            operations.calls(),
            ["validate", "recover-runner-job", "cancel-runner-job"]
        );
    }

    #[test]
    fn adapter_cancellation_fails_uncertain_for_unreconciled_dispatch_intent() {
        let operations = Arc::new(RecordedStageOperations::default());
        let adapter = StageExecutionAdapter::new(operations.clone());
        let request = execution_request();
        let mut dispatching = LabStagingCheckpoint::initial(&envelope());
        dispatching.phase = LabStagingPhase::DispatchRunner;
        dispatching.source_snapshot_id = Some("snapshot-1".to_string());
        dispatching.workspace_id = Some("workspace-1".to_string());
        dispatching.runtime_id = Some("runtime-1".to_string());
        dispatching.hydration_id = Some("hydration-1".to_string());
        dispatching.stage_intent = Some(dispatching.intent_for_current_action());

        let error = adapter
            .cancel(&request, &dispatching)
            .expect_err("an in-flight dispatch without a child identity is uncertain");

        assert!(error.message.contains("cannot prove"));
        assert_eq!(
            operations.calls(),
            [
                "validate",
                "recover-runner-job",
                "cancel-active-staging-jobs"
            ]
        );
    }

    #[test]
    fn adapter_cancellation_stops_active_hydration_children() {
        let operations = Arc::new(RecordedStageOperations::default());
        operations.set_active_staging_jobs(1);
        let adapter = StageExecutionAdapter::new(operations.clone());

        adapter
            .cancel(
                &execution_request(),
                &LabStagingCheckpoint::initial(&envelope()),
            )
            .expect("cancel active hydration child");

        assert_eq!(
            operations.calls(),
            [
                "validate",
                "recover-runner-job",
                "cancel-active-staging-jobs"
            ]
        );
    }

    #[test]
    fn partial_or_cancelled_hydration_does_not_advance_the_checkpoint() {
        for cancelled in [false, true] {
            let operations = Arc::new(RecordedStageOperations::default());
            if cancelled {
                operations.cancel_during_hydration_once();
            } else {
                operations.fail_hydration_once();
            }
            let adapter = StageExecutionAdapter::new(operations.clone());
            let request = execution_request();
            let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
            checkpoint.phase = LabStagingPhase::HydrateDependencies;
            checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
            checkpoint.workspace_id = Some("workspace-1".to_string());
            checkpoint.runtime_id = Some("runtime-1".to_string());
            let mut persisted = Vec::new();
            assert!(adapter
                .execute_with_checkpoint(
                    &request,
                    checkpoint,
                    &LabStagingCancellationToken::default(),
                    |next| {
                        persisted.push(next.clone());
                        Ok(())
                    },
                )
                .is_err());
            assert_eq!(persisted.len(), 1);
            assert_eq!(persisted[0].phase, LabStagingPhase::HydrateDependencies);
            assert!(persisted[0].stage_intent.is_some());
            assert_eq!(operations.calls(), ["validate", "hydration"]);
        }
    }

    #[test]
    fn retryable_observation_outage_keeps_the_original_child_recoverable() {
        let operations = Arc::new(RecordedStageOperations::default());
        operations.fail_observation_once();
        let adapter = StageExecutionAdapter::new(operations.clone());
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::ObserveRunner;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        checkpoint.hydration_id = Some("hydration-1".to_string());
        checkpoint.final_runner_job_id = Some("runner-job-1".to_string());

        let completed = adapter
            .execute_with_checkpoint(
                &execution_request(),
                checkpoint,
                &LabStagingCancellationToken::default(),
                |_| Ok(()),
            )
            .expect("retry observation");

        assert_eq!(completed.phase, LabStagingPhase::Completed);
        assert_eq!(operations.calls(), ["validate", "observe", "observe"]);
    }

    #[test]
    fn production_recovery_fails_closed_for_an_intent_without_a_receipt() {
        let request = execution_request();
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        let intent = checkpoint.intent_for_current_action();
        checkpoint.stage_intent = Some(intent.clone());
        let error = ProductionLabStagingOperations
            .recover_stage(&request, &checkpoint, &intent)
            .expect_err("an intent with no workspace receipt is ambiguous");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
    }

    struct CancellationAdapter(Arc<AtomicUsize>);

    impl LabStagingExecutionAdapter for CancellationAdapter {
        fn execute(
            &self,
            _request: &LabStagingExecutionRequest,
            checkpoint: LabStagingCheckpoint,
            _cancellation: LabStagingCancellationToken,
            _job: ControllerJobHandle,
        ) -> Result<LabStagingCheckpoint> {
            Ok(checkpoint)
        }

        fn cancel(
            &self,
            _request: &LabStagingExecutionRequest,
            checkpoint: &LabStagingCheckpoint,
        ) -> Result<()> {
            assert_eq!(
                checkpoint.final_runner_job_id.as_deref(),
                Some("runner-job-1")
            );
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn cancellation_signals_the_owned_token_before_adapter_confirmation() {
        let _serial = global_state_lock().lock().expect("lock");
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "cancel-authority";
            submit_recipe_run(run_id);
            let args = vec!["agent-task".to_string(), "run-plan".to_string()];
            persist_lab_staging_recipe(
                run_id,
                "lab-1",
                &recipe_request(Some(recipe_command()), &args, HashMap::new()),
            )
            .expect("persist");
            let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                LabStagingRecipe,
            >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)
            .expect("attachment");
            let plan =
                homeboy_agents::agent_task_lifecycle::load_controller_plan(run_id).expect("plan");
            let envelope = LabStagingDispatchEnvelope::new(
                run_id,
                "lab-1",
                LabStagingInputRef::AgentTaskAttempt {
                    run_id: run_id.to_string(),
                    recipe: LabStagingRecipeRef::from_authority(attachment.payload_digest, &plan)
                        .expect("ref"),
                },
            );
            let mut checkpoint = LabStagingCheckpoint::initial(&envelope);
            checkpoint.phase = LabStagingPhase::Completed;
            checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
            checkpoint.workspace_id = Some("workspace-1".to_string());
            checkpoint.runtime_id = Some("runtime-1".to_string());
            checkpoint.hydration_id = Some("hydration-1".to_string());
            checkpoint.final_runner_job_id = Some("runner-job-1".to_string());
            cancellations()
                .lock()
                .expect("lock")
                .remove(&checkpoint.run_id);
            let cancelled = Arc::new(AtomicUsize::new(0));
            install_production_execution_adapter(Arc::new(CancellationAdapter(Arc::clone(
                &cancelled,
            ))));

            LabStagingDispatchDriver
                .cancel(&serde_json::to_value(&checkpoint).expect("serialize"))
                .expect("cancel");

            assert!(cancellations()
                .lock()
                .expect("lock")
                .get(&checkpoint.run_id)
                .expect("recreated cancellation token")
                .is_cancelled());
            assert_eq!(cancelled.load(Ordering::SeqCst), 1);
            *adapter_slot().lock().expect("adapter lock") = None;
            cancellations()
                .lock()
                .expect("lock")
                .remove(&checkpoint.run_id);
        });
    }

    #[test]
    fn unsupported_raw_input_reference_is_rejected() {
        let value = json!({
            "schema": LAB_STAGING_DISPATCH_SCHEMA,
            "run_id": "attempt-1",
            "runner_id": "lab-1",
            "input": { "type": "uri", "locator": "file:///private/input.json" },
        });
        assert!(LabStagingDispatchDriver::envelope(&value).is_err());
    }

    #[test]
    fn malformed_or_inconsistent_checkpoint_is_rejected_before_projection() {
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::Completed;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        // A dispatching phase cannot omit completed hydration/final handoff.
        assert!(LabStagingDispatchDriver::checkpoint(
            serde_json::to_value(&checkpoint).expect("serialize"),
        )
        .is_err());

        checkpoint.phase = LabStagingPhase::AcceptedMaterializeWorkspace;
        checkpoint.workspace_id = Some("workspace-1".to_string());
        assert!(LabStagingDispatchDriver::checkpoint(
            serde_json::to_value(&checkpoint).expect("serialize"),
        )
        .is_err());

        let mut inconsistent = LabStagingCheckpoint::initial(&envelope());
        inconsistent.input = LabStagingInputRef::AgentTaskAttempt {
            run_id: "another-attempt".to_string(),
            recipe: recipe_ref(),
        };
        assert!(LabStagingDispatchDriver::checkpoint(
            serde_json::to_value(&inconsistent).expect("serialize"),
        )
        .is_err());
    }

    #[test]
    fn production_adapter_readiness_fails_closed_when_uninstalled() {
        let _serial = global_state_lock().lock().expect("lock");
        *adapter_slot().lock().expect("lock") = None;
        assert!(require_production_adapter().is_err());
    }

    fn durable_workspace_state(request: &LabStagingExecutionRequest) -> DurableLabWorkspaceStage {
        DurableLabWorkspaceStage {
            schema: "homeboy/durable-lab-workspace-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe).expect("recipe digest"),
            plan_digest: canonical_digest(&request.durable_agent_task_plan).expect("plan digest"),
            source_snapshot_id: "snapshot-1".to_string(),
            workspace_id: "/runner/workspaces/attempt-1".to_string(),
            remote_cwd: "/runner/workspaces/attempt-1".to_string(),
            homeboy_handoff_identity: None,
            stage: json!({
                "remote_cwd": "/runner/workspaces/attempt-1",
                "source_snapshot": {
                    "workspace_snapshot_identity": "snapshot-1",
                    "remote_path": "/runner/workspaces/attempt-1",
                },
                "command": ["homeboy", "agent-task", "run-plan"],
                "remapped_args": ["agent-task", "run-plan"],
            }),
        }
    }

    #[test]
    fn durable_workspace_state_reuses_exact_recording_and_fails_closed_on_tamper_or_recipe_change()
    {
        let request = execution_request();
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::MaterializeRuntime;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("/runner/workspaces/attempt-1".to_string());
        let durable = durable_workspace_state(&request);

        durable
            .validate(&request, &checkpoint)
            .expect("valid resume");
        assert!(!serde_json::to_string(&durable)
            .expect("serialize")
            .contains("private-marker"));

        let mut tampered = durable.clone();
        tampered.stage["source_snapshot"]["remote_path"] = json!("/other");
        assert!(tampered.validate(&request, &checkpoint).is_err());

        let mut changed = request.clone();
        changed.recipe.normalized_args.push("--changed".to_string());
        assert!(durable.validate(&changed, &checkpoint).is_err());
    }

    #[test]
    fn durable_hydration_receipt_binds_results_and_dispatch_inputs_without_secrets() {
        let request = execution_request();
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::DispatchRunner;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("/runner/workspaces/attempt-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        let workspace = durable_workspace_state(&request);
        let plan = serde_json::to_value(homeboy_core::plan::HomeboyPlan::for_description(
            homeboy_core::plan::PlanKind::LabOffload,
            "hydration",
        ))
        .expect("plan");
        let runtime = DurableLabRuntimeStage {
            schema: "homeboy/durable-lab-runtime-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe).expect("recipe digest"),
            plan_digest: canonical_digest(&request.durable_agent_task_plan).expect("plan digest"),
            workspace_id: workspace.workspace_id.clone(),
            runtime_id: "runtime-1".to_string(),
            output: json!({ "remote_cwd": workspace.workspace_id, "plan": plan }),
        };
        let hydration_record = json!({ "Applied": {
            "schema": crate::lab::offload::HYDRATION_SCHEMA,
            "status": "hydrated",
            "workspace": workspace.workspace_id,
            "steps": [{ "job_id": "hydration-job-1" }]
        }});
        let hydration_id = canonical_digest(&hydration_record).expect("hydration digest");
        checkpoint.hydration_id = Some(hydration_id.clone());
        let receipt = DurableLabHydrationStage {
            schema: "homeboy/durable-lab-hydration-stage/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe).expect("recipe digest"),
            plan_digest: canonical_digest(&request.durable_agent_task_plan).expect("plan digest"),
            workspace_id: workspace.workspace_id.clone(),
            runtime_id: runtime.runtime_id.clone(),
            hydration_id,
            plan: runtime.output["plan"].clone(),
            hydration_record: hydration_record.clone(),
            hydration_metadata: hydration_metadata_from_record(&hydration_record),
            execution_ids: vec!["hydration-job-1".to_string()],
            dispatch_inputs: json!({
                "workspace_stage": workspace.stage,
                "runtime_output": runtime.output,
                "plan": runtime.output["plan"],
            }),
        };
        receipt
            .validate(&request, &checkpoint, &workspace, &runtime)
            .expect("bound receipt");
        assert!(!serde_json::to_string(&receipt)
            .expect("serialize")
            .contains("private-marker"));

        let mut tampered = receipt.clone();
        tampered.execution_ids.clear();
        assert!(tampered
            .validate(&request, &checkpoint, &workspace, &runtime)
            .is_err());
        let mut different_runner = request.clone();
        different_runner.recipe.runner_id = "other-runner".to_string();
        assert!(receipt
            .validate(&different_runner, &checkpoint, &workspace, &runtime)
            .is_err());
    }

    #[test]
    fn dispatch_receipt_fails_closed_on_tamper_and_never_serializes_secret_values() {
        let request = execution_request();
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::ObserveRunner;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        checkpoint.hydration_id = Some("hydration-1".to_string());
        checkpoint.final_runner_job_id = Some("runner-job-1".to_string());
        let receipt = DurableLabDispatchReceipt {
            schema: "homeboy/durable-lab-dispatch-receipt/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe).expect("digest"),
            workspace_attachment_digest: "sha256:workspace".to_string(),
            runtime_attachment_digest: "sha256:runtime".to_string(),
            hydration_attachment_digest: "sha256:hydration".to_string(),
            daemon_lease_id: Some("lease-1".to_string()),
            reservation_job_id: Some("reservation-1".to_string()),
            reverse_submission_key: None,
            runner_job_id: "runner-job-1".to_string(),
            remote_workspace: "/runner/workspace".to_string(),
            remote_command: vec!["homeboy".to_string(), "agent-task".to_string()],
            workload_identity: "sha256:workload".to_string(),
            homeboy_handoff_execution_evidence: None,
        };
        receipt.validate(&request, &checkpoint).expect("receipt");
        assert!(!serde_json::to_string(&receipt)
            .expect("serialize")
            .contains("private-marker"));
        let mut tampered = receipt;
        tampered.runner_job_id = "other-job".to_string();
        assert!(tampered.validate(&request, &checkpoint).is_err());
    }

    #[test]
    fn production_dispatch_recovery_rebuilds_receipt_after_accepted_bind_before_receipt() {
        let _serial = global_state_lock().lock().expect("lock");
        homeboy_core::test_support::with_isolated_home(|_| {
            let request = execution_request();
            submit_recipe_run(&request.recipe.run_id);
            homeboy_agents::agent_task_lifecycle::record_lab_offload_phase(
                &request.recipe.run_id,
                &request.recipe.runner_id,
                "dispatching",
                Some("/runner/workspaces/attempt-1"),
                None,
                None,
                Some(&request.durable_agent_task_plan),
            )
            .expect("record staging handoff");

            let workspace = durable_workspace_state(&request);
            let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
            checkpoint.phase = LabStagingPhase::DispatchRunner;
            checkpoint.source_snapshot_id = Some(workspace.source_snapshot_id.clone());
            checkpoint.workspace_id = Some(workspace.workspace_id.clone());
            checkpoint.runtime_id = Some("runtime-1".to_string());

            let runtime = DurableLabRuntimeStage {
                schema: "homeboy/durable-lab-runtime-stage/v1".to_string(),
                run_id: request.recipe.run_id.clone(),
                runner_id: request.recipe.runner_id.clone(),
                recipe_digest: canonical_digest(&request.recipe).expect("recipe digest"),
                plan_digest: canonical_digest(&request.durable_agent_task_plan)
                    .expect("plan digest"),
                workspace_id: workspace.workspace_id.clone(),
                runtime_id: "runtime-1".to_string(),
                output: json!({
                    "remote_cwd": workspace.workspace_id,
                    "command": ["homeboy", "agent-task", "run-plan"],
                }),
            };
            let hydration_record = json!({ "NotApplied": { "reason": "fixture" } });
            let hydration = DurableLabHydrationStage {
                schema: "homeboy/durable-lab-hydration-stage/v1".to_string(),
                run_id: request.recipe.run_id.clone(),
                runner_id: request.recipe.runner_id.clone(),
                recipe_digest: canonical_digest(&request.recipe).expect("recipe digest"),
                plan_digest: canonical_digest(&request.durable_agent_task_plan)
                    .expect("plan digest"),
                workspace_id: workspace.workspace_id.clone(),
                runtime_id: runtime.runtime_id.clone(),
                hydration_id: canonical_digest(&hydration_record).expect("hydration digest"),
                plan: Value::Null,
                hydration_record: hydration_record.clone(),
                hydration_metadata: hydration_metadata_from_record(&hydration_record),
                execution_ids: Vec::new(),
                dispatch_inputs: json!({
                    "workspace_stage": workspace.stage,
                    "runtime_output": runtime.output,
                    "plan": null,
                }),
            };
            checkpoint.hydration_id = Some(hydration.hydration_id.clone());
            homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
                &request.recipe.run_id,
                DURABLE_LAB_WORKSPACE_STAGE_ATTACHMENT_KIND,
                &workspace,
            )
            .expect("persist workspace receipt");
            homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
                &request.recipe.run_id,
                DURABLE_LAB_RUNTIME_STAGE_ATTACHMENT_KIND,
                &runtime,
            )
            .expect("persist runtime receipt");
            homeboy_agents::agent_task_lifecycle::persist_private_run_attachment(
                &request.recipe.run_id,
                DURABLE_LAB_HYDRATION_STAGE_ATTACHMENT_KIND,
                &hydration,
            )
            .expect("persist hydration receipt");

            homeboy_agents::agent_task_lifecycle::bind_accepted_lab_runner_job(
                &homeboy_core::lab_contract::RunnerJobIdentity::new(
                    &request.recipe.run_id,
                    &request.recipe.runner_id,
                    "runner-job-accepted",
                ),
                "/runner/workspaces/attempt-1",
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "run-plan".to_string(),
                ],
            )
            .expect("bind accepted job before receipt");

            let intent = checkpoint.intent_for_current_action();
            let recovered = ProductionLabStagingOperations
                .recover_stage(&request, &checkpoint, &intent)
                .expect("recover accepted dispatch")
                .expect("recovered dispatch effect");
            assert!(matches!(
                recovered,
                LabStagingStageEffect::Dispatch(ref job_id) if job_id == "runner-job-accepted"
            ));
            let receipt = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                DurableLabDispatchReceipt,
            >(
                &request.recipe.run_id,
                DURABLE_LAB_DISPATCH_RECEIPT_ATTACHMENT_KIND,
            )
            .expect("reconstructed dispatch receipt");
            assert_eq!(receipt.payload.runner_job_id, "runner-job-accepted");
            assert!(receipt.payload.daemon_lease_id.is_none());
            assert!(receipt.payload.reservation_job_id.is_none());
            assert!(receipt.payload.reverse_submission_key.is_none());
        });
    }

    #[test]
    fn reverse_dispatch_receipt_requires_the_bound_broker_submission_key() {
        let mut request = execution_request();
        request.recipe.tunnel_mode = crate::RunnerTunnelMode::Reverse;
        let mut checkpoint = LabStagingCheckpoint::initial(&envelope());
        checkpoint.phase = LabStagingPhase::ObserveRunner;
        checkpoint.source_snapshot_id = Some("snapshot-1".to_string());
        checkpoint.workspace_id = Some("workspace-1".to_string());
        checkpoint.runtime_id = Some("runtime-1".to_string());
        checkpoint.hydration_id = Some("hydration-1".to_string());
        checkpoint.final_runner_job_id = Some("runner-job-1".to_string());
        let mut receipt = DurableLabDispatchReceipt {
            schema: "homeboy/durable-lab-dispatch-receipt/v1".to_string(),
            run_id: request.recipe.run_id.clone(),
            runner_id: request.recipe.runner_id.clone(),
            recipe_digest: canonical_digest(&request.recipe).expect("digest"),
            workspace_attachment_digest: "sha256:workspace".to_string(),
            runtime_attachment_digest: "sha256:runtime".to_string(),
            hydration_attachment_digest: "sha256:hydration".to_string(),
            daemon_lease_id: None,
            reservation_job_id: None,
            reverse_submission_key: Some(crate::execution::reverse_broker_submission_key(
                &request.recipe.runner_id,
                &request.recipe.run_id,
            )),
            runner_job_id: "runner-job-1".to_string(),
            remote_workspace: "/runner/workspace".to_string(),
            remote_command: vec!["homeboy".to_string(), "agent-task".to_string()],
            workload_identity: "sha256:workload".to_string(),
            homeboy_handoff_execution_evidence: None,
        };

        receipt
            .validate(&request, &checkpoint)
            .expect("reverse receipt");
        receipt.reverse_submission_key = Some("wrong-key".to_string());
        assert!(receipt.validate(&request, &checkpoint).is_err());
    }

    #[test]
    fn registration_keeps_lab_routing_disabled_without_submission_hook() {
        let _serial = global_state_lock().lock().expect("lock");
        *adapter_slot().lock().expect("adapter lock") = None;
        register();
        assert!(!routing_ready());
    }

    #[test]
    fn production_enablement_installs_the_staging_adapter() {
        let _serial = global_state_lock().lock().expect("lock");
        *adapter_slot().lock().expect("adapter lock") = None;
        register();
        enable_production_routing();
        assert!(routing_ready());
        *adapter_slot().lock().expect("adapter lock") = None;
    }

    #[test]
    fn cancellation_guard_removes_token_after_adapter_error() {
        let _serial = global_state_lock().lock().expect("lock");
        let run_id = "adapter-error".to_string();
        let token = LabStagingCancellationToken::default();
        cancellations()
            .lock()
            .expect("lock")
            .insert(run_id.clone(), token);
        let result: Result<()> = {
            let _cleanup = CancellationGuard(run_id.clone());
            Err(Error::internal_unexpected("adapter failed"))
        };
        assert!(result.is_err());
        assert!(!cancellations().lock().expect("lock").contains_key(&run_id));
    }

    #[test]
    fn recipe_round_trips_without_secret_values_and_reconstructs_borrowed_request() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "recipe-round-trip";
            submit_recipe_run(run_id);
            let args = vec!["agent-task".to_string(), "run-plan".to_string()];
            let request = recipe_request(
                Some(recipe_command()),
                &args,
                HashMap::from([("PUBLIC_FLAG".to_string(), "enabled".to_string())]),
            );
            let recipe = persist_lab_staging_recipe(run_id, "lab-1", &request).expect("persist");
            let loaded = load_lab_staging_recipe(run_id).expect("load");
            assert_eq!(loaded.recipe, recipe);
            let execution: LabStagingExecutionRequest = loaded.into();
            assert_eq!(execution.recipe.normalized_args, args);
            assert_eq!(execution.recipe.runner_id, "lab-1");
            assert_eq!(
                execution.recipe.source_path,
                Some(PathBuf::from("/work/source"))
            );
            assert_eq!(execution.recipe.job_override_env["PUBLIC_FLAG"], "enabled");
            assert!(!execution
                .recipe
                .job_override_env
                .contains_key("AGENT_TOKEN"));
            assert_eq!(execution.recipe.secret_env_names, vec!["AGENT_TOKEN"]);
            let bytes = std::fs::read(
                homeboy_core::paths::homeboy_data()
                    .expect("data root")
                    .join("agent-task-runs")
                    .join(run_id)
                    .join("private/lab-staging-recipe.json"),
            )
            .expect("attachment bytes");
            assert!(!String::from_utf8_lossy(&bytes).contains("private-marker"));
            let public = LabStagingDispatchEnvelope::new(
                run_id,
                "lab-1",
                LabStagingInputRef::AgentTaskAttempt {
                    run_id: run_id.to_string(),
                    recipe: recipe_ref(),
                },
            )
            .public_projection();
            assert!(!public.to_string().contains("private-marker"));
        });
    }

    #[test]
    fn recipe_rejects_inline_secrets_and_non_agent_task_or_mismatched_identity() {
        let args = vec!["agent-task".to_string(), "run-plan".to_string()];
        let inline = recipe_request(
            Some(recipe_command()),
            &args,
            HashMap::from([("AGENT_TOKEN".to_string(), "private-marker".to_string())]),
        );
        let error =
            LabStagingRecipe::from_request("run", "lab", &inline).expect_err("secret rejected");
        assert!(!error.message.contains("private-marker"));

        let wrong_args = vec!["review".to_string()];
        let wrong_shape = recipe_request(Some(recipe_command()), &wrong_args, HashMap::new());
        assert!(LabStagingRecipe::from_request("run", "lab", &wrong_shape).is_err());

        let mut recipe = LabStagingRecipe::from_request(
            "run",
            "lab",
            &recipe_request(Some(recipe_command()), &args, HashMap::new()),
        )
        .expect("recipe");
        recipe.schema = "unsupported".to_string();
        assert!(recipe.validate().is_err());
        recipe.schema = LAB_STAGING_RECIPE_SCHEMA.to_string();
        recipe.secret_env_names = vec!["AGENT_TOKEN".to_string(), "AGENT_TOKEN".to_string()];
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn prepare_recipe_lookup_failure_terminalizes_the_parent_with_retry_metadata() {
        let _serial = global_state_lock().lock().expect("lock");
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "prepare-plan-lookup-failure";
            submit_recipe_run(run_id);
            let args = vec!["agent-task".to_string(), "run-plan".to_string()];
            persist_lab_staging_recipe(
                run_id,
                "lab-1",
                &recipe_request(Some(recipe_command()), &args, HashMap::new()),
            )
            .expect("persist recipe");
            let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                LabStagingRecipe,
            >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)
            .expect("recipe attachment");
            let plan = homeboy_agents::agent_task_lifecycle::load_controller_plan(run_id)
                .expect("durable plan");
            std::fs::remove_file(
                homeboy_core::paths::homeboy_data()
                    .expect("data root")
                    .join("agent-task-runs")
                    .join(run_id)
                    .join("private/lab-staging-recipe.json"),
            )
            .expect("remove recipe to force real authoritative lookup failure");
            let envelope = LabStagingDispatchEnvelope::new(
                run_id,
                "lab-1",
                LabStagingInputRef::AgentTaskAttempt {
                    run_id: run_id.to_string(),
                    recipe: LabStagingRecipeRef::from_authority(attachment.payload_digest, &plan)
                        .expect("recipe reference"),
                },
            );

            LabStagingDispatchDriver
                .prepare(serde_json::to_value(envelope).expect("serialize envelope"))
                .expect_err("missing recipe must fail before staging");

            let record = homeboy_agents::agent_task_lifecycle::status(run_id).expect("parent");
            assert!(matches!(
                record.state,
                homeboy_agents::agent_task_lifecycle::AgentTaskRunState::Failed
            ));
            assert_eq!(
                record.metadata["pre_execution_failure"]["phase"],
                "lab_staging_prepare"
            );
            assert_eq!(
                record.metadata["pre_execution_failure"]["provider_executions_consumed"],
                0
            );
            assert!(!routing_ready());
        });
    }

    #[test]
    fn secret_bearing_recipe_never_reaches_public_or_durable_projection_surfaces() {
        let _serial = global_state_lock().lock().expect("lock");
        homeboy_core::test_support::with_isolated_home(|_| {
            const MARKER: &str = "staging-secret-marker";
            let run_id = "secret-projection";
            submit_recipe_run(run_id);
            let args = vec!["agent-task".to_string(), "run-plan".to_string()];
            let request = recipe_request(
                Some(recipe_command()),
                &args,
                HashMap::from([("PUBLIC_FLAG".to_string(), "enabled".to_string())]),
            );
            let recipe = persist_lab_staging_recipe(run_id, "lab-1", &request).expect("persist");
            let plan = homeboy_agents::agent_task_lifecycle::load_controller_plan(run_id)
                .expect("durable plan");
            let attachment = homeboy_agents::agent_task_lifecycle::load_private_run_attachment::<
                LabStagingRecipe,
            >(run_id, LAB_STAGING_RECIPE_ATTACHMENT_KIND)
            .expect("recipe attachment");
            let envelope = LabStagingDispatchEnvelope::new(
                run_id,
                "lab-1",
                LabStagingInputRef::AgentTaskAttempt {
                    run_id: run_id.to_string(),
                    recipe: LabStagingRecipeRef::from_authority(attachment.payload_digest, &plan)
                        .expect("recipe reference"),
                },
            );
            let checkpoint = LabStagingCheckpoint::initial(&envelope);
            let public_request = LabStagingDispatchDriver
                .public_request(&serde_json::to_value(&envelope).expect("request"))
                .expect("public request");
            let public_checkpoint = LabStagingDispatchDriver
                .public_progress(&serde_json::to_value(&checkpoint).expect("checkpoint"))
                .expect("public checkpoint");
            let record = homeboy_agents::agent_task_lifecycle::status(run_id).expect("record");
            let recipe_bytes = std::fs::read(
                homeboy_core::paths::homeboy_data()
                    .expect("data root")
                    .join("agent-task-runs")
                    .join(run_id)
                    .join("private/lab-staging-recipe.json"),
            )
            .expect("recipe bytes");

            for surface in [
                serde_json::to_string(&recipe).expect("recipe"),
                public_request.to_string(),
                public_checkpoint.to_string(),
                serde_json::to_string(&record).expect("record"),
                String::from_utf8_lossy(&recipe_bytes).into_owned(),
                LabStagingDispatchDriver
                    .public_error(&Error::internal_unexpected("staging failed"))
                    .data
                    .to_string(),
            ] {
                assert!(!surface.contains(MARKER), "secret leaked to {surface}");
            }
            assert_eq!(recipe.secret_env_names, ["AGENT_TOKEN"]);
            assert!(!routing_ready());
        });
    }

    #[test]
    fn recipe_load_requires_existing_attempt_plan_and_immutable_attachment() {
        homeboy_core::test_support::with_isolated_home(|_| {
            assert!(load_lab_staging_recipe("missing-attempt").is_err());
            let run_id = "recipe-conflict";
            submit_recipe_run(run_id);
            let args = vec!["agent-task".to_string(), "run-plan".to_string()];
            let request = recipe_request(Some(recipe_command()), &args, HashMap::new());
            persist_lab_staging_recipe(run_id, "lab-1", &request).expect("persist");
            assert!(persist_lab_staging_recipe(run_id, "lab-2", &request).is_err());
            let record = homeboy_agents::agent_task_lifecycle::status(run_id).expect("record");
            std::fs::remove_file(record.plan_path).expect("remove durable plan");
            assert!(load_lab_staging_recipe(run_id).is_err());
        });
    }
}
