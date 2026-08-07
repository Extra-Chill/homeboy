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
use uuid::Uuid;

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskTypedArtifact, AgentTaskWorkflowEvidence,
};
use crate::agent_task_lifecycle as lifecycle;
use crate::agent_task_loop_controller::{
    self as controller, AgentTaskGateBundle, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleResult, AgentTaskGateCheckResult, AgentTaskLoopActionDiagnostic,
    AgentTaskLoopActionStatus, AgentTaskLoopArtifactRef, AgentTaskLoopControllerRecord,
    AgentTaskLoopControllerState, AgentTaskLoopEntity, AgentTaskLoopExternalEvent,
    AgentTaskLoopGateStatus, AgentTaskLoopHistoryEvent, AgentTaskLoopPolicy,
    AgentTaskLoopPolicyAction, AgentTaskLoopPolicyActionRecord, AgentTaskLoopProvenanceRef,
    AgentTaskLoopRunRef, AgentTaskLoopRunnerAvailability, AgentTaskLoopTaskLineage,
    AgentTaskLoopTerminalStatus, AgentTaskLoopTransition, AgentTaskPrOwnershipRequest,
    AgentTaskPrOwnershipState, AgentTaskPrOwnershipStatusUpdate,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan};
use crate::agent_task_service::{self, AgentTaskRunResult};
use homeboy_core::git::{pr_find, pr_view, PrFindOptions, PrState};
use homeboy_core::plan::{HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus};
use homeboy_core::{Error, Result};
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
mod field_access;
pub mod loop_spec_validation_provider;
mod pr_ownership;
mod proof;
mod reports;
mod request;
mod run_command;
mod run_failure_summary;
pub mod runner_availability;
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
    build_run_failure_summary, ControllerRunFailureSummary, CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA,
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

/// Compute the stable repo-loop spec fingerprint for operator diagnostics.
pub fn spec_fingerprint_for_status(spec: &AgentTaskRepoLoopSpec) -> Result<String> {
    repo_loop_spec_fingerprint(spec)
}

/// Read the persisted repo-loop spec fingerprint from a controller record.
pub fn controller_spec_fingerprint_for_status(
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    repo_loop_spec_fingerprint_from_metadata(record)
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
    if replay_runner_terminal_event(&mut record, &request)? {
        controller::write_controller(&record)?;
        return Ok(ControllerEventReport {
            schema: APPLY_EVENT_RESULT_SCHEMA,
            controller: record,
            actions: Vec::new(),
        });
    }
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

/// Reconcile accepted Lab handoffs and open waits before selecting the next
/// pending action.
///
/// This is the reconnect path: lifecycle status refreshes the authoritative
/// runner snapshot, then the same typed terminal projection is applied. It
/// also resolves `WaitForEvent`/`WaitForController` waits whose subject is
/// already durably terminal — see [`reconcile_open_waits`] for the evidence
/// rule — because a controller parked in `Waiting` has no pending action, so
/// `resume` returns `idle` and exits without ever looking at the wait.
fn reconcile_waiting_runner_actions(record: &mut AgentTaskLoopControllerRecord) -> Result<bool> {
    let mut changed = false;
    for action in record
        .next_actions
        .clone()
        .into_iter()
        .filter(|action| action.status == AgentTaskLoopActionStatus::WaitingForRunner)
    {
        let Some(identity) = lab_runner_handoff_identity(&action) else {
            continue;
        };
        let run_id = identity["run_id"].as_str().unwrap_or_default();
        let run = lifecycle::status(run_id)?;
        if let Some(status) = terminal_runner_action_status(run.state) {
            project_runner_terminal_action(record, &action.action_id, &identity, status, None)?;
            changed = true;
        }
    }
    changed |= reconcile_open_waits(record)?.changed();
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Open-wait reconciliation (W3-9)
// ---------------------------------------------------------------------------

/// Wait event types whose subject is a durable agent-task run.
///
/// The allowlist is the whole safety argument. A wait carries only an event
/// type, an optional entity, and an opaque `external_ref`; nothing in that
/// shape says whether Homeboy can observe the thing being waited on. Resolving
/// by inference would satisfy a `github.pr.merged` wait because *some* local
/// record happened to be terminal, and a wait that resolves wrongly is worse
/// than one that stalls — the controller would advance past a gate the world
/// has not actually passed.
const RUN_TERMINAL_WAIT_EVENT_TYPES: &[&str] = &[
    "agent_task.runner_terminal",
    "agent_task.run_terminal",
    "agent_task.cook_terminal",
];

/// The event type `WaitForController` records for a child controller.
const CONTROLLER_TERMINAL_WAIT_EVENT_TYPE: &str = "controller.terminal";

/// Escalation policy value that turns an expired wait into a terminal
/// controller state rather than merely unblocking the loop.
const ESCALATE_ON_TIMEOUT_POLICY: &str = "escalate";

/// What one open-wait reconcile pass did to a controller.
#[derive(Debug, Clone, Default)]
pub struct WaitReconcileOutcome {
    /// Wait keys satisfied by durable evidence.
    pub resolved: Vec<String>,
    /// Wait keys whose declared `timeout_at` had passed.
    pub timed_out: Vec<String>,
    /// `true` when an expired wait escalated the controller to a terminal state.
    pub escalated: bool,
    /// Open waits remaining after the pass.
    pub open_waits: usize,
}

impl WaitReconcileOutcome {
    pub fn changed(&self) -> bool {
        !self.resolved.is_empty() || !self.timed_out.is_empty()
    }
}

/// Resolve open waits from durable run and controller state, and expire waits
/// whose declared deadline has passed.
///
/// # Evidence accepted
///
/// - A `controller.terminal` wait resolves when the named child controller's
///   persisted record is in one of the terminal states the wait declared (or
///   the default terminal set when it declared none). The child loop id comes
///   from the recorded subcontroller reference when there is one, because that
///   id is already sanitized the way the record on disk is named.
/// - A run-terminal wait (see [`RUN_TERMINAL_WAIT_EVENT_TYPES`]) resolves when
///   the run named by `external_ref` has a terminal durable lifecycle state.
///
/// # Evidence refused
///
/// - Any other `event_type`. `github.pr.checks_changed`, `github.pr.merged`,
///   and every operator-authored type describe things Homeboy does not
///   observe locally; there is no durable evidence to read, so the wait stays
///   open until something applies the event.
/// - A wait with no `external_ref`. Its only remaining identity is an entity
///   id, which names what the wait is *about*, not what would satisfy it.
///   Matching on that alone is a guess.
/// - A subject whose record cannot be read. A transient store error is not
///   evidence of terminality, exactly as it is not in the daemon's completion
///   notifier.
/// - A non-terminal subject. Keep waiting.
///
/// # Expiry
///
/// Only an explicitly declared `timeout_at` expires a wait. That field has
/// existed on the wait type since it was written and was never enforced
/// anywhere, so honouring it is the escalation the type was designed for. No
/// deadline is invented for a wait that declared none: guessing a timeout is
/// the same class of error as guessing evidence, and it would silently unblock
/// long-running waits that are working correctly. An unparseable `timeout_at`
/// never expires a wait — a malformed timestamp must not become a deadline.
fn reconcile_open_waits(
    record: &mut AgentTaskLoopControllerRecord,
) -> Result<WaitReconcileOutcome> {
    let now = chrono::Utc::now();
    let open: Vec<controller::AgentTaskLoopWait> = record
        .waits
        .iter()
        .filter(|wait| wait.status == controller::AgentTaskLoopWaitStatus::Open)
        .cloned()
        .collect();

    let mut satisfied: Vec<(String, String)> = Vec::new();
    let mut expired: Vec<(String, Option<String>)> = Vec::new();
    for wait in &open {
        if let Some(evidence) = durable_wait_evidence(record, wait) {
            satisfied.push((wait.wait_key.clone(), evidence));
            continue;
        }
        if wait_deadline_passed(wait, now) {
            expired.push((wait.wait_key.clone(), wait.escalation_policy.clone()));
        }
    }

    let mut outcome = WaitReconcileOutcome::default();
    for (wait_key, evidence) in &satisfied {
        let Some(wait) = open_wait_mut(record, wait_key) else {
            continue;
        };
        wait.status = controller::AgentTaskLoopWaitStatus::Satisfied;
        wait.satisfied_by_event_id = Some(evidence.clone());
        outcome.resolved.push(wait_key.clone());
    }
    for (wait_key, _) in &expired {
        let Some(wait) = open_wait_mut(record, wait_key) else {
            continue;
        };
        wait.status = controller::AgentTaskLoopWaitStatus::TimedOut;
        outcome.timed_out.push(wait_key.clone());
    }

    if !outcome.resolved.is_empty() {
        push_controller_history(
            record,
            "controller.wait.resolved",
            None,
            serde_json::json!({
                "wait_keys": outcome.resolved,
                "source": "durable_state_reconcile",
            }),
        );
    }
    if !outcome.timed_out.is_empty() {
        push_controller_history(
            record,
            "controller.wait.timed_out",
            None,
            serde_json::json!({
                "wait_keys": outcome.timed_out,
                "observed_at": now.to_rfc3339(),
            }),
        );
    }

    // Mirrors `apply_event`: a controller with nothing left to wait for is
    // runnable again, so its pending actions become reachable.
    if outcome.changed()
        && record.open_wait_count() == 0
        && record.state == AgentTaskLoopControllerState::Waiting
    {
        record.state = AgentTaskLoopControllerState::Running;
    }

    // Escalation is opt-in on the exact declared policy. A wait whose author
    // said nothing about escalation is unblocked, not terminalized.
    if expired
        .iter()
        .any(|(_, policy)| policy.as_deref() == Some(ESCALATE_ON_TIMEOUT_POLICY))
    {
        record.state = AgentTaskLoopControllerState::Escalated;
        outcome.escalated = true;
        push_controller_history(
            record,
            "controller.wait.escalated",
            None,
            serde_json::json!({
                "wait_keys": outcome.timed_out,
                "escalation_policy": ESCALATE_ON_TIMEOUT_POLICY,
            }),
        );
    }

    outcome.open_waits = record.open_wait_count();
    Ok(outcome)
}

fn open_wait_mut<'record>(
    record: &'record mut AgentTaskLoopControllerRecord,
    wait_key: &str,
) -> Option<&'record mut controller::AgentTaskLoopWait> {
    record.waits.iter_mut().find(|wait| {
        wait.wait_key == wait_key && wait.status == controller::AgentTaskLoopWaitStatus::Open
    })
}

/// Durable evidence that an open wait is already satisfied, or `None`.
///
/// Every failure path returns `None`. Refusing to resolve leaves the wait
/// exactly as it was, which is always recoverable; resolving on a guess is not.
fn durable_wait_evidence(
    record: &AgentTaskLoopControllerRecord,
    wait: &controller::AgentTaskLoopWait,
) -> Option<String> {
    if wait.event_type == CONTROLLER_TERMINAL_WAIT_EVENT_TYPE {
        return controller_wait_evidence(record, wait);
    }
    if RUN_TERMINAL_WAIT_EVENT_TYPES.contains(&wait.event_type.as_str()) {
        return run_wait_evidence(wait);
    }
    None
}

fn controller_wait_evidence(
    record: &AgentTaskLoopControllerRecord,
    wait: &controller::AgentTaskLoopWait,
) -> Option<String> {
    let subcontroller = record
        .subcontrollers
        .iter()
        .find(|sub| sub.wait_key.as_deref() == Some(wait.wait_key.as_str()));
    // The subcontroller reference is preferred: its loop id is already
    // sanitized the way the child's record is named on disk, where
    // `external_ref` carries whatever the policy author wrote.
    let child_loop_id = subcontroller
        .map(|sub| sub.loop_id.clone())
        .or_else(|| wait.external_ref.clone())?;
    if child_loop_id.trim().is_empty() {
        return None;
    }
    let terminal_states = controller::controller_terminal_states(
        subcontroller
            .map(|sub| sub.terminal_states.as_slice())
            .unwrap_or_default(),
    );
    // `load_controller`, not `controller_status`: reading the persisted record
    // is the evidence. `controller_status` refreshes child projections and
    // would recurse through a controller graph on every wait.
    let child = controller::load_controller(&child_loop_id).ok()?;
    terminal_states
        .contains(&child.state)
        .then(|| format!("controller-terminal:{child_loop_id}:{:?}", child.state))
}

fn run_wait_evidence(wait: &controller::AgentTaskLoopWait) -> Option<String> {
    let run_id = wait.external_ref.as_deref()?.trim();
    if run_id.is_empty() {
        return None;
    }
    let run = lifecycle::status(run_id).ok()?;
    run.state
        .is_terminal()
        .then(|| format!("run-terminal:{run_id}:{:?}", run.state))
}

fn wait_deadline_passed(
    wait: &controller::AgentTaskLoopWait,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(deadline) = wait.timeout_at.as_deref() else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(deadline)
        .map(|deadline| deadline.with_timezone(&chrono::Utc) <= now)
        // A malformed deadline is not a deadline.
        .unwrap_or(false)
}

/// Schema for the controller wait-reconcile report.
pub const WAIT_RECONCILE_RESULT_SCHEMA: &str = "homeboy/agent-task-controller-wait-reconcile/v1";

#[derive(Debug, Clone, Serialize)]
pub struct ControllerWaitReconcileEntry {
    pub loop_id: String,
    pub before_state: AgentTaskLoopControllerState,
    pub after_state: AgentTaskLoopControllerState,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolved_waits: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timed_out_waits: Vec<String>,
    pub open_waits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControllerWaitReconcileReport {
    pub schema: &'static str,
    pub considered: usize,
    pub changed: usize,
    pub failed: usize,
    pub controllers: Vec<ControllerWaitReconcileEntry>,
}

/// Sweep every durable controller with open waits and resolve what durable
/// state already satisfies.
///
/// This is the automatic driver `Waiting` never had. `resume` stops at `idle`
/// for a controller whose only remaining work is a wait, and nothing polled,
/// subscribed, or timed out — so a controller that dispatched a cook sat there
/// indefinitely even after that cook terminalized, until a human ran
/// `apply-event`.
///
/// Every controller is reconciled independently: one unreadable or
/// unwriteable record is recorded against its own entry and never aborts the
/// sweep.
pub fn reconcile_waiting_controllers() -> Result<ControllerWaitReconcileReport> {
    let records = controller::list_controllers()?;
    let mut entries = Vec::new();
    let mut changed = 0usize;
    let mut failed = 0usize;

    for mut record in records {
        if record.open_wait_count() == 0 {
            continue;
        }
        let loop_id = record.loop_id.clone();
        let before_state = record.state;
        // Optimistic-concurrency token. Controller records have no locking, so
        // a background writer on a timer could clobber an operator's `resume`
        // that landed while this pass was reading. `reconcile_open_waits` does
        // not touch `updated_at`, which makes the loaded value a usable
        // version marker for a re-check immediately before the write.
        let observed_at = record.updated_at.clone();
        let outcome = match reconcile_open_waits(&mut record) {
            Ok(outcome) => outcome,
            Err(error) => {
                failed += 1;
                entries.push(ControllerWaitReconcileEntry {
                    loop_id,
                    before_state,
                    after_state: before_state,
                    resolved_waits: Vec::new(),
                    timed_out_waits: Vec::new(),
                    open_waits: 0,
                    error: Some(error.message),
                });
                continue;
            }
        };
        if !outcome.changed() {
            continue;
        }
        // Somebody else advanced this controller while the pass ran. Their
        // write is newer and is based on state this pass never saw; drop this
        // one and let the next tick re-derive from the record they left.
        // Reconciliation is idempotent, so losing a pass costs one interval.
        if controller::load_controller(&loop_id)
            .map(|current| current.updated_at != observed_at)
            .unwrap_or(true)
        {
            continue;
        }
        record.touch();
        if let Err(error) = controller::write_controller(&record) {
            failed += 1;
            entries.push(ControllerWaitReconcileEntry {
                loop_id,
                before_state,
                after_state: before_state,
                resolved_waits: outcome.resolved,
                timed_out_waits: outcome.timed_out,
                open_waits: outcome.open_waits,
                error: Some(error.message),
            });
            continue;
        }
        changed += 1;
        emit_wait_reconcile_notifications(&record, before_state, &outcome);
        entries.push(ControllerWaitReconcileEntry {
            loop_id,
            before_state,
            after_state: record.state,
            resolved_waits: outcome.resolved,
            timed_out_waits: outcome.timed_out,
            open_waits: outcome.open_waits,
            error: None,
        });
    }

    Ok(ControllerWaitReconcileReport {
        schema: WAIT_RECONCILE_RESULT_SCHEMA,
        considered: entries.len(),
        changed,
        failed,
        controllers: entries,
    })
}

/// Announce what the sweep changed.
///
/// Only emitted when the pass actually moved something. A controller that is
/// still waiting on the same events it was waiting on last minute is not news,
/// and a sweep that announced it every tick would be a heartbeat — exactly the
/// noise the cook emitters were kept sparse to avoid.
fn emit_wait_reconcile_notifications(
    record: &AgentTaskLoopControllerRecord,
    before_state: AgentTaskLoopControllerState,
    outcome: &WaitReconcileOutcome,
) {
    let phase = (!record.phase.is_empty()).then_some(record.phase.as_str());
    if record.state != before_state {
        let reason = if outcome.escalated {
            Some("a declared wait deadline passed")
        } else if !outcome.timed_out.is_empty() {
            Some("a declared wait deadline passed")
        } else {
            Some("durable state satisfied every open wait")
        };
        crate::agent_task_notify::controller_state_changed(
            &record.loop_id,
            phase,
            before_state,
            record.state,
            reason,
            outcome.open_waits,
        );
        return;
    }
    if record.state == AgentTaskLoopControllerState::Waiting {
        let waits = record
            .waits
            .iter()
            .filter(|wait| wait.status == controller::AgentTaskLoopWaitStatus::Open)
            .map(|wait| crate::agent_task_notify::ControllerWaitSummary {
                wait_key: wait.wait_key.clone(),
                event_type: wait.event_type.clone(),
                external_ref: wait.external_ref.clone(),
            })
            .collect::<Vec<_>>();
        crate::agent_task_notify::controller_waiting(&record.loop_id, phase, &waits);
    }
}

fn replay_runner_terminal_event(
    record: &mut AgentTaskLoopControllerRecord,
    request: &ControllerApplyEventRequest,
) -> Result<bool> {
    if request.event_type != "agent_task.runner_terminal" {
        return Ok(false);
    }
    let identity = request.payload.get("identity").cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "payload.identity",
            "runner terminal event requires identity",
            None,
            None,
        )
    })?;
    // Match the terminal event to its controller action by canonical
    // run/runner/job identity rather than exact JSON equality — the stored and
    // replayed identity objects can differ in cosmetic fields (extra keys,
    // handoff_id) while naming the same job. Fall back to raw `Value` equality
    // only when a side is not a projectable dispatch-identity object.
    let event_identity =
        homeboy_core::lab_contract::RunnerJobIdentity::from_dispatch_value(&identity);
    let action = record.next_actions.iter().find(|action| {
        let Some(stored) = lab_runner_handoff_identity(action) else {
            return false;
        };
        match (
            event_identity.as_ref(),
            homeboy_core::lab_contract::RunnerJobIdentity::from_dispatch_value(&stored),
        ) {
            (Some(event_identity), Some(stored_identity)) => {
                event_identity.matches(&stored_identity)
            }
            _ => stored == identity,
        }
    });
    let Some(action) = action else {
        return Err(Error::validation_invalid_argument(
            "payload.identity",
            "runner terminal event does not match a controller action handoff identity",
            Some(identity.to_string()),
            None,
        ));
    };
    let action_id = action.action_id.clone();
    let action_status = action.status;
    if matches!(
        action_status,
        AgentTaskLoopActionStatus::Completed
            | AgentTaskLoopActionStatus::Failed
            | AgentTaskLoopActionStatus::Cancelled
    ) {
        return Ok(true);
    }
    if action_status != AgentTaskLoopActionStatus::WaitingForRunner {
        return Err(Error::validation_invalid_argument(
            "payload.identity",
            "runner terminal event matched an action that is not waiting for the runner",
            Some(action_id.clone()),
            None,
        ));
    }
    let run_id = identity["run_id"].as_str().unwrap_or_default();
    let run = lifecycle::status(run_id)?;
    let status = terminal_runner_action_status(run.state).ok_or_else(|| {
        Error::validation_invalid_argument(
            "payload.identity",
            "runner terminal replay requires a terminal persisted run",
            Some(run_id.to_string()),
            None,
        )
    })?;
    project_runner_terminal_action(
        record,
        &action_id,
        &identity,
        status,
        Some(&request.payload),
    )?;
    Ok(true)
}

fn lab_runner_handoff_identity(action: &AgentTaskLoopPolicyActionRecord) -> Option<Value> {
    action
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "runner_handoff")?
        .details
        .get("identity")
        .cloned()
}

fn terminal_runner_action_status(
    state: lifecycle::AgentTaskRunState,
) -> Option<AgentTaskLoopActionStatus> {
    match state {
        lifecycle::AgentTaskRunState::Succeeded => Some(AgentTaskLoopActionStatus::Completed),
        lifecycle::AgentTaskRunState::Cancelled => Some(AgentTaskLoopActionStatus::Cancelled),
        lifecycle::AgentTaskRunState::CandidateRecoverable
        | lifecycle::AgentTaskRunState::PartialRecoverable
        | lifecycle::AgentTaskRunState::PartialFailure
        | lifecycle::AgentTaskRunState::Failed => Some(AgentTaskLoopActionStatus::Failed),
        _ => None,
    }
}

fn project_runner_terminal_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    identity: &Value,
    status: AgentTaskLoopActionStatus,
    replay: Option<&Value>,
) -> Result<()> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| Error::internal_unexpected("runner handoff action disappeared"))?;
    if action.status != AgentTaskLoopActionStatus::WaitingForRunner {
        return Ok(());
    }
    action.status = status;
    push_controller_history(
        record,
        "controller.action.runner_terminal_projected",
        None,
        serde_json::json!({
            "action_id": action_id,
            "identity": identity,
            "status": action_status_report_label(status),
            "replay": replay,
        }),
    );
    Ok(())
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
    if reconcile_waiting_runner_actions(&mut record)? {
        controller::write_controller(&record)?;
    }
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
        let mut record = controller::controller_status(loop_id)?;
        if reconcile_waiting_runner_actions(&mut record)? {
            controller::write_controller(&record)?;
        }
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
