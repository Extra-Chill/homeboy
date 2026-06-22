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
    AgentTaskLoopRunRef, AgentTaskLoopTaskLineage, AgentTaskLoopTerminalStatus,
    AgentTaskLoopTransition, AgentTaskPrOwnershipRequest, AgentTaskPrOwnershipState,
    AgentTaskPrOwnershipStatusUpdate,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::core::agent_task_service::{self, AgentTaskRunResult};
use crate::core::git::{pr_find, pr_view, PrFindOptions, PrState};
use crate::core::plan::{HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus};
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

mod action_state;
mod actions;
mod artifacts;
mod dispatch_defaults;
mod pr_ownership;
mod reports;
mod request;
mod spec;
mod spec_compile;

use action_state::*;
use actions::*;
use artifacts::*;
pub use dispatch_defaults::*;
use pr_ownership::*;
pub use reports::*;
pub use request::*;
pub use spec::*;
pub(crate) use spec_compile::validate_loop_spec;
use spec_compile::{
    compile_loop_spec_policy, compile_loop_spec_workflows, controller_spec_homeboy_plan,
    merge_policy_into_event_payload, reconcile_repo_loop_spec_actions, repo_loop_spec_fingerprint,
    repo_loop_spec_fingerprint_from_metadata, set_repo_loop_spec_metadata,
    RepoLoopSpecReconciliation, REPO_LOOP_SPEC_ACTION_REASON, REPO_LOOP_SPEC_WORKFLOW_REASON,
};

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
    })
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

/// Drain pending controller actions until none remain or one fails.
pub fn resume<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerResumeReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut results = Vec::new();
    loop {
        let record = controller::controller_status(loop_id)?;
        let Some(action_id) = first_pending_action_id(&record) else {
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: false,
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
                    results,
                    controller: record,
                },
                exit_code: action_result.exit_code,
            });
        }
    }
}

#[cfg(test)]
mod tests;
