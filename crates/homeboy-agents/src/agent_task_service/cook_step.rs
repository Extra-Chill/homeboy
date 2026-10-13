//! Detached Cook execution as a daemon `step`.
//!
//! A detached Cook is one `step` job: the daemon spawns `homeboy agent-task
//! cook --wait --cook-id <id> ...` with `env_clear()` plus the environment the
//! launcher admitted here, and supervises that child by PID and start
//! identity. The launcher's environment is captured explicitly, so the
//! daemon's own environment never decides which credentials or toolchain a
//! Cook sees.
//!
//! The admitted environment is the declared process mechanics every provider
//! launch already inherits, the variables each provider declares it inherits,
//! Homeboy's own `HOMEBOY_*` configuration, and the caller's explicit
//! overrides. Provider secrets are resolved by the Cook child from configured
//! agent-task sources exactly as in the foreground. A provider secret that
//! exists only in the launcher's shell environment cannot be reproduced and is
//! refused at admission with the names to declare.
//!
//! When the child exits, the observer registered here reconciles the linked
//! Cook record the way the child would have had it survived: a handoff that
//! never materialized fails, an interrupted attempt is recorded as such, and
//! the terminal notification is delivered once.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use homeboy_core::daemon::step_job::{self, StepExit, StepJobRequest};
use homeboy_core::daemon::workflow_job::WorkflowRunOutcome;
use homeboy_core::env_materialization_plan::{EnvMaterializationPlan, EnvSecretRef};
use homeboy_core::{Error, ErrorCode, Result};
use serde_json::{json, Value};

use crate::agent_task_lifecycle;

/// Idempotency-key prefix of the step that runs an initial detached Cook.
pub const COOK_LAUNCH_STEP_KEY_PREFIX: &str = "agent-task-cook-launch:";
/// Idempotency-key prefix of the step that runs one local Cook retry.
pub const COOK_RETRY_STEP_KEY_PREFIX: &str = "agent-task-cook-retry:";

/// Process mechanics a Homeboy step needs beyond the provider base list: Git
/// and SSH transport to the operator's agent, and locale/timezone for tools it
/// runs.
const STEP_PROCESS_ENV: &[&str] = &[
    "SSH_AUTH_SOCK",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "__CF_USER_TEXT_ENCODING",
];
const HOMEBOY_ENV_PREFIX: &str = "HOMEBOY_";
/// `HOMEBOY_*` variables that describe the admitting process's own execution
/// (its run, its runner job and placement, its daemon frame, its step) rather
/// than Homeboy configuration. A step is a new execution: inheriting these
/// would make a Cook launched from inside a runner job believe it is that
/// runner job's hosted attempt.
const INVOCATION_CONTEXT_ENV: &[&str] = &[
    "HOMEBOY_RUN_ID",
    "HOMEBOY_ACTIVE_RUN_ID",
    "HOMEBOY_BENCH_RUN_ID",
    "HOMEBOY_COMMAND",
    "HOMEBOY_DAEMON_STATE_DIR",
    "HOMEBOY_SETTINGS_JSON",
    "HOMEBOY_SOURCE_SNAPSHOT_JSON",
    "HOMEBOY_STEP_OUTPUT",
];
const INVOCATION_CONTEXT_ENV_PREFIXES: &[&str] = &[
    "HOMEBOY_RUNNER_",
    "HOMEBOY_LAB_EXECUTION_",
    "HOMEBOY_DAEMON_CHILD_",
];

/// Whether this process runs inside a runner job. The CLI clears the
/// placement markers once routing has consumed them; the job's own identity
/// and Lab execution provenance remain for its lifetime.
fn admitted_inside_runner_job() -> bool {
    homeboy_core::resource_policy_context::is_runner_hosted_exec()
        || homeboy_core::resource_policy_context::has_lab_execution_provenance()
        || std::env::var_os("HOMEBOY_RUNNER_JOB_ID").is_some()
}

fn is_invocation_context_env(name: &str) -> bool {
    name == step_job::STEP_JOB_ID_ENV
        || INVOCATION_CONTEXT_ENV.contains(&name)
        || INVOCATION_CONTEXT_ENV_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}
const CHILD_RESULT_TAIL_BYTES: u64 = 64 * 1024;

/// Capture the environment a detached Cook step runs with, from this
/// launcher's process environment.
///
/// `overrides` carries values the launcher sets explicitly for the child
/// (launcher identity, notification route); they replace captured values.
pub fn admitted_cook_environment(
    overrides: impl IntoIterator<Item = (String, String)>,
) -> Result<EnvMaterializationPlan> {
    let catalog = crate::agent_task_provider::AgentTaskProviderCatalog::discover();
    let mut names = Vec::new();
    let mut ambient_only_secrets = Vec::new();
    for provider in &catalog.providers {
        let secret_names = crate::agent_task_provider::provider_secret_env_plan(
            provider,
            &serde_json::from_value(json!({
                "task_id": "step-environment-admission",
                "executor": { "backend": provider.backend },
                "instructions": "",
            }))
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        )
        .secret_env_names();
        for env in &provider.invocation.env {
            if env.value.is_none()
                && !env.redacted.unwrap_or(false)
                && !secret_names.contains(&env.name)
                && matches!(env.source.as_deref(), Some("env" | "inherit"))
            {
                names.push(env.name.clone());
            }
        }
        ambient_only_secrets
            .extend(crate::agent_task_provider::ambient_only_provider_secret_env(provider));
    }
    ambient_only_secrets.sort();
    ambient_only_secrets.dedup();
    if !ambient_only_secrets.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "environment",
            format!(
                "provider secret(s) exist only in this shell's environment and cannot reach a daemon-supervised Cook: {}",
                ambient_only_secrets.join(", ")
            ),
            None,
            Some(vec![
                "Declare each one as an agent-task secret (`homeboy agent-task secrets --help`) so the Cook resolves it without inheriting this shell.".to_string(),
            ]),
        );
        error.details["ambient_only_secret_env"] = json!(ambient_only_secrets);
        return Err(error);
    }
    Ok(admitted_homeboy_step_environment(names, overrides))
}

/// Capture the environment a daemon step running this Homeboy binary needs,
/// from the caller's process environment: process mechanics, Homeboy's own
/// `HOMEBOY_*` configuration, the `extra_names` the caller declares, and its
/// explicit `overrides`. Credential-named variables are never admitted as
/// public values.
pub fn admitted_homeboy_step_environment(
    extra_names: impl IntoIterator<Item = String>,
    overrides: impl IntoIterator<Item = (String, String)>,
) -> EnvMaterializationPlan {
    let names = crate::agent_task_provider::BASE_INHERITED_ENV
        .iter()
        .chain(STEP_PROCESS_ENV)
        .map(|name| name.to_string())
        .chain(extra_names);
    let policy = homeboy_core::redaction::RedactionPolicy::default();
    let mut public_env = BTreeMap::new();
    for name in names {
        if let Ok(value) = std::env::var(&name) {
            public_env.insert(name, value);
        }
    }
    for (name, value) in std::env::vars() {
        if name.starts_with(HOMEBOY_ENV_PREFIX) && !is_invocation_context_env(&name) {
            public_env.insert(name, value);
        }
    }
    // A runner-hosted exec's temp directory belongs to that job and is removed
    // when it ends; work admitted from inside it outlives the job.
    if admitted_inside_runner_job() {
        for name in ["TMPDIR", "TMP", "TEMP"] {
            public_env.remove(name);
        }
    }
    public_env.extend(overrides);
    public_env.retain(|name, _| !policy.is_credential_key_name(name));
    let mut plan = EnvMaterializationPlan {
        public_env,
        secret_refs: Vec::<EnvSecretRef>::new(),
        ..EnvMaterializationPlan::default()
    };
    plan.normalize();
    plan
}

/// The step that runs an initial detached Cook for one launcher generation.
pub fn cook_launch_step_request(
    cook_id: &str,
    launcher_id: &str,
    program: &Path,
    args: Vec<String>,
    cwd: &Path,
    env: EnvMaterializationPlan,
) -> StepJobRequest {
    StepJobRequest::new(
        format!("{COOK_LAUNCH_STEP_KEY_PREFIX}{cook_id}:{launcher_id}"),
        program.display().to_string(),
        args,
        cwd.display().to_string(),
        env,
    )
    .with_linked_durable_run_id(cook_id)
}

/// The step that runs one reserved local Cook retry. The key is the retry run,
/// so a launcher that takes over a crashed one converges on the same step.
pub fn cook_retry_step_request(
    run_id: &str,
    program: &Path,
    args: Vec<String>,
    cwd: &Path,
    env: EnvMaterializationPlan,
) -> StepJobRequest {
    StepJobRequest::new(
        format!("{COOK_RETRY_STEP_KEY_PREFIX}{run_id}"),
        program.display().to_string(),
        args,
        cwd.display().to_string(),
        env,
    )
    .with_linked_durable_run_id(run_id)
}

/// Register the step driver plus the Cook-side hooks it consults. Idempotent.
pub fn register_cook_step_hooks() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        step_job::register_step_job_driver();
        step_job::register_step_secret_resolver(Arc::new(|refs: &[EnvSecretRef]| {
            let names = refs
                .iter()
                .map(|secret| secret.name.clone())
                .collect::<Vec<_>>();
            crate::agent_task_secrets::resolve_configured_secret_env(&names).map_err(|error| {
                let mut failure = Error::validation_invalid_argument(
                    "step.env.secret_refs",
                    error.message,
                    None,
                    None,
                );
                failure.details["missing_secret_env"] = json!(error.missing_secret_env);
                failure
            })
        }));
        // A workflow `cook` step's outcome is its Cook's terminal run state.
        homeboy_core::daemon::workflow_job::register_workflow_run_outcome_resolver(Arc::new(
            |cook_id: &str| workflow_cook_outcome(cook_id),
        ));
        // Per-child progress of a running Cook step (#15689).
        homeboy_core::daemon::workflow_job::register_workflow_run_progress_resolver(Arc::new(
            |cook_id: &str| workflow_cook_progress(cook_id),
        ));
        step_job::register_step_exit_observer(Arc::new(|exit: &StepExit| {
            if let Err(error) = observe_cook_step_exit(exit) {
                eprintln!(
                    "Cook step {} exit reconciliation failed: {}",
                    exit.job_id, error.message
                );
            }
        }));
    });
}

pub(super) fn observe_cook_step_exit(exit: &StepExit) -> Result<()> {
    let Some(linked) = exit.linked_durable_run_id.as_deref() else {
        return Ok(());
    };
    // Every other linked step runs a Cook that bound its own handoff: a
    // detached launch, or a workflow `cook` step.
    if exit.key.starts_with(COOK_RETRY_STEP_KEY_PREFIX) {
        observe_retry_exit(linked, exit)
    } else {
        observe_launch_exit(linked, exit)
    }
}

/// The record that speaks for a Cook, or why it cannot answer yet.
///
/// The latest attempt the durable recipe declares speaks for the Cook. The
/// handoff parent speaks only for a Cook it never redirected to an attempt
/// (one that failed before materializing, which the step exit observer
/// records); a redirected parent's own state says nothing about the Cook's
/// outcome, so an unreadable recipe or attempt then means "not yet".
fn workflow_cook_record(
    cook_id: &str,
) -> std::result::Result<agent_task_lifecycle::AgentTaskRunRecord, WorkflowRunOutcome> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        .map_err(|_| WorkflowRunOutcome::Pending)?;
    let parent = store.read_record(cook_id).ok();
    let redirected = parent
        .as_ref()
        .is_some_and(|parent| parent.metadata["detached_cook_handoff"]["state"] == "redirected");
    let latest = super::load_recipe(cook_id)
        .ok()
        .and_then(|recipe| recipe.attempts.last().map(|attempt| attempt.run_id.clone()));
    if let Some(attempt) = latest.and_then(|run_id| store.read_record(&run_id).ok()) {
        return Ok(attempt);
    }
    match parent {
        _ if redirected => Err(WorkflowRunOutcome::Pending),
        Some(parent) => Ok(parent),
        None => Err(WorkflowRunOutcome::Unknown),
    }
}

/// The settled canonical state of a Cook. A terminal attempt whose terminal
/// continuation (promotion and PR finalization) is still queued or running has
/// not settled: the continuation decides its final state.
fn workflow_cook_outcome(cook_id: &str) -> WorkflowRunOutcome {
    let record = match workflow_cook_record(cook_id) {
        Ok(record) => record,
        Err(outcome) => return outcome,
    };
    if !record.state.is_terminal() {
        return WorkflowRunOutcome::Pending;
    }
    let continuation = super::CookRecipeStore::from_current_data_root()
        .and_then(|store| super::continuation_state_in_store(&store, cook_id, &record.run_id));
    match continuation {
        Ok(super::CookContinuationState::Pending | super::CookContinuationState::Claimed) => {
            return WorkflowRunOutcome::Pending;
        }
        // A runner-executed attempt that produced a candidate is promoted and
        // finalized by the terminal continuation the controller admits once
        // it projects the runner's result: until that continuation exists and
        // ends, the Cook has not settled.
        Ok(super::CookContinuationState::Absent)
            if record.runner_id().is_some()
                && matches!(
                    record.state,
                    agent_task_lifecycle::AgentTaskRunState::Succeeded
                        | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                        | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
                ) =>
        {
            return WorkflowRunOutcome::Pending;
        }
        _ => {}
    }
    // The Cook's outcome is its canonical subject read, the one `agent-task
    // status <cook>` and the control-plane API serve: an attempt record alone
    // reports a provider attempt, not the candidate, gates, and publication
    // that decide whether the Cook produced its result.
    let Ok(run) = crate::orchestration::run_from_current_environment(cook_id) else {
        return WorkflowRunOutcome::Pending;
    };
    if !run.state.is_terminal() {
        return WorkflowRunOutcome::Pending;
    }
    match serde_json::to_value(run.state)
        .ok()
        .and_then(|state| state.as_str().map(str::to_string))
    {
        Some(state) => WorkflowRunOutcome::Settled(state),
        None => WorkflowRunOutcome::Unknown,
    }
}

/// The live state and phase of a Cook's latest attempt, `None` before its
/// durable record exists.
fn workflow_cook_progress(cook_id: &str) -> Option<Value> {
    let record = workflow_cook_record(cook_id).ok()?;
    Some(json!({
        "run_id": record.run_id,
        "state": record.state,
        "phase": record
            .metadata
            .get("phase")
            .or_else(|| record.metadata.get("current_phase"))
            .cloned()
            .unwrap_or_else(|| json!(record.state)),
        "updated_at": record.updated_at,
        "provider": record.provider_handles.last().map(|handle| &handle.backend),
    }))
}

fn observe_launch_exit(cook_id: &str, exit: &StepExit) -> Result<()> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let parent = store.read_record(cook_id)?;
    if exit.cancelled {
        // A workflow `cook` step derives its Cook ID from its own step key, so
        // it is that Cook's only owner: cancel the Cook, attempt and runner
        // job included. A detached launch only cancels the launch generation
        // it still owns.
        if !exit.key.starts_with(COOK_LAUNCH_STEP_KEY_PREFIX) {
            let _ = agent_task_lifecycle::cancel_run_in_store(
                &store,
                cook_id,
                Some("workflow Cook step cancelled"),
            );
        } else if let Some(launcher_id) =
            parent.metadata["detached_cook_handoff"]["launcher_id"].as_str()
        {
            let _ = agent_task_lifecycle::cancel_claimed_detached_cook_in_store(
                &store,
                cook_id,
                launcher_id,
                Some("Cook step cancelled"),
            );
        }
        return Ok(());
    }
    // A handoff that never materialized an attempt has no other owner left.
    let parent = agent_task_lifecycle::fail_detached_cook_handoff_parent_in_store(
        &store,
        cook_id,
        "detached Cook exited before materializing its first attempt",
    )?;
    if parent.metadata["detached_cook_handoff"]["state"] != "redirected" {
        return Ok(());
    }
    let Ok(index) = store.read_cook_index(cook_id) else {
        return Ok(());
    };
    let attempt = index.latest_run_id;
    let record = store.read_record(&attempt)?;
    if !record.state.is_terminal() {
        agent_task_lifecycle::record_interrupted_local_owner_in_store(&store, &attempt)?;
    }
    notify_cook_attempt(cook_id, &attempt)
}

fn observe_retry_exit(run_id: &str, exit: &StepExit) -> Result<()> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let record = store.read_record(run_id)?;
    let Some(cook_id) = record.metadata["cook_id"].as_str().map(str::to_string) else {
        return Ok(());
    };
    if exit.cancelled {
        if !record.state.is_terminal() {
            let _ = agent_task_lifecycle::cancel_run_in_store(
                &store,
                run_id,
                Some("Cook retry step cancelled"),
            );
        }
        return Ok(());
    }
    if !record.state.is_terminal() {
        let provider_started = record
            .metadata
            .get("provider_executions")
            .and_then(Value::as_array)
            .is_some_and(|executions| !executions.is_empty());
        if provider_started {
            agent_task_lifecycle::record_interrupted_local_owner_in_store(&store, run_id)?;
        } else {
            let plan = store.read_controller_plan(run_id)?;
            agent_task_lifecycle::record_pre_execution_failure_in_store(
                &store,
                run_id,
                &plan,
                "local_retry_supervisor",
                &retry_child_failure(&exit.log_path, run_id),
            )?;
        }
    }
    notify_cook_attempt(&cook_id, run_id)
}

fn notify_cook_attempt(cook_id: &str, attempt_run_id: &str) -> Result<()> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let record = store.read_record(attempt_run_id)?;
    if !record.state.is_terminal() {
        return Ok(());
    }
    let exit_code = i32::from(record.state != agent_task_lifecycle::AgentTaskRunState::Succeeded);
    crate::agent_task_notify::cook_terminal_for_attempt(
        cook_id,
        attempt_run_id,
        attempt_status_label(record.state),
        exit_code,
    );
    Ok(())
}

#[cfg(test)]
pub(crate) fn finalize_detached_cook_attempt(cook_id: &str, attempt_run_id: &str) -> Result<()> {
    notify_cook_attempt(cook_id, attempt_run_id)
}

fn attempt_status_label(state: agent_task_lifecycle::AgentTaskRunState) -> &'static str {
    use agent_task_lifecycle::AgentTaskRunState;
    match state {
        AgentTaskRunState::Queued => "queued",
        AgentTaskRunState::Running => "running",
        AgentTaskRunState::Succeeded => "succeeded",
        AgentTaskRunState::CandidateRecoverable => "candidate_recoverable",
        AgentTaskRunState::PartialRecoverable => "partial_recoverable",
        AgentTaskRunState::PartialFailure => "partial_failure",
        AgentTaskRunState::Failed => "failed",
        AgentTaskRunState::Cancelled => "cancelled",
    }
}

/// The retry child's own typed failure, read from the tail of its step log,
/// so a retry that failed before provider execution reports why.
fn retry_child_failure(log_path: &Path, run_id: &str) -> Error {
    let Some(value) = last_failed_command_result(log_path) else {
        return retry_child_diagnostics_unavailable(log_path);
    };
    let diagnostic = value
        .get("diagnostics")
        .or_else(|| value.get("error"))
        .or_else(|| value.pointer("/data/failure_context/diagnostic"))
        .filter(|diagnostic| diagnostic.is_object())
        .cloned();
    let Some(diagnostic) = diagnostic else {
        return retry_child_diagnostics_unavailable(log_path);
    };
    let (Some(code), Some(message)) = (
        diagnostic.get("code").and_then(Value::as_str),
        diagnostic.get("message").and_then(Value::as_str),
    ) else {
        return retry_child_diagnostics_unavailable(log_path);
    };
    let mut evidence = json!({
        "schema": value.get("schema"),
        "command": value.get("command"),
        "operation": value.get("operation"),
        "status": value.get("status"),
        "diagnostic": diagnostic,
        "failure_context": value.pointer("/data/failure_context"),
        "child_result_evidence": {
            "kind": "detached-child-command-result",
            "evidence_uri": format!("homeboy://agent-task/run/{run_id}/status#detached-child-command-result"),
        },
    });
    evidence = homeboy_core::redaction::redact_json(&evidence);
    bound_child_diagnostic(&mut evidence, 0);
    let classification = diagnostic
        .pointer("/details/classification")
        .or_else(|| value.pointer("/data/failure_context/classification"))
        .or_else(|| value.pointer("/data/terminal_failure_classification"))
        .and_then(Value::as_str);
    let mut error = Error::new(
        match code {
            "storage.exhausted" => ErrorCode::StorageExhausted,
            _ => ErrorCode::InternalUnexpected,
        },
        homeboy_core::redaction::redact_string(message),
        json!({
            "field": code,
            "child_reported_error_code": code,
            "child_error_code": code,
            "child_failure_classification": classification,
            "child_command_result": evidence,
        }),
    )
    .with_retryable(true);
    if let Some(hints) = diagnostic.get("hints").and_then(Value::as_array) {
        for hint in hints
            .iter()
            .filter_map(|hint| hint.get("message").and_then(Value::as_str))
            .take(4)
        {
            error = error.with_hint(homeboy_core::redaction::redact_string(hint));
        }
    }
    error
}

fn last_failed_command_result(log_path: &Path) -> Option<Value> {
    let mut file = std::fs::File::open(log_path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(
        length.saturating_sub(CHILD_RESULT_TAIL_BYTES),
    ))
    .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    bytes
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, byte)| **byte == b'{')
        .filter_map(|(start, _)| {
            let mut stream = serde_json::Deserializer::from_slice(&bytes[start..]).into_iter();
            stream.next()?.ok()
        })
        .find(|value: &Value| {
            value["schema"] == "homeboy/command-result/v3" && value["success"] == Value::Bool(false)
        })
}

fn retry_child_diagnostics_unavailable(log_path: &Path) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        "local Cook retry exited before provider execution",
        json!({
            "field": "local_retry_supervisor",
            "child_diagnostics": {
                "status": "unavailable",
                "reason": "no_failed_command_result",
                "step_log": log_path.display().to_string(),
            },
        }),
    )
    .with_retryable(true)
}

fn bound_child_diagnostic(value: &mut Value, depth: usize) {
    const TEXT_LIMIT: usize = 2048;
    if depth >= 4 {
        *value = Value::String("[omitted: diagnostic depth limit]".to_string());
        return;
    }
    match value {
        Value::String(text) if text.len() > TEXT_LIMIT => {
            let mut end = TEXT_LIMIT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("...[truncated]");
        }
        Value::Array(values) => {
            values.truncate(8);
            for value in values {
                bound_child_diagnostic(value, depth + 1);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                bound_child_diagnostic(value, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::{with_isolated_home, EnvVarGuard};

    #[test]
    fn admitted_environment_is_declared_mechanics_and_homeboy_configuration_only() {
        with_isolated_home(|_| {
            let _ambient = EnvVarGuard::set("COOK_STEP_UNDECLARED_AMBIENT", "ambient");
            let _homeboy = EnvVarGuard::set("HOMEBOY_COOK_STEP_FIXTURE", "homeboy-config");
            let _credential = EnvVarGuard::set("HOMEBOY_COOK_STEP_FIXTURE_TOKEN", "secret");
            let plan = admitted_cook_environment([(
                "HOMEBOY_COOK_STEP_OVERRIDE".to_string(),
                "override".to_string(),
            )])
            .expect("admit environment");
            assert!(plan.public_env.contains_key("PATH"));
            assert!(plan.public_env.contains_key("HOME"));
            assert_eq!(
                plan.public_env
                    .get("HOMEBOY_COOK_STEP_FIXTURE")
                    .map(String::as_str),
                Some("homeboy-config")
            );
            assert_eq!(
                plan.public_env
                    .get("HOMEBOY_COOK_STEP_OVERRIDE")
                    .map(String::as_str),
                Some("override")
            );
            assert!(!plan.public_env.contains_key("COOK_STEP_UNDECLARED_AMBIENT"));
            assert!(!plan
                .public_env
                .contains_key("HOMEBOY_COOK_STEP_FIXTURE_TOKEN"));
            assert!(plan.secret_refs.is_empty());
        });
    }

    #[test]
    fn admitted_environment_drops_the_admitting_process_execution_context() {
        with_isolated_home(|_| {
            let _job = EnvVarGuard::set("HOMEBOY_RUNNER_JOB_ID", "runner-job");
            let _run = EnvVarGuard::set("HOMEBOY_RUN_ID", "runner-exec-run");
            let _frame = EnvVarGuard::set("HOMEBOY_DAEMON_STATE_DIR", "/runner/daemon");
            let _tmp = EnvVarGuard::set("TMPDIR", "/tmp/runner-job.t");
            let _config = EnvVarGuard::set("HOMEBOY_COOK_STEP_CONTEXT_FIXTURE", "kept");
            let plan = admitted_cook_environment(Vec::new()).expect("admit environment");
            for name in [
                "HOMEBOY_RUNNER_JOB_ID",
                "HOMEBOY_RUN_ID",
                "HOMEBOY_DAEMON_STATE_DIR",
                "TMPDIR",
            ] {
                assert!(!plan.public_env.contains_key(name), "{name} leaked");
            }
            assert!(plan
                .public_env
                .contains_key("HOMEBOY_COOK_STEP_CONTEXT_FIXTURE"));
        });
    }

    #[test]
    fn launch_and_retry_steps_are_keyed_and_linked_to_their_durable_runs() {
        let launch = cook_launch_step_request(
            "cook-a",
            "launcher-1",
            Path::new("/bin/homeboy"),
            vec!["--wait".to_string()],
            Path::new("/tmp"),
            EnvMaterializationPlan::default(),
        );
        assert_eq!(launch.key, "agent-task-cook-launch:cook-a:launcher-1");
        assert_eq!(launch.linked_durable_run_id.as_deref(), Some("cook-a"));
        let retry = cook_retry_step_request(
            "cook-a-attempt-2",
            Path::new("/bin/homeboy"),
            Vec::new(),
            Path::new("/tmp"),
            EnvMaterializationPlan::default(),
        );
        assert_eq!(retry.key, "agent-task-cook-retry:cook-a-attempt-2");
        assert_eq!(
            retry.linked_durable_run_id.as_deref(),
            Some("cook-a-attempt-2")
        );
    }

    #[test]
    fn retry_failure_reads_the_last_typed_command_result_from_the_step_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("step.log");
        std::fs::write(
            &log,
            format!(
                "progress line\n{}\n",
                json!({
                    "schema": "homeboy/command-result/v3",
                    "success": false,
                    "diagnostics": { "code": "storage.exhausted", "message": "disk is full" },
                })
            ),
        )
        .unwrap();
        let error = retry_child_failure(&log, "run-a");
        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert_eq!(error.message, "disk is full");
        let missing = retry_child_failure(&dir.path().join("absent.log"), "run-a");
        assert_eq!(
            missing.details["child_diagnostics"]["status"],
            "unavailable"
        );
    }
}
