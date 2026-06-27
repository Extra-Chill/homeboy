//! Durable agent-task controller execution service.
//!
//! Owns the controller execution policy that used to live in the CLI adapter.
//! Callers (CLI, daemon, future automation) build typed requests, hand them to
//! the service, and serialize the typed reports the service returns. The CLI
//! adapter is responsible only for argument parsing and JSON envelope rendering.
//!
//! Reports keep their existing JSON shapes via `serde` so the CLI continues to
//! emit the same envelopes after the move.

use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskTypedArtifact, AgentTaskWorkflowEvidence,
};
use crate::core::agent_task_lifecycle as lifecycle;
use crate::core::agent_task_loop_controller::{
    self as controller, AgentTaskGateBundle, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleResult, AgentTaskGateBundleStatus, AgentTaskGateCheckResult,
    AgentTaskLoopActionDiagnostic, AgentTaskLoopActionStatus, AgentTaskLoopArtifactRef,
    AgentTaskLoopControllerRecord, AgentTaskLoopControllerState, AgentTaskLoopEntity,
    AgentTaskLoopExternalEvent, AgentTaskLoopHistoryEvent, AgentTaskLoopPolicy,
    AgentTaskLoopPolicyAction, AgentTaskLoopPolicyActionRecord, AgentTaskLoopProvenanceRef,
    AgentTaskLoopRunRef, AgentTaskLoopRunnerAvailability, AgentTaskLoopRunnerExecutionTarget,
    AgentTaskLoopTaskLineage, AgentTaskLoopTerminalStatus, AgentTaskLoopTransition,
    AgentTaskPrOwnershipRequest, AgentTaskPrOwnershipState, AgentTaskPrOwnershipStatusUpdate,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::core::agent_task_service::{self, AgentTaskRunResult};
use crate::core::git::{pr_find, pr_view, PrFindOptions, PrState};
use crate::core::plan::{HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus};
use crate::core::runner::{self, RunnerActiveJobState};
use crate::core::{Error, Result};
use std::collections::HashMap;
use std::process::Command;

/// Schema for the apply-event report envelope.
pub const APPLY_EVENT_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-event-result/v1";
/// Schema for single-action run reports (run-next and run).
pub const ACTION_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-action-result/v1";
/// Schema for the multi-action resume report envelope.
pub const RESUME_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-resume-result/v1";
/// Schema for the list-controllers report envelope.
pub const LIST_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-list/v1";
/// Schema for repo-authored loop-spec initialization reports.
pub const FROM_SPEC_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-from-spec-result/v1";
/// Schema for dry controller-spec plan reports.
pub const PLAN_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-plan-result/v1";
/// Schema for from-spec executable agent-task plan compilation reports.
pub const EXECUTABLE_PLAN_RESULT_SCHEMA: &str =
    "homeboy/agent-task-loop-controller-executable-plan-result/v1";

mod action_state;
mod actions;
mod artifacts;
mod dispatch_defaults;
mod pr_ownership;
mod proof;
mod reports;
mod request;
mod run_failure_summary;
mod spec;
mod spec_compile;
mod spec_source;

use action_state::*;
use actions::*;
use artifacts::*;
pub use dispatch_defaults::*;
use pr_ownership::*;
pub use proof::{
    derive_proof_identity, prepare_controller_proof, resolve_proof_profile, CatalogReadinessProbe,
    ControllerProofIdentity, ControllerProofPreflightCheck, ControllerProofPreparation,
    ControllerProofProfile, ProcessSecretEnv, ProofReadinessProbe, ProofSecretEnv,
    CONTROLLER_PROOF_PREFLIGHT_SCHEMA,
};
pub use reports::*;
pub use request::*;
pub use run_failure_summary::{
    build_run_failure_summary, ControllerRunEvidenceRef, ControllerRunFailureSummary,
    CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA,
};
pub use spec::*;
#[cfg(test)]
pub(crate) use spec_compile::validate_artifact_flow_bindings;
pub(crate) use spec_compile::{
    compile_executable_plan_from_spec, homeboy_runtime_artifacts, validate_loop_spec,
};
use spec_compile::{
    compile_loop_spec_policy, compile_loop_spec_workflows, controller_spec_homeboy_plan,
    merge_policy_into_event_payload, reconcile_repo_loop_spec_actions, repo_loop_spec_fingerprint,
    repo_loop_spec_fingerprint_from_metadata, set_repo_loop_spec_metadata,
    RepoLoopSpecReconciliation, REPO_LOOP_SPEC_ACTION_REASON, REPO_LOOP_SPEC_WORKFLOW_REASON,
};
pub use spec_source::{load_materialize_spec_source, MaterializeSpecSource};

const DEFAULT_CONTROLLER_RESUME_MAX_ACTIONS: usize = 100;

/// Hard lifetime cap on the number of actions a single controller may
/// accumulate in `next_actions`. Each executed action can record further
/// follow-up actions (gates, PR-ownership polls, retries, etc.), and several of
/// those actions are non-dedupable, so a stuck loop grows its action log
/// without bound across repeated `run`/`resume` cycles. When the cap is
/// reached the controller escalates to a terminal state instead of executing
/// (and recording) more actions — mirroring the deterministic loop's
/// max-iteration guard.
pub(crate) const MAX_CONTROLLER_LIFETIME_ACTIONS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerResumeOptions {
    pub max_actions: usize,
    pub stop_on_terminal: bool,
}

impl Default for ControllerResumeOptions {
    fn default() -> Self {
        Self {
            max_actions: DEFAULT_CONTROLLER_RESUME_MAX_ACTIONS,
            stop_on_terminal: true,
        }
    }
}

/// Create a new durable controller record.
pub fn init(request: ControllerInitRequest) -> Result<AgentTaskLoopControllerRecord> {
    controller::create_controller(&request.loop_id, &request.phase, &request.config_version)
}

/// Initialize or resume a controller from a repo-owned loop spec and queue executable actions.
pub fn init_from_spec(request: ControllerFromSpecRequest) -> Result<ControllerFromSpecReport> {
    let spec = request.spec;
    validate_loop_spec(&spec)?;
    let spec_fingerprint = repo_loop_spec_fingerprint(&spec)?;
    let mut initialized = false;
    let mut record = match existing_controller(&spec.loop_id)? {
        Some(record) => record,
        None => {
            initialized = true;
            AgentTaskLoopControllerRecord::new(
                spec.loop_id.clone(),
                spec.phase.clone(),
                spec.config_version.clone(),
            )
        }
    };
    let previous_spec_fingerprint = repo_loop_spec_fingerprint_from_metadata(&record);
    let reconciliation = if initialized {
        RepoLoopSpecReconciliation::default()
    } else {
        reconcile_repo_loop_spec_actions(
            &mut record,
            previous_spec_fingerprint.as_deref(),
            &spec_fingerprint,
        )?
    };

    record.phase = spec.phase.clone();
    record.config_version = spec.config_version.clone();
    if !spec.metadata.is_null() {
        record.metadata = spec.metadata.clone();
    }
    for entity in &spec.entities {
        record.upsert_entity(
            entity.entity_type.clone(),
            entity.key.clone(),
            entity.parent_entity_ids.clone(),
            entity.metadata.clone(),
        );
    }
    for bundle in &spec.gate_bundles {
        if let Some(existing) = record
            .gate_bundles
            .iter_mut()
            .find(|existing| existing.bundle_id == bundle.bundle_id)
        {
            *existing = bundle.clone();
        } else {
            record.gate_bundles.push(bundle.clone());
        }
    }

    let mut actions = Vec::new();
    for action in compile_loop_spec_workflows(&spec)? {
        actions.push(record.record_action(action, REPO_LOOP_SPEC_WORKFLOW_REASON));
    }
    for action in &spec.actions {
        actions.push(record.record_action(action.clone(), REPO_LOOP_SPEC_ACTION_REASON));
    }
    if let Some(policy) = compile_loop_spec_policy(&spec) {
        if let Some(event) = spec.initial_event.clone() {
            let event_id = event
                .event_id
                .unwrap_or_else(|| format!("loop-spec-event-{}", record.history.len() + 1));
            let payload = merge_policy_into_event_payload(event.payload, policy);
            actions.extend(record.apply_event(AgentTaskLoopExternalEvent {
                event_id,
                event_type: event.event_type,
                event_key: event.event_key,
                entity_id: event.entity_id,
                payload,
            }));
        } else {
            actions.extend(record.evaluate_policy(&policy, None));
        }
    }
    set_repo_loop_spec_metadata(&mut record, &spec, &spec_fingerprint);
    push_controller_history(
        &mut record,
        "controller.loop_spec.applied",
        None,
        serde_json::json!({
            "schema": spec.schema,
            "initialized": initialized,
            "spec_fingerprint": spec_fingerprint,
            "previous_spec_fingerprint": previous_spec_fingerprint,
            "reconciled_action_count": reconciliation.removed_action_count,
            "reconciled_dedupe_key_count": reconciliation.removed_dedupe_key_count,
            "queued_action_count": actions.iter().filter(|action| action.status == AgentTaskLoopActionStatus::Pending).count(),
        }),
    );
    controller::write_controller(&record)?;
    Ok(ControllerFromSpecReport {
        schema: FROM_SPEC_RESULT_SCHEMA,
        loop_id: record.loop_id.clone(),
        initialized,
        actions,
        controller: record,
        resume_state: None,
    })
}

/// Initialize a controller from a repo-owned loop spec for immediate resume.
///
/// Defaults to the guarded resolution, which fails closed when an existing
/// controller was created from a different (or missing) spec fingerprint. That
/// keeps proof reruns from silently draining stale actions after a loop spec
/// changed (#6123).
pub fn init_from_spec_for_resume(
    request: ControllerFromSpecRequest,
) -> Result<ControllerFromSpecReport> {
    init_from_spec_for_resume_with_resolution(request, ControllerResumeStateResolution::default())
}

/// Initialize a controller for resume, applying an explicit stale-state resolution.
///
/// When the supplied spec fingerprint matches the persisted controller (or no
/// controller exists yet) the run proceeds normally regardless of resolution.
/// When the persisted fingerprint is missing or different, the resolution
/// decides the outcome:
///
/// - [`ControllerResumeStateResolution::Guard`] refuses with a clear error.
/// - [`ControllerResumeStateResolution::Replace`] discards the persisted record
///   and re-initializes from the spec.
/// - [`ControllerResumeStateResolution::Fork`] applies the spec under a derived
///   `loop_id`, leaving the original controller untouched.
/// - [`ControllerResumeStateResolution::ResumeExisting`] accepts the stale state
///   and resumes the persisted controller as-is.
pub fn init_from_spec_for_resume_with_resolution(
    mut request: ControllerFromSpecRequest,
    resolution: ControllerResumeStateResolution,
) -> Result<ControllerFromSpecReport> {
    let spec_fingerprint = repo_loop_spec_fingerprint(&request.spec)?;
    let requested_loop_id = request.spec.loop_id.clone();
    let existing = existing_controller(&requested_loop_id)?;
    let controller_path = controller::controller_record_path(&requested_loop_id)?
        .display()
        .to_string();

    let Some(record) = existing else {
        // No persisted state: this is a clean create regardless of resolution.
        let mut report = init_from_spec(request)?;
        report.resume_state = Some(ControllerResumeStateReport {
            action: "creating",
            resolution: resolution.keyword(),
            loop_id: requested_loop_id.clone(),
            requested_loop_id,
            controller_path,
            spec_fingerprint,
            previous_spec_fingerprint: None,
            existing_controller: false,
            fingerprint_match: false,
        });
        return Ok(report);
    };

    let previous = repo_loop_spec_fingerprint_from_metadata(&record);
    let fingerprint_match = previous.as_deref() == Some(spec_fingerprint.as_str());

    if fingerprint_match {
        // Persisted state is compatible: ordinary resume.
        let mut report = init_from_spec(request)?;
        report.resume_state = Some(ControllerResumeStateReport {
            action: "resuming",
            resolution: resolution.keyword(),
            loop_id: requested_loop_id.clone(),
            requested_loop_id,
            controller_path,
            spec_fingerprint,
            previous_spec_fingerprint: previous,
            existing_controller: true,
            fingerprint_match: true,
        });
        return Ok(report);
    }

    // Persisted state is stale/incompatible. Honor the operator's resolution.
    match resolution {
        ControllerResumeStateResolution::Guard => {
            let prior = previous
                .as_deref()
                .map(|fingerprint| format!("prior_spec_fingerprint={fingerprint}"))
                .unwrap_or_else(|| "prior_spec_fingerprint=<none>".to_string());
            Err(Error::validation_invalid_argument(
                "spec_fingerprint",
                format!(
                    "refusing to reuse stale persisted controller state for '{}': the persisted spec fingerprint is missing or different from the requested spec. Re-run with --reconcile-stale to safely reset run-scoped state automatically, or choose --replace, --fork, or --resume-existing; a fresh loop_id also avoids the conflict",
                    record.loop_id
                ),
                previous.clone(),
                Some(vec![
                    format!("state_path={controller_path}"),
                    prior,
                    format!("requested_spec_fingerprint={spec_fingerprint}"),
                    "safe_next_action=--reconcile-stale (auto reset run-scoped state, no manual cleanup)".to_string(),
                    "resolutions=--reconcile-stale|--replace|--fork|--resume-existing".to_string(),
                ]),
            ))
        }
        ControllerResumeStateResolution::Replace
        | ControllerResumeStateResolution::ReconcileStale => {
            // Both discard the stale persisted record and re-create isolated
            // run-scoped state from the spec; `ReconcileStale` is the one-flag
            // proof-run alias surfaced under its own evidence keyword (#6221).
            reset_controller_state(&record.loop_id)?;
            let mut report = init_from_spec(request)?;
            report.resume_state = Some(ControllerResumeStateReport {
                action: "replacing",
                resolution: resolution.keyword(),
                loop_id: requested_loop_id.clone(),
                requested_loop_id,
                controller_path,
                spec_fingerprint,
                previous_spec_fingerprint: previous,
                existing_controller: true,
                fingerprint_match: false,
            });
            Ok(report)
        }
        ControllerResumeStateResolution::Fork => {
            let fork_loop_id = derive_fork_loop_id(&requested_loop_id, &spec_fingerprint);
            request.spec.loop_id = fork_loop_id.clone();
            let fork_path = controller::controller_record_path(&fork_loop_id)?
                .display()
                .to_string();
            let mut report = init_from_spec(request)?;
            report.resume_state = Some(ControllerResumeStateReport {
                action: "forking",
                resolution: resolution.keyword(),
                loop_id: fork_loop_id,
                requested_loop_id,
                controller_path: fork_path,
                spec_fingerprint,
                previous_spec_fingerprint: previous,
                existing_controller: true,
                fingerprint_match: false,
            });
            Ok(report)
        }
        ControllerResumeStateResolution::ResumeExisting => {
            let mut report = init_from_spec(request)?;
            report.resume_state = Some(ControllerResumeStateReport {
                action: "resuming",
                resolution: resolution.keyword(),
                loop_id: requested_loop_id.clone(),
                requested_loop_id,
                controller_path,
                spec_fingerprint,
                previous_spec_fingerprint: previous,
                existing_controller: true,
                fingerprint_match: false,
            });
            Ok(report)
        }
    }
}

/// Derive an isolated fork loop id from the requested id and spec fingerprint.
///
/// Forks are operator-requested fresh runs. Include a nonce so repeated forks of
/// the same spec cannot collapse onto an earlier fork controller and inherit its
/// terminal child outcomes.
fn derive_fork_loop_id(requested_loop_id: &str, spec_fingerprint: &str) -> String {
    let short = spec_fingerprint
        .strip_prefix("sha256:")
        .unwrap_or(spec_fingerprint)
        .chars()
        .take(12)
        .collect::<String>();
    let nonce = Uuid::new_v4().simple().to_string();
    let nonce = &nonce[..12];
    // Use a sanitization-stable separator: the loop_id becomes a single path
    // segment (slashes are collapsed to `_` by sanitize_path_segment), so the
    // persisted `record.loop_id` would otherwise diverge from the derived id.
    format!("{requested_loop_id}-fork-{short}-{nonce}")
}

/// Remove a persisted controller record (and its directory) so a replace
/// resolution starts from a clean slate. Missing state is treated as success.
fn reset_controller_state(loop_id: &str) -> Result<()> {
    let record_path = controller::controller_record_path(loop_id)?;
    let dir = record_path.parent().unwrap_or(&record_path);
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(dir.display().to_string()),
        )),
    }
}

/// Compile a declarative controller spec into a generic Homeboy plan without writing state.
pub fn plan_from_spec(request: ControllerPlanRequest) -> Result<ControllerPlanReport> {
    let spec = request.spec;
    validate_loop_spec(&spec)?;
    let spec_fingerprint = repo_loop_spec_fingerprint(&spec)?;
    let mut record = AgentTaskLoopControllerRecord::new(
        spec.loop_id.clone(),
        spec.phase.clone(),
        spec.config_version.clone(),
    );
    if !spec.metadata.is_null() {
        record.metadata = spec.metadata.clone();
    }
    for entity in &spec.entities {
        record.upsert_entity(
            entity.entity_type.clone(),
            entity.key.clone(),
            entity.parent_entity_ids.clone(),
            entity.metadata.clone(),
        );
    }
    record.gate_bundles.extend(spec.gate_bundles.clone());

    let mut actions = Vec::new();
    for action in compile_loop_spec_workflows(&spec)? {
        actions.push(record.record_action(action, REPO_LOOP_SPEC_WORKFLOW_REASON));
    }
    for action in &spec.actions {
        actions.push(record.record_action(action.clone(), REPO_LOOP_SPEC_ACTION_REASON));
    }
    if let Some(policy) = compile_loop_spec_policy(&spec) {
        if let Some(event) = spec.initial_event.clone() {
            let event_id = event
                .event_id
                .unwrap_or_else(|| format!("loop-spec-event-{}", record.history.len() + 1));
            let payload = merge_policy_into_event_payload(event.payload, policy);
            actions.extend(record.apply_event(AgentTaskLoopExternalEvent {
                event_id,
                event_type: event.event_type,
                event_key: event.event_key,
                entity_id: event.entity_id,
                payload,
            }));
        } else {
            actions.extend(record.evaluate_policy(&policy, None));
        }
    }

    let plan = controller_spec_homeboy_plan(&spec, &spec_fingerprint, &record, &actions)?;
    Ok(ControllerPlanReport {
        schema: PLAN_RESULT_SCHEMA,
        loop_id: record.loop_id,
        spec_fingerprint,
        plan,
        actions,
        run_command: Some("homeboy agent-task controller from-spec <spec> --resume".to_string()),
    })
}

/// Compile a loop controller spec into an executable agent-task plan.
///
/// This is the from-spec compiler primitive requested in #5101: the executable
/// plan builder consumes the loop controller spec as the single source of truth.
/// It derives the executable plan stages and inter-stage dependencies from the
/// spec's workflows and `artifact_flow` (artifact_graph) edges, validates task
/// bindings against those edges, and represents Homeboy-owned runtime artifacts
/// (e.g. `static_validation_run`) as synthetic runtime stages so downstream
/// callers never hard-code Homeboy/Sandbox internals. No controller state is
/// written.
pub fn compile_plan_from_spec(
    request: ControllerPlanRequest,
) -> Result<ControllerExecutablePlanReport> {
    let spec = request.spec;
    let plan = compile_executable_plan_from_spec(&spec)?;
    let spec_fingerprint = repo_loop_spec_fingerprint(&spec)?;
    let runtime_artifacts = homeboy_runtime_artifacts(&spec)
        .into_iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect();
    Ok(ControllerExecutablePlanReport {
        schema: EXECUTABLE_PLAN_RESULT_SCHEMA,
        loop_id: spec.loop_id,
        spec_fingerprint,
        runtime_artifacts,
        plan,
    })
}

/// Read a durable controller record.
pub fn status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    controller::load_controller(loop_id)
}

/// List every durable controller record.
pub fn list() -> Result<ControllerListReport> {
    Ok(ControllerListReport {
        schema: LIST_RESULT_SCHEMA,
        controllers: controller::list_controllers()?,
    })
}

fn existing_controller(loop_id: &str) -> Result<Option<AgentTaskLoopControllerRecord>> {
    let requested = AgentTaskLoopControllerRecord::new(loop_id, "init", "v1").loop_id;
    Ok(controller::list_controllers()?
        .into_iter()
        .find(|record| record.loop_id == requested))
}

/// Mark a tracked entity as human-ready work and persist the controller.
pub fn mark_human_ready(
    request: ControllerMarkHumanReadyRequest,
) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = controller::load_controller(&request.loop_id)?;
    record.mark_human_ready(&request.entity_id, request.reason)?;
    controller::write_controller(&record)?;
    Ok(record)
}

/// Apply an external event to the controller and return the resulting actions.
pub fn apply_event(request: ControllerApplyEventRequest) -> Result<ControllerEventReport> {
    let mut record = controller::load_controller(&request.loop_id)?;
    let event_id = request
        .event_id
        .unwrap_or_else(|| format!("event-{}", record.history.len() + 1));
    let actions = record.apply_event(AgentTaskLoopExternalEvent {
        event_id,
        event_type: request.event_type,
        event_key: request.event_key,
        entity_id: request.entity_id,
        payload: request.payload,
    });
    controller::write_controller(&record)?;
    Ok(ControllerEventReport {
        schema: APPLY_EVENT_RESULT_SCHEMA,
        controller: record,
        actions,
    })
}

/// Claim and execute the first pending controller action, if any.
pub fn run_next<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut record = controller::controller_status(loop_id)?;
    let Some(action_id) = first_pending_action_id(&record) else {
        return Ok(AgentTaskRunResult {
            value: ControllerActionReport {
                schema: ACTION_RESULT_SCHEMA,
                loop_id: record.loop_id.clone(),
                claimed: false,
                action_id: None,
                status: None,
                failure_summary: None,
                runtime_evidence: None,
                execution: None,
                controller: record,
            },
            exit_code: 0,
        });
    };
    execute_controller_action(&mut record, &action_id, executor, dispatch)
}

/// Claim and execute the named pending controller action.
pub fn run_action<E, D>(
    loop_id: &str,
    action_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut record = controller::load_controller(loop_id)?;
    execute_controller_action(&mut record, action_id, executor, dispatch)
}

/// Drain pending controller actions until the default finite limit, idle, terminal state, or failure.
pub fn resume<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerResumeReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    resume_with_options(
        loop_id,
        executor,
        dispatch,
        ControllerResumeOptions::default(),
    )
}

/// Drain pending controller actions until the supplied finite options stop execution.
pub fn resume_with_options<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
    options: ControllerResumeOptions,
) -> Result<AgentTaskRunResult<ControllerResumeReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut results = Vec::new();
    while results.len() < options.max_actions {
        let record = controller::controller_status(loop_id)?;
        if options.stop_on_terminal && controller_state_is_terminal(record.state) {
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: false,
                    stopped_reason: "terminal_state".to_string(),
                    results,
                    controller: record,
                },
                exit_code: 0,
            });
        }
        let Some(action_id) = first_pending_action_id(&record) else {
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: false,
                    stopped_reason: "idle".to_string(),
                    results,
                    controller: record,
                },
                exit_code: 0,
            });
        };
        let action_result = run_action(loop_id, &action_id, executor.clone(), dispatch)?;
        let value = serde_json::to_value(&action_result.value)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        results.push(value);
        if action_result.exit_code != 0 {
            let record = controller::controller_status(loop_id)?;
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    stopped_reason: "action_failed".to_string(),
                    results,
                    controller: record,
                },
                exit_code: action_result.exit_code,
            });
        }
        if options.stop_on_terminal
            && controller_state_is_terminal(action_result.value.controller.state)
        {
            let record = controller::controller_status(loop_id)?;
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    stopped_reason: "terminal_state".to_string(),
                    results,
                    controller: record,
                },
                exit_code: 0,
            });
        }
    }
    let record = controller::controller_status(loop_id)?;
    Ok(AgentTaskRunResult {
        value: ControllerResumeReport {
            schema: RESUME_RESULT_SCHEMA,
            loop_id: record.loop_id.clone(),
            claimed: !results.is_empty(),
            stopped_reason: "max_actions_reached".to_string(),
            results,
            controller: record,
        },
        exit_code: 0,
    })
}

fn controller_state_is_terminal(state: AgentTaskLoopControllerState) -> bool {
    matches!(
        state,
        AgentTaskLoopControllerState::HumanReady
            | AgentTaskLoopControllerState::Completed
            | AgentTaskLoopControllerState::Abandoned
            | AgentTaskLoopControllerState::Escalated
            | AgentTaskLoopControllerState::Failed
    )
}

#[cfg(test)]
mod tests;
