//! Durable agent-task controller execution service.
//!
//! Owns the controller execution policy that used to live in the CLI adapter.
//! Callers (CLI, daemon, future automation) build typed requests, hand them to
//! the service, and serialize the typed reports the service returns. The CLI
//! adapter is responsible only for argument parsing and JSON envelope rendering.
//!
//! Reports keep their existing JSON shapes via `serde` so the CLI continues to
//! emit the same envelopes after the move.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task_lifecycle as lifecycle;
use crate::core::agent_task_loop_controller::{
    self as controller, AgentTaskGateBundle, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleResult, AgentTaskGateBundleStatus, AgentTaskGateCheckResult,
    AgentTaskLoopActionDiagnostic, AgentTaskLoopActionStatus, AgentTaskLoopArtifactRef,
    AgentTaskLoopControllerRecord, AgentTaskLoopControllerState, AgentTaskLoopEntity,
    AgentTaskLoopExternalEvent, AgentTaskLoopHistoryEvent, AgentTaskLoopPolicy,
    AgentTaskLoopPolicyAction, AgentTaskLoopPolicyActionRecord, AgentTaskLoopProvenanceRef,
    AgentTaskLoopRunRef, AgentTaskLoopTaskLineage, AgentTaskLoopTransition,
    AgentTaskPrOwnershipRequest, AgentTaskPrOwnershipState, AgentTaskPrOwnershipStatusUpdate,
};
use crate::core::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};
use crate::core::agent_task_service::{self, AgentTaskRunResult};
use crate::core::git::{pr_find, pr_view, PrFindOptions, PrState};
use crate::core::plan::{HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus};
use crate::core::{Error, Result};
use std::collections::HashMap;
use std::process::Command;

const REPO_LOOP_SPEC_METADATA_KEY: &str = "repo_loop_spec";
const REPO_LOOP_SPEC_WORKFLOW_REASON: &str = "repo loop spec workflow";
const REPO_LOOP_SPEC_ACTION_REASON: &str = "repo loop spec action";

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

/// Request to create a new durable controller record.
#[derive(Debug, Clone)]
pub struct ControllerInitRequest {
    pub loop_id: String,
    pub phase: String,
    pub config_version: String,
}

/// Request to initialize or resume a durable controller from a repo-authored loop spec.
#[derive(Debug, Clone)]
pub struct ControllerFromSpecRequest {
    pub spec: AgentTaskRepoLoopSpec,
}

/// Generic repo-authored loop spec compiled into Homeboy controller state/actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(alias = "controller_id")]
    pub loop_id: String,
    #[serde(default = "default_loop_spec_phase")]
    pub phase: String,
    #[serde(default = "default_loop_spec_config_version")]
    pub config_version: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<AgentTaskRepoLoopSpecEntity>,
    #[serde(
        default,
        deserialize_with = "deserialize_agents",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub agents: Vec<AgentTaskRepoLoopSpecAgent>,
    #[serde(
        default,
        deserialize_with = "deserialize_tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tools: Vec<AgentTaskRepoLoopSpecTool>,
    #[serde(
        default,
        deserialize_with = "deserialize_abilities",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub abilities: Vec<AgentTaskRepoLoopSpecAbility>,
    #[serde(
        default,
        deserialize_with = "deserialize_workflows",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub workflows: Vec<AgentTaskRepoLoopSpecWorkflow>,
    #[serde(
        default,
        deserialize_with = "deserialize_artifacts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub artifacts: Vec<AgentTaskRepoLoopSpecArtifact>,
    #[serde(
        default,
        deserialize_with = "deserialize_dependencies",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dependencies: Vec<AgentTaskRepoLoopSpecDependency>,
    #[serde(
        default,
        deserialize_with = "deserialize_gates",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub gates: Vec<AgentTaskRepoLoopSpecGate>,
    #[serde(
        default,
        deserialize_with = "deserialize_metrics",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub metrics: Vec<AgentTaskRepoLoopSpecMetric>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_bundles: Vec<AgentTaskGateBundle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<AgentTaskLoopPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<AgentTaskRepoLoopSpecPhase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AgentTaskLoopPolicyAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_event: Option<AgentTaskRepoLoopSpecEvent>,
}

/// Entity seed declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecEntity {
    pub entity_type: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_entity_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Agent role declared by a repo loop spec. Homeboy maps this to an executor backend later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecAgent {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

/// Tool contract declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecTool {
    pub tool_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input_schema: Value,
}

/// Ability contract declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecAbility {
    pub ability_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
}

/// Workflow declared by a repo loop spec and compiled by Homeboy into controller actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecWorkflow {
    pub workflow_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emits: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub inputs: Value,
}

/// Artifact contract declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecArtifact {
    pub artifact_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Dependency declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecDependency {
    pub dependency_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Gate declared by a repo loop spec; Homeboy owns gate orchestration and state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecGate {
    pub gate_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
}

/// Metric definition declared by a repo loop spec; Homeboy evaluates it through gate actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecMetric {
    pub metric_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
}

/// Phase shorthand compiled into an [`AgentTaskLoopTransition`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecPhase {
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_json_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AgentTaskLoopPolicyAction>,
}

/// Optional event used to evaluate event-gated policy transitions while compiling a spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecEvent {
    #[serde(default = "default_loop_spec_event_type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

fn deserialize_agents<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecAgent>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "agent_id")
}

fn deserialize_tools<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecTool>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "tool_id")
}

fn deserialize_abilities<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecAbility>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "ability_id")
}

fn deserialize_workflows<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecWorkflow>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "workflow_id")
}

fn deserialize_artifacts<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecArtifact>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "artifact_id")
}

fn deserialize_dependencies<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecDependency>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "dependency_id")
}

fn deserialize_gates<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecGate>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "gate_id")
}

fn deserialize_metrics<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecMetric>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "metric_id")
}

fn deserialize_vec_or_keyed_map<'de, D, T>(
    deserializer: D,
    id_field: &'static str,
) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    let normalized = match value {
        Value::Array(items) => Value::Array(items),
        Value::Object(items) => {
            let mut normalized = Vec::with_capacity(items.len());
            for (key, item) in items {
                let Value::Object(mut item) = item else {
                    return Err(serde::de::Error::custom(format!(
                        "keyed map values must be objects with optional {id_field}"
                    )));
                };
                item.entry(id_field.to_string())
                    .or_insert_with(|| Value::String(key));
                normalized.push(Value::Object(item));
            }
            Value::Array(normalized)
        }
        other => {
            return Err(serde::de::Error::custom(format!(
                "expected array or keyed object map for {id_field} items, got {}",
                json_type_name(&other)
            )));
        }
    };

    serde_json::from_value(normalized).map_err(serde::de::Error::custom)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Request to apply an external event to a controller.
#[derive(Debug, Clone)]
pub struct ControllerApplyEventRequest {
    pub loop_id: String,
    pub event_type: String,
    /// Optional stable event id. Generated from the loop history length when omitted.
    pub event_id: Option<String>,
    pub event_key: Option<String>,
    pub entity_id: Option<String>,
    /// Event payload JSON. May contain a `policy` object to evaluate.
    pub payload: Value,
}

/// Request to mark a tracked entity as human-ready work.
#[derive(Debug, Clone)]
pub struct ControllerMarkHumanReadyRequest {
    pub loop_id: String,
    pub entity_id: String,
    pub reason: Option<String>,
}

/// Typed report returned by `apply_event`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerEventReport {
    pub schema: &'static str,
    pub controller: AgentTaskLoopControllerRecord,
    pub actions: Vec<AgentTaskLoopPolicyActionRecord>,
}

/// Typed report returned by `init_from_spec`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerFromSpecReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub initialized: bool,
    pub actions: Vec<AgentTaskLoopPolicyActionRecord>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Typed report returned by `run_next` and `run_action`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerActionReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<Value>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Typed report returned by `resume`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerResumeReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub claimed: bool,
    pub results: Vec<Value>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Typed list report returned by `list`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerListReport {
    pub schema: &'static str,
    pub controllers: Vec<AgentTaskLoopControllerRecord>,
}

/// Optional dispatch hook used when a `spawn_task` request asks for `"mode": "dispatch"`.
///
/// The CLI adapter implements this to bridge controller-driven dispatch into the
/// existing `agent-task dispatch` command. Callers that do not need dispatch
/// mode can pass [`NoopDispatchHook`].
pub trait ControllerDispatchHook {
    /// Run a dispatch request and return its JSON envelope and process exit code.
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)>;
}

/// Default dispatch hook that refuses dispatch requests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDispatchHook;

impl ControllerDispatchHook for NoopDispatchHook {
    fn dispatch(&self, _request: &Value) -> Result<(Value, i32)> {
        Err(Error::validation_invalid_argument(
            "request.mode",
            "controller dispatch hook is not wired; pass a ControllerDispatchHook to enable mode 'dispatch'",
            None,
            None,
        ))
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

#[derive(Debug, Default)]
struct RepoLoopSpecReconciliation {
    removed_action_count: usize,
    removed_dedupe_key_count: usize,
}

fn repo_loop_spec_fingerprint(spec: &AgentTaskRepoLoopSpec) -> Result<String> {
    let bytes = serde_json::to_vec(spec).map_err(|error| {
        Error::internal_json(
            format!("repo loop spec fingerprint serialization failed: {error}"),
            Some("agent-task controller from-spec".to_string()),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn repo_loop_spec_fingerprint_from_metadata(
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    record
        .metadata
        .get(REPO_LOOP_SPEC_METADATA_KEY)
        .and_then(|value| value.get("fingerprint"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn set_repo_loop_spec_metadata(
    record: &mut AgentTaskLoopControllerRecord,
    spec: &AgentTaskRepoLoopSpec,
    fingerprint: &str,
) {
    let mut metadata = match std::mem::take(&mut record.metadata) {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("repo_loop_metadata".to_string(), other);
            map
        }
    };
    metadata.insert(
        REPO_LOOP_SPEC_METADATA_KEY.to_string(),
        serde_json::json!({
            "schema": spec.schema,
            "fingerprint": fingerprint,
            "config_version": spec.config_version,
        }),
    );
    record.metadata = Value::Object(metadata);
}

fn reconcile_repo_loop_spec_actions(
    record: &mut AgentTaskLoopControllerRecord,
    previous_fingerprint: Option<&str>,
    current_fingerprint: &str,
) -> Result<RepoLoopSpecReconciliation> {
    if previous_fingerprint == Some(current_fingerprint) {
        return Ok(RepoLoopSpecReconciliation::default());
    }

    let managed_action_ids: Vec<String> = record
        .next_actions
        .iter()
        .filter(|action| is_repo_loop_spec_action(action))
        .map(|action| action.action_id.clone())
        .collect();
    if managed_action_ids.is_empty() {
        return Ok(RepoLoopSpecReconciliation::default());
    }

    if let Some(running) = record.next_actions.iter().find(|action| {
        is_repo_loop_spec_action(action) && action.status == AgentTaskLoopActionStatus::Running
    }) {
        return Err(Error::validation_invalid_argument(
            "spec_fingerprint",
            format!(
                "repo loop spec changed for '{}' while repo-spec action '{}' is running; wait for it to finish before reapplying the spec",
                record.loop_id, running.action_id
            ),
            previous_fingerprint.map(ToString::to_string),
            Some(vec![current_fingerprint.to_string()]),
        ));
    }

    let mut dedupe_keys = record
        .next_actions
        .iter()
        .filter(|action| is_repo_loop_spec_action(action))
        .filter_map(|action| action.dedupe_key.clone())
        .collect::<Vec<_>>();
    dedupe_keys.sort();
    dedupe_keys.dedup();

    let removed_action_count = managed_action_ids.len();
    record
        .next_actions
        .retain(|action| !is_repo_loop_spec_action(action));
    let mut removed_dedupe_key_count = 0;
    for dedupe_key in dedupe_keys {
        if record.dedupe_keys.remove(&dedupe_key).is_some() {
            removed_dedupe_key_count += 1;
        }
    }

    Ok(RepoLoopSpecReconciliation {
        removed_action_count,
        removed_dedupe_key_count,
    })
}

fn is_repo_loop_spec_action(action: &AgentTaskLoopPolicyActionRecord) -> bool {
    action.reason == REPO_LOOP_SPEC_WORKFLOW_REASON || action.reason == REPO_LOOP_SPEC_ACTION_REASON
}

fn validate_loop_spec(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    if spec.loop_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "loop_id",
            "repo loop spec requires a non-empty loop_id",
            None,
            None,
        ));
    }
    for (index, phase) in spec.phases.iter().enumerate() {
        if phase.phase.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                format!("phases[{index}].phase"),
                "repo loop spec phase requires a non-empty phase name",
                None,
                None,
            ));
        }
    }
    for (index, workflow) in spec.workflows.iter().enumerate() {
        if workflow.workflow_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                format!("workflows[{index}].workflow_id"),
                "repo loop spec workflow requires a non-empty workflow_id",
                None,
                None,
            ));
        }
        if workflow
            .prompt
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            && workflow.tasks.is_empty()
        {
            return Err(Error::validation_invalid_argument(
                format!("workflows[{index}]"),
                "repo loop spec workflow requires prompt or tasks",
                None,
                None,
            ));
        }
        if let Some(agent_id) = &workflow.agent_id {
            validate_declared_id(
                format!("workflows[{index}].agent_id"),
                agent_id,
                &spec.agents,
                |agent| &agent.agent_id,
            )?;
        }
        validate_declared_ids(
            format!("workflows[{index}].tools"),
            &workflow.tools,
            &spec.tools,
            |tool| &tool.tool_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].abilities"),
            &workflow.abilities,
            &spec.abilities,
            |ability| &ability.ability_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].artifacts"),
            &workflow.artifacts,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].consumes"),
            &workflow.consumes,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].emits"),
            &workflow.emits,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_workflow_dependencies(
            format!("workflows[{index}].dependencies"),
            &workflow.dependencies,
            spec,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].gates"),
            &workflow.gates,
            &spec.gates,
            |gate| &gate.gate_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].metrics"),
            &workflow.metrics,
            &spec.metrics,
            |metric| &metric.metric_id,
        )?;
    }
    for (index, agent) in spec.agents.iter().enumerate() {
        validate_declared_ids(
            format!("agents[{index}].tools"),
            &agent.tools,
            &spec.tools,
            |tool| &tool.tool_id,
        )?;
        validate_declared_ids(
            format!("agents[{index}].abilities"),
            &agent.abilities,
            &spec.abilities,
            |ability| &ability.ability_id,
        )?;
    }
    Ok(())
}

fn validate_declared_ids<T, F>(
    field: String,
    requested: &[String],
    items: &[T],
    id: F,
) -> Result<()>
where
    F: Fn(&T) -> &String + Copy,
{
    for value in requested {
        validate_declared_id(field.clone(), value, items, id)?;
    }
    Ok(())
}

fn validate_declared_id<T, F>(field: String, requested: &str, items: &[T], id: F) -> Result<()>
where
    F: Fn(&T) -> &String,
{
    if items.iter().any(|item| id(item) == requested) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        field,
        "repo loop spec references an undeclared contract id",
        Some(requested.to_string()),
        None,
    ))
}

fn validate_workflow_dependencies(
    field: String,
    requested: &[String],
    spec: &AgentTaskRepoLoopSpec,
) -> Result<()> {
    for value in requested {
        if spec
            .dependencies
            .iter()
            .any(|dependency| dependency.dependency_id == value.as_str())
            || spec
                .artifacts
                .iter()
                .any(|artifact| artifact.artifact_id == value.as_str())
        {
            continue;
        }

        let mut known_ids: Vec<String> = spec
            .dependencies
            .iter()
            .map(|dependency| format!("dependency:{}", dependency.dependency_id))
            .chain(
                spec.artifacts
                    .iter()
                    .map(|artifact| format!("artifact:{}", artifact.artifact_id)),
            )
            .collect();
        known_ids.sort();
        return Err(Error::validation_invalid_argument(
            field,
            "repo loop spec workflow dependency must reference a declared dependency_id or artifact_id",
            Some(value.clone()),
            Some(known_ids),
        ));
    }
    Ok(())
}

fn compile_loop_spec_workflows(
    spec: &AgentTaskRepoLoopSpec,
) -> Result<Vec<AgentTaskLoopPolicyAction>> {
    spec.workflows
        .iter()
        .map(|workflow| compile_loop_spec_workflow(spec, workflow))
        .collect()
}

fn compile_loop_spec_workflow(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<AgentTaskLoopPolicyAction> {
    let request = workflow_dispatch_request(spec, workflow)?;
    let dedupe_key = format!("workflow:{}", workflow.workflow_id);
    if workflow.entity_ids.is_empty() {
        Ok(AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id: None,
            request,
        })
    } else {
        Ok(AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids: workflow.entity_ids.clone(),
            request_template: request,
        })
    }
}

fn workflow_dispatch_request(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    let mut dispatch = serde_json::Map::new();
    if let Some(prompt) = workflow.prompt.as_ref().filter(|prompt| !prompt.is_empty()) {
        dispatch.insert("prompt".to_string(), Value::String(prompt.clone()));
    }
    if !workflow.tasks.is_empty() {
        dispatch.insert(
            "tasks".to_string(),
            Value::Array(workflow.tasks.iter().cloned().map(Value::String).collect()),
        );
    }
    apply_workflow_dispatch_defaults(spec, &mut dispatch);
    let context = workflow_client_context(spec, workflow)?;
    dispatch.insert(
        "client_context".to_string(),
        Value::String(context.to_string()),
    );
    let required_capabilities = workflow_required_capabilities(spec, workflow);
    if !required_capabilities.is_empty() {
        dispatch.insert(
            "required_capabilities".to_string(),
            Value::Array(
                required_capabilities
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Ok(serde_json::json!({
        "mode": "dispatch",
        "dispatch": Value::Object(dispatch),
    }))
}

fn apply_workflow_dispatch_defaults(
    spec: &AgentTaskRepoLoopSpec,
    dispatch: &mut serde_json::Map<String, Value>,
) {
    let Some(defaults) = spec
        .metadata
        .get("dispatch_defaults")
        .and_then(Value::as_object)
    else {
        return;
    };
    for key in ["cwd", "workspace", "repo"] {
        if dispatch.contains_key(key) {
            continue;
        }
        if let Some(value) = defaults
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            dispatch.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
}

fn workflow_client_context(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    Ok(serde_json::json!({
        "schema": "homeboy/repo-loop-workflow-context/v1",
        "loop_id": spec.loop_id,
        "workflow_id": workflow.workflow_id,
        "plan": workflow_homeboy_plan(spec, workflow)?,
        "agent": workflow.agent_id.as_ref().and_then(|agent_id| {
            spec.agents.iter().find(|agent| &agent.agent_id == agent_id)
        }),
        "tools": select_by_id(&spec.tools, &workflow.tools, |tool| &tool.tool_id),
        "abilities": select_by_id(&spec.abilities, &workflow.abilities, |ability| &ability.ability_id),
        "artifacts": select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| &artifact.artifact_id),
        "artifact_dependencies": workflow_artifact_dependencies(spec, workflow),
        "dependencies": select_by_id(&spec.dependencies, &workflow.dependencies, |dependency| &dependency.dependency_id),
        "gates": select_by_id(&spec.gates, &workflow.gates, |gate| &gate.gate_id),
        "metrics": select_by_id(&spec.metrics, &workflow.metrics, |metric| &metric.metric_id),
        "inputs": workflow.inputs,
    }))
}

fn workflow_homeboy_plan(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<HomeboyPlan> {
    let mut plan = HomeboyPlan::for_description(
        PlanKind::AgentTask,
        format!("repo loop workflow {}", workflow.workflow_id),
    );
    plan.id = format!("{}:{}", spec.loop_id, workflow.workflow_id);
    plan.inputs.insert(
        "schema".to_string(),
        Value::String("homeboy/repo-loop-workflow-plan/v1".to_string()),
    );
    plan.inputs
        .insert("loop_id".to_string(), Value::String(spec.loop_id.clone()));
    plan.inputs.insert(
        "workflow_id".to_string(),
        Value::String(workflow.workflow_id.clone()),
    );
    if let Some(agent_id) = &workflow.agent_id {
        plan.inputs
            .insert("agent_id".to_string(), Value::String(agent_id.clone()));
    }
    plan.inputs.insert(
        "declarations".to_string(),
        workflow_declaration_context(spec, workflow)?,
    );
    let required_capabilities = workflow_required_capabilities(spec, workflow);
    if !required_capabilities.is_empty() {
        plan.policy.insert(
            "required_capabilities".to_string(),
            Value::Array(
                required_capabilities
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    plan.steps.push(PlanStep {
        id: format!("dispatch:{}", workflow.workflow_id),
        kind: "agent_task_dispatch".to_string(),
        label: Some(workflow.workflow_id.clone()),
        blocking: true,
        scope: workflow.entity_ids.clone(),
        needs: workflow.dependencies.clone(),
        status: PlanStepStatus::Ready,
        inputs: HashMap::from([(
            "workflow".to_string(),
            workflow_declaration_context(spec, workflow)?,
        )]),
        outputs: HashMap::new(),
        skip_reason: None,
        policy: HashMap::new(),
        missing: Vec::new(),
    });
    for artifact in select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| {
        &artifact.artifact_id
    }) {
        let mut data = HashMap::new();
        data.insert("kind".to_string(), Value::String(artifact.kind.clone()));
        data.insert("required".to_string(), Value::Bool(artifact.required));
        if let Some(description) = &artifact.description {
            data.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        plan.artifacts.push(PlanArtifact {
            id: artifact.artifact_id.clone(),
            path: None,
            artifact_type: Some(artifact.kind.clone()),
            data,
        });
    }
    Ok(plan)
}

fn workflow_declaration_context(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    Ok(serde_json::json!({
        "agent": workflow.agent_id.as_ref().and_then(|agent_id| {
            spec.agents.iter().find(|agent| &agent.agent_id == agent_id)
        }),
        "tools": select_by_id(&spec.tools, &workflow.tools, |tool| &tool.tool_id),
        "abilities": select_by_id(&spec.abilities, &workflow.abilities, |ability| &ability.ability_id),
        "artifacts": select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| &artifact.artifact_id),
        "artifact_dependencies": workflow_artifact_dependencies(spec, workflow),
        "dependencies": select_by_id(&spec.dependencies, &workflow.dependencies, |dependency| &dependency.dependency_id),
        "gates": select_by_id(&spec.gates, &workflow.gates, |gate| &gate.gate_id),
        "metrics": select_by_id(&spec.metrics, &workflow.metrics, |metric| &metric.metric_id),
        "inputs": workflow.inputs,
    }))
}

fn workflow_required_capabilities(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<String> {
    let mut capabilities = Vec::new();
    if let Some(agent_id) = &workflow.agent_id {
        if let Some(agent) = spec.agents.iter().find(|agent| &agent.agent_id == agent_id) {
            for tool_id in &agent.tools {
                push_capability(&mut capabilities, "tool", tool_id);
            }
            for ability_id in &agent.abilities {
                push_capability(&mut capabilities, "ability", ability_id);
            }
        }
    }
    for tool_id in &workflow.tools {
        push_capability(&mut capabilities, "tool", tool_id);
    }
    for ability_id in &workflow.abilities {
        push_capability(&mut capabilities, "ability", ability_id);
    }
    capabilities
}

fn push_capability(capabilities: &mut Vec<String>, kind: &str, id: &str) {
    let capability = format!("{kind}:{id}");
    if !capabilities.contains(&capability) {
        capabilities.push(capability);
    }
}

fn select_by_id<'a, T, F>(items: &'a [T], ids: &[String], id: F) -> Vec<&'a T>
where
    F: Fn(&T) -> &String,
{
    if ids.is_empty() {
        return Vec::new();
    }
    ids.iter()
        .filter_map(|requested| items.iter().find(|item| id(item) == requested))
        .collect()
}

fn workflow_artifact_dependencies(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<Value> {
    let mut ids = workflow.consumes.clone();
    for dependency in &workflow.dependencies {
        if spec
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == dependency.as_str())
            && !ids.contains(dependency)
        {
            ids.push(dependency.clone());
        }
    }

    ids.iter()
        .filter_map(|id| {
            let artifact = spec
                .artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == id.as_str())?;
            let mut value = serde_json::to_value(artifact).ok()?;
            if let Some(object) = value.as_object_mut() {
                let producer_workflow_ids: Vec<Value> = spec
                    .workflows
                    .iter()
                    .filter(|producer| producer.workflow_id != workflow.workflow_id)
                    .filter(|producer| {
                        producer.artifacts.contains(id) || producer.emits.contains(id)
                    })
                    .map(|producer| Value::String(producer.workflow_id.clone()))
                    .collect();
                if !producer_workflow_ids.is_empty() {
                    object.insert(
                        "producer_workflow_ids".to_string(),
                        Value::Array(producer_workflow_ids),
                    );
                }
            }
            Some(value)
        })
        .collect()
}

fn compile_loop_spec_policy(spec: &AgentTaskRepoLoopSpec) -> Option<AgentTaskLoopPolicy> {
    let mut transitions = Vec::new();
    if let Some(policy) = &spec.policy {
        transitions.extend(policy.transitions.clone());
    }
    transitions.extend(spec.phases.iter().enumerate().map(|(index, phase)| {
        AgentTaskLoopTransition {
            transition_id: phase
                .transition_id
                .clone()
                .unwrap_or_else(|| format!("{}-{}", phase.phase, index + 1)),
            from_phase: Some(phase.phase.clone()),
            on_event_type: phase.on_event_type.clone(),
            when_json_path: phase.when_json_path.clone(),
            actions: phase.actions.clone(),
        }
    }));
    if transitions.is_empty() {
        None
    } else {
        Some(AgentTaskLoopPolicy {
            policy_id: spec
                .schema
                .clone()
                .unwrap_or_else(|| "repo-loop-spec".to_string()),
            transitions,
        })
    }
}

fn merge_policy_into_event_payload(payload: Value, policy: AgentTaskLoopPolicy) -> Value {
    let policy = serde_json::to_value(policy).unwrap_or(Value::Null);
    match payload {
        Value::Object(mut object) => {
            object.insert("policy".to_string(), policy);
            Value::Object(object)
        }
        Value::Null => serde_json::json!({ "policy": policy }),
        other => serde_json::json!({ "value": other, "policy": policy }),
    }
}

fn default_loop_spec_phase() -> String {
    "init".to_string()
}

fn default_loop_spec_config_version() -> String {
    "v1".to_string()
}

fn default_loop_spec_event_type() -> String {
    "loop_spec.applied".to_string()
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

fn execute_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let action = claim_controller_action(record, action_id)?;
    controller::write_controller(record)?;

    match execute_claimed_controller_action(record, &action, executor, dispatch) {
        Ok((execution, exit_code)) => {
            let mut exit_code = exit_code;
            let missing_required_artifacts = if exit_code == 0 {
                validate_required_action_artifacts(&action, &execution)
            } else {
                Vec::new()
            };
            if missing_required_artifacts.is_empty() {
                complete_controller_action(record, action_id, &execution, exit_code)?;
            } else {
                exit_code = 1;
                fail_controller_action_with_diagnostics(
                    record,
                    action_id,
                    required_artifact_diagnostics(&missing_required_artifacts),
                    &execution,
                )?;
            }
            controller::write_controller(record)?;
            Ok(AgentTaskRunResult {
                value: ControllerActionReport {
                    schema: ACTION_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    action_id: Some(action_id.to_string()),
                    status: Some(
                        if exit_code == 0 {
                            "completed"
                        } else {
                            "failed"
                        }
                        .to_string(),
                    ),
                    execution: Some(execution),
                    controller: record.clone(),
                },
                exit_code,
            })
        }
        Err(error) => {
            fail_controller_action(record, action_id, &error.to_string())?;
            controller::write_controller(record)?;
            Err(error)
        }
    }
}

fn execute_claimed_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id,
            request,
        } => execute_spawn_task_action(
            record,
            action,
            dedupe_key,
            entity_id.as_deref(),
            request,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids,
            request_template,
        } => execute_fan_out_action(
            record,
            action,
            dedupe_key,
            entity_ids,
            request_template,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::SpawnController {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_controller",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::SpawnSubloop {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_subloop",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::Join { wait_key } => Ok((
            serde_json::json!({ "mode": "join", "wait_key": wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Ok((
            serde_json::json!({ "mode": "wait_for_event", "wait_key": wait.wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForController {
            loop_id,
            entity_id,
            wait_key,
            terminal_states,
        } => execute_wait_for_controller_action(
            record,
            loop_id,
            entity_id.as_deref(),
            wait_key.as_deref(),
            terminal_states,
        ),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => execute_run_gates_action(record, bundle_id, entity_id.as_deref()),
        AgentTaskLoopPolicyAction::OwnPrUntilGreen {
            ownership,
            entity_id,
        } => execute_own_pr_until_green_action(record, ownership, entity_id.as_deref()),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
            record.mark_human_ready(entity_id, reason.clone())?;
            Ok((
                serde_json::json!({ "mode": "mark_human_ready", "entity_id": entity_id }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Complete { reason } => {
            record.state = AgentTaskLoopControllerState::Completed;
            Ok((
                serde_json::json!({ "mode": "complete", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Abandon { reason } => {
            record.state = AgentTaskLoopControllerState::Abandoned;
            Ok((
                serde_json::json!({ "mode": "abandon", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Escalate { reason } => {
            record.state = AgentTaskLoopControllerState::Escalated;
            Ok((
                serde_json::json!({ "mode": "escalate", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::RouteFinding {
            finding,
            dedupe_key,
            entity_id,
            request_template,
        } => execute_route_finding_action(
            record,
            action,
            finding,
            dedupe_key,
            entity_id.as_deref(),
            request_template,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. }
        | AgentTaskLoopPolicyAction::Retry { .. }
        | AgentTaskLoopPolicyAction::RequestChanges { .. } => {
            Err(Error::validation_invalid_argument(
                "action_id",
                format!(
                    "controller action '{}' is not executable by the generic controller runner yet",
                    action.action_id
                ),
                Some(action.action_id.clone()),
                None,
            ))
        }
    }
}

fn execute_spawn_task_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("run_plan");
    match mode {
        "run_plan" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = if lifecycle::run_record_exists(&run_id)? {
                lifecycle::status(&run_id)?
            } else {
                lifecycle::submit_plan(&plan, Some(&run_id))?
            };
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            let run_result = agent_task_service::run_submitted(submitted.run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                    "aggregate": aggregate_value,
                }),
                run_result.exit_code,
            ))
        }
        "submit" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                }),
                0,
            ))
        }
        "run" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::run_submitted(run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                run_result.exit_code,
            ))
        }
        "resume" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::resume(run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                run_result.exit_code,
            ))
        }
        "run_next" => {
            let run_result = agent_task_service::run_next(executor)?;
            let value = match run_result.value {
                Some(aggregate) => serde_json::to_value(&aggregate)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
                None => serde_json::json!({ "claimed": false }),
            };
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                run_result.exit_code,
            ))
        }
        "dispatch" => {
            let (value, exit_code) = dispatch.dispatch(request)?;
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                exit_code,
            ))
        }
        other => Err(Error::validation_invalid_argument(
            "request.mode",
            format!("unsupported spawn_task request mode '{other}'"),
            Some(other.to_string()),
            Some(vec![
                "Supported modes: run_plan, submit, run, resume, run_next, dispatch".to_string(),
            ]),
        )),
    }
}

fn execute_fan_out_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_ids: &[String],
    request_template: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut results = Vec::new();
    let mut exit_code = 0;
    for entity_id in entity_ids {
        let request = materialize_fan_out_request(request_template, entity_id);
        let child_dedupe_key = format!("{dedupe_key}:{entity_id}");
        let child_action = AgentTaskLoopPolicyActionRecord {
            action_id: format!("{}:{entity_id}", action.action_id),
            action: AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: child_dedupe_key.clone(),
                entity_id: Some(entity_id.clone()),
                request: request.clone(),
            },
            status: AgentTaskLoopActionStatus::Running,
            reason: action.reason.clone(),
            created_at: action.created_at.clone(),
            dedupe_key: Some(child_dedupe_key.clone()),
            diagnostics: Vec::new(),
        };
        let (result, child_exit_code) = execute_spawn_task_action(
            record,
            &child_action,
            &child_dedupe_key,
            Some(entity_id),
            &request,
            executor.clone(),
            dispatch,
        )?;
        if child_exit_code != 0 {
            exit_code = child_exit_code;
        }
        results.push(result);
    }
    Ok((
        serde_json::json!({ "mode": "fan_out", "results": results }),
        exit_code,
    ))
}

#[allow(clippy::too_many_arguments)]
fn execute_spawn_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    mode: &str,
    dedupe_key: &str,
    loop_id: &str,
    entity_id: Option<&str>,
    phase: &str,
    config_version: &str,
    request: &Value,
) -> Result<(Value, i32)> {
    let mut created = false;
    let mut child = match controller::load_controller(loop_id) {
        Ok(child) => child,
        Err(_) => {
            created = true;
            let mut child = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
            child.parent_loop_id = Some(record.loop_id.clone());
            child.parent_action_id = Some(action.action_id.clone());
            child.parent_entity_id = entity_id.map(str::to_string);
            child.metadata = request.clone();
            child
        }
    };
    let mut child_changed = false;
    if child.parent_loop_id.is_none() {
        child.parent_loop_id = Some(record.loop_id.clone());
        child_changed = true;
    }
    if child.parent_action_id.is_none() {
        child.parent_action_id = Some(action.action_id.clone());
        child_changed = true;
    }
    if child.parent_entity_id.is_none() {
        child.parent_entity_id = entity_id.map(str::to_string);
        child_changed = child.parent_entity_id.is_some();
    }
    if created || child_changed {
        controller::write_controller(&child)?;
    }
    push_controller_history(
        record,
        "controller.action.spawned_controller",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "loop_id": child.loop_id,
            "mode": mode,
            "created": created,
        }),
    );
    Ok((
        serde_json::json!({
            "mode": mode,
            "loop_id": child.loop_id,
            "phase": child.phase,
            "config_version": child.config_version,
            "created": created,
        }),
        0,
    ))
}

fn execute_wait_for_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    loop_id: &str,
    entity_id: Option<&str>,
    wait_key: Option<&str>,
    terminal_states: &[AgentTaskLoopControllerState],
) -> Result<(Value, i32)> {
    let wait_key = wait_key
        .map(str::to_string)
        .unwrap_or_else(|| format!("controller:{}:terminal", loop_id.replace(['/', ':'], "_")));
    let terminal_states = if terminal_states.is_empty() {
        vec![
            AgentTaskLoopControllerState::Completed,
            AgentTaskLoopControllerState::Failed,
            AgentTaskLoopControllerState::HumanReady,
            AgentTaskLoopControllerState::Abandoned,
            AgentTaskLoopControllerState::Escalated,
        ]
    } else {
        terminal_states.to_vec()
    };
    let child_state = controller::controller_status(loop_id)
        .ok()
        .map(|child| child.state);
    let satisfied = child_state.is_some_and(|state| terminal_states.contains(&state));
    if satisfied {
        for wait in &mut record.waits {
            if wait.wait_key == wait_key && wait.status == controller::AgentTaskLoopWaitStatus::Open
            {
                wait.status = controller::AgentTaskLoopWaitStatus::Satisfied;
                wait.satisfied_by_event_id =
                    child_state.map(|state| format!("controller-terminal:{loop_id}:{state:?}"));
            }
        }
        if record.state == AgentTaskLoopControllerState::Waiting {
            record.state = AgentTaskLoopControllerState::Running;
        }
    } else {
        record.state = AgentTaskLoopControllerState::Waiting;
    }
    Ok((
        serde_json::json!({
            "mode": "wait_for_controller",
            "loop_id": loop_id,
            "entity_id": entity_id,
            "wait_key": wait_key,
            "terminal_states": terminal_states,
            "child_state": child_state,
            "satisfied": satisfied,
        }),
        0,
    ))
}

fn execute_run_gates_action(
    record: &mut AgentTaskLoopControllerRecord,
    bundle_id: &str,
    entity_id: Option<&str>,
) -> Result<(Value, i32)> {
    if let Some(existing) = record
        .gate_results
        .iter()
        .rev()
        .find(|result| result.bundle_id == bundle_id && result.entity_id.as_deref() == entity_id)
        .cloned()
    {
        let exit_code = if existing.status == AgentTaskGateBundleStatus::Failed {
            1
        } else {
            0
        };
        return Ok((
            serde_json::json!({ "mode": "run_gates", "bundle_id": bundle_id, "entity_id": entity_id, "result": existing }),
            exit_code,
        ));
    }

    let bundle = record
        .gate_bundles
        .iter()
        .find(|bundle| bundle.bundle_id == bundle_id)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "bundle_id",
                format!("gate bundle '{bundle_id}' does not exist"),
                Some(bundle_id.to_string()),
                None,
            )
        })?;

    let mut checks = Vec::new();
    for check in &bundle.checks {
        let result = match check.kind {
            AgentTaskGateBundleCheckKind::Command => run_command_gate_check(check)?,
            AgentTaskGateBundleCheckKind::Manual => AgentTaskGateCheckResult {
                check_id: check.check_id.clone(),
                status: AgentTaskGateBundleStatus::Warn,
                retryable: check.retryable,
                classification: Some("manual_gate_requires_external_result".to_string()),
                evidence: Vec::new(),
                details: serde_json::json!({ "input": check.input }),
            },
            AgentTaskGateBundleCheckKind::Api | AgentTaskGateBundleCheckKind::Tool => {
                AgentTaskGateCheckResult {
                    check_id: check.check_id.clone(),
                    status: AgentTaskGateBundleStatus::Failed,
                    retryable: check.retryable,
                    classification: Some("unsupported_generic_gate_kind".to_string()),
                    evidence: Vec::new(),
                    details: serde_json::json!({ "kind": check.kind, "input": check.input }),
                }
            }
        };
        checks.push(result);
    }

    let status = if checks
        .iter()
        .any(|check| check.status == AgentTaskGateBundleStatus::Failed)
    {
        AgentTaskGateBundleStatus::Failed
    } else if checks
        .iter()
        .any(|check| check.status == AgentTaskGateBundleStatus::Warn)
    {
        AgentTaskGateBundleStatus::Warn
    } else {
        AgentTaskGateBundleStatus::Passed
    };
    let result = AgentTaskGateBundleResult {
        result_id: format!("gate-result-{}", record.gate_results.len() + 1),
        bundle_id: bundle_id.to_string(),
        entity_id: entity_id.map(str::to_string),
        run_id: None,
        status,
        checks,
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    record.gate_results.push(result.clone());
    let exit_code = if status == AgentTaskGateBundleStatus::Failed {
        1
    } else {
        0
    };
    Ok((
        serde_json::json!({ "mode": "run_gates", "bundle_id": bundle_id, "entity_id": entity_id, "result": result }),
        exit_code,
    ))
}

fn run_command_gate_check(
    check: &crate::core::agent_task_loop_controller::AgentTaskGateBundleCheck,
) -> Result<AgentTaskGateCheckResult> {
    let command = check
        .input
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "command",
                "command gate check requires input.command",
                Some(check.input.to_string()),
                None,
            )
        })?;
    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .output()
        .map_err(|error| {
            Error::internal_io(
                format!("failed to execute gate command '{command}': {error}"),
                None,
            )
        })?;
    let status = if output.status.success() {
        AgentTaskGateBundleStatus::Passed
    } else {
        AgentTaskGateBundleStatus::Failed
    };
    Ok(AgentTaskGateCheckResult {
        check_id: check.check_id.clone(),
        status,
        retryable: check.retryable,
        classification: None,
        evidence: Vec::new(),
        details: serde_json::json!({
            "command": command,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }),
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_route_finding_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    finding: &controller::AgentTaskLoopFindingPacket,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request_template: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let entity_id = ensure_finding_entity(record, finding, entity_id);
    let request = materialize_route_finding_request(request_template, finding, &entity_id);
    execute_spawn_task_action(
        record,
        action,
        dedupe_key,
        Some(&entity_id),
        &request,
        executor,
        dispatch,
    )
}

fn ensure_finding_entity(
    record: &mut AgentTaskLoopControllerRecord,
    finding: &controller::AgentTaskLoopFindingPacket,
    entity_id: Option<&str>,
) -> String {
    if let Some(entity_id) = entity_id {
        return entity_id.to_string();
    }
    let key = finding
        .reproduction_key
        .as_deref()
        .unwrap_or(&finding.finding_id);
    let entity_id = format!("finding:{}", key.replace(['/', ':', '#', ' '], "_"));
    record
        .entities
        .entry(entity_id.clone())
        .or_insert_with(|| AgentTaskLoopEntity {
            entity_id: entity_id.clone(),
            entity_type: "finding".to_string(),
            key: key.to_string(),
            dedupe_key: format!("entity:finding:{key}"),
            state: Some("routed".to_string()),
            human_ready: false,
            parent_entity_ids: Vec::new(),
            run_refs: Vec::new(),
            artifact_refs: finding.lineage.clone(),
            provenance: finding
                .lineage
                .iter()
                .map(|artifact| AgentTaskLoopProvenanceRef {
                    kind: artifact
                        .kind
                        .clone()
                        .unwrap_or_else(|| "artifact".to_string()),
                    uri: artifact.uri.clone(),
                    caused_by: Some(finding.finding_id.clone()),
                })
                .collect(),
            metadata: serde_json::json!({
                "severity": finding.severity,
                "owner": finding.owner,
                "source_transformer": finding.source_transformer,
            }),
        });
    entity_id
}

fn materialize_route_finding_request(
    template: &Value,
    finding: &controller::AgentTaskLoopFindingPacket,
    entity_id: &str,
) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.insert(
            "finding_id".to_string(),
            Value::String(finding.finding_id.clone()),
        );
        object
            .entry("finding".to_string())
            .or_insert_with(|| serde_json::to_value(finding).unwrap_or(Value::Null));
    }
    request
}

fn execute_own_pr_until_green_action(
    record: &mut AgentTaskLoopControllerRecord,
    ownership: &AgentTaskPrOwnershipRequest,
    entity_id: Option<&str>,
) -> Result<(Value, i32)> {
    let pr_number = match ownership.pr_number {
        Some(number) => Some(number),
        None => find_pr_number(ownership)?,
    };
    let Some(pr_number) = pr_number else {
        let update = AgentTaskPrOwnershipStatusUpdate {
            missing_pr: true,
            ..AgentTaskPrOwnershipStatusUpdate::default()
        };
        let status =
            record.record_pr_ownership_status(ownership, entity_id.map(str::to_string), update);
        return Ok((
            serde_json::json!({
                "mode": "own_pr_until_green",
                "ownership": status,
                "action": "stop",
                "reason": "no open pull request matched the owned branch"
            }),
            1,
        ));
    };

    let view = pr_view(
        ownership.component_id.as_deref(),
        pr_number,
        ownership.path.clone(),
    )?;
    let previous_retry_count = existing_pr_retry_count(record, &ownership.ownership_id);
    let retry_count = if pr_needs_retry(&view.ci_state, view.review_decision.as_deref()) {
        previous_retry_count.saturating_add(1)
    } else {
        previous_retry_count
    };
    let update = AgentTaskPrOwnershipStatusUpdate {
        pr_number: Some(view.number),
        pr_url: Some(format!(
            "https://github.com/{}/{}/pull/{}",
            view.owner, view.repo, view.number
        )),
        head_sha: view.head_sha.clone(),
        ci_state: Some(view.ci_state.clone()),
        ci_summary: Some(view.ci_summary.clone()),
        review_decision: view.review_decision.clone(),
        merge_state: view
            .merge_state
            .clone()
            .or_else(|| Some(view.state.clone())),
        retry_count,
        evidence: vec![AgentTaskLoopArtifactRef {
            uri: format!("github://{}/{}/pull/{}", view.owner, view.repo, view.number),
            kind: Some("github.pull_request".to_string()),
            label: Some("Owned pull request".to_string()),
        }],
        missing_pr: false,
    };
    let status =
        record.record_pr_ownership_status(ownership, entity_id.map(str::to_string), update);
    queue_pr_ownership_follow_up(record, ownership, entity_id, &status);
    Ok((
        serde_json::json!({
            "mode": "own_pr_until_green",
            "ownership": status,
        }),
        0,
    ))
}

fn pr_needs_retry(ci_state: &str, review_decision: Option<&str>) -> bool {
    ci_state == "terminal_failed"
        || ci_state == "stale"
        || review_decision == Some("CHANGES_REQUESTED")
}

fn find_pr_number(ownership: &AgentTaskPrOwnershipRequest) -> Result<Option<u64>> {
    let output = pr_find(
        ownership.component_id.as_deref(),
        PrFindOptions {
            base: Some(ownership.base.clone()),
            head: Some(ownership.head.clone()),
            state: PrState::Open,
            limit: 10,
            path: ownership.path.clone(),
        },
    )?;
    Ok(output.items.into_iter().next().map(|item| item.number))
}

fn existing_pr_retry_count(record: &AgentTaskLoopControllerRecord, ownership_id: &str) -> u32 {
    record
        .pr_ownerships
        .iter()
        .find(|ownership| ownership.ownership_id == ownership_id)
        .map(|ownership| ownership.retry_count)
        .unwrap_or_default()
}

fn queue_pr_ownership_follow_up(
    record: &mut AgentTaskLoopControllerRecord,
    ownership: &AgentTaskPrOwnershipRequest,
    entity_id: Option<&str>,
    status: &crate::core::agent_task_loop_controller::AgentTaskPrOwnershipRecord,
) {
    match status.state {
        AgentTaskPrOwnershipState::ChangesRequested => {
            let feedback_id = format!(
                "pr-ownership:{}:retry-{}",
                ownership.ownership_id,
                status.retry_count + 1
            );
            record.record_action(
                AgentTaskLoopPolicyAction::RequestChanges {
                    target_run_id: ownership.ownership_id.clone(),
                    feedback_id: Some(feedback_id),
                },
                "owned PR has red checks or requested changes",
            );
        }
        AgentTaskPrOwnershipState::RetryLimitReached => {
            if let Some(entity_id) = entity_id {
                record.record_action(
                    AgentTaskLoopPolicyAction::MarkHumanReady {
                        entity_id: entity_id.to_string(),
                        reason: Some(
                            "owned PR reached retry limit with red checks or requested changes"
                                .to_string(),
                        ),
                    },
                    "owned PR retry limit reached",
                );
            }
        }
        AgentTaskPrOwnershipState::WaitingForChecks => {
            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(
                    crate::core::agent_task_loop_controller::AgentTaskLoopWait {
                        wait_key: format!("pr-ownership:{}:checks", ownership.ownership_id),
                        event_type: "github.pr.checks_changed".to_string(),
                        entity_id: entity_id.map(str::to_string),
                        external_ref: status
                            .pr_number
                            .map(|number| format!("{}#{}", ownership.head, number)),
                        timeout_at: None,
                        escalation_policy: Some("reinspect_pr".to_string()),
                        status:
                            crate::core::agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    },
                ),
                "owned PR is waiting for checks",
            );
        }
        AgentTaskPrOwnershipState::GreenReady => {
            if let Some(entity_id) = entity_id {
                record.record_action(
                    AgentTaskLoopPolicyAction::MarkHumanReady {
                        entity_id: entity_id.to_string(),
                        reason: Some(
                            "owned PR checks are green and merge is allowed by policy".to_string(),
                        ),
                    },
                    "owned PR is green",
                );
            }
        }
        AgentTaskPrOwnershipState::WaitingForMerge => {
            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(
                    crate::core::agent_task_loop_controller::AgentTaskLoopWait {
                        wait_key: format!("pr-ownership:{}:merged", ownership.ownership_id),
                        event_type: "github.pr.merged".to_string(),
                        entity_id: entity_id.map(str::to_string),
                        external_ref: status
                            .pr_number
                            .map(|number| format!("{}#{}", ownership.head, number)),
                        timeout_at: None,
                        escalation_policy: Some("wait_for_merge".to_string()),
                        status:
                            crate::core::agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    },
                ),
                "owned PR is green and waiting for merge",
            );
        }
        AgentTaskPrOwnershipState::Merged => {
            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("owned PR merged".to_string()),
                },
                "owned PR lifecycle completed",
            );
        }
        AgentTaskPrOwnershipState::MissingPr
        | AgentTaskPrOwnershipState::Stopped
        | AgentTaskPrOwnershipState::Tracking => {}
    }
}

fn claim_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
) -> Result<AgentTaskLoopPolicyActionRecord> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    if action.status != AgentTaskLoopActionStatus::Pending {
        return Err(Error::validation_invalid_argument(
            "action_id",
            format!(
                "controller action '{}' is {:?}, not pending",
                action.action_id, action.status
            ),
            Some(action.action_id.clone()),
            None,
        ));
    }
    action.status = AgentTaskLoopActionStatus::Running;
    let action = action.clone();
    push_controller_history(
        record,
        "controller.action.claimed",
        None,
        serde_json::json!({ "action_id": action.action_id, "dedupe_key": action.dedupe_key }),
    );
    Ok(action)
}

fn complete_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    execution: &Value,
    exit_code: i32,
) -> Result<()> {
    let status = if exit_code == 0 {
        AgentTaskLoopActionStatus::Completed
    } else {
        AgentTaskLoopActionStatus::Failed
    };
    set_controller_action_status(record, action_id, status)?;
    push_controller_history(
        record,
        if exit_code == 0 {
            "controller.action.completed"
        } else {
            "controller.action.failed"
        },
        None,
        serde_json::json!({ "action_id": action_id, "exit_code": exit_code, "execution": execution }),
    );
    Ok(())
}

fn fail_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    message: &str,
) -> Result<()> {
    set_controller_action_status(record, action_id, AgentTaskLoopActionStatus::Failed)?;
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({ "action_id": action_id, "error": message }),
    );
    Ok(())
}

fn fail_controller_action_with_diagnostics(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    diagnostics: Vec<AgentTaskLoopActionDiagnostic>,
    execution: &Value,
) -> Result<()> {
    let message = diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| "controller action failed".to_string());
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = AgentTaskLoopActionStatus::Failed;
    action.reason = message.clone();
    action.diagnostics.extend(diagnostics.clone());
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({
            "action_id": action_id,
            "error": message,
            "diagnostics": diagnostics,
            "execution": execution,
        }),
    );
    Ok(())
}

fn set_controller_action_status(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    status: AgentTaskLoopActionStatus,
) -> Result<()> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = status;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RequiredWorkflowArtifact {
    artifact_id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

fn validate_required_action_artifacts(
    action: &AgentTaskLoopPolicyActionRecord,
    execution: &Value,
) -> Vec<RequiredWorkflowArtifact> {
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask { request, .. }
        | AgentTaskLoopPolicyAction::RouteFinding {
            request_template: request,
            ..
        } => missing_required_artifacts_for_execution(request, execution, None),
        AgentTaskLoopPolicyAction::FanOut {
            request_template, ..
        } => execution
            .get("results")
            .and_then(Value::as_array)
            .map(|results| {
                results
                    .iter()
                    .enumerate()
                    .flat_map(|(index, result)| {
                        missing_required_artifacts_for_execution(
                            request_template,
                            result,
                            Some(format!("results[{index}]")),
                        )
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                required_workflow_artifacts(request_template)
                    .into_iter()
                    .map(|mut artifact| {
                        artifact.scope = Some("results".to_string());
                        artifact
                    })
                    .collect()
            }),
        _ => Vec::new(),
    }
}

fn missing_required_artifacts_for_execution(
    request: &Value,
    execution: &Value,
    scope: Option<String>,
) -> Vec<RequiredWorkflowArtifact> {
    required_workflow_artifacts(request)
        .into_iter()
        .filter_map(|mut artifact| {
            if execution_contains_artifact(execution, &artifact.artifact_id, &artifact.kind) {
                None
            } else {
                artifact.scope = scope.clone();
                Some(artifact)
            }
        })
        .collect()
}

fn required_workflow_artifacts(request: &Value) -> Vec<RequiredWorkflowArtifact> {
    let mut artifacts = Vec::new();
    collect_required_artifacts_from_plan(request.get("plan").unwrap_or(request), &mut artifacts);
    if let Some(dispatch) = request.get("dispatch") {
        collect_required_artifacts_from_client_context(dispatch, &mut artifacts);
    }
    collect_required_artifacts_from_client_context(request, &mut artifacts);
    artifacts
}

fn collect_required_artifacts_from_client_context(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    let Some(context) = value.get("client_context") else {
        return;
    };
    let context = match context {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    collect_required_artifacts_from_plan(&context["plan"], artifacts);
    collect_required_artifacts_from_declarations(&context["artifacts"], artifacts);
}

fn collect_required_artifacts_from_plan(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    collect_required_artifacts_from_declarations(&value["artifacts"], artifacts);
}

fn collect_required_artifacts_from_declarations(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    let Some(declarations) = value.as_array() else {
        return;
    };
    for declaration in declarations {
        let required = declaration
            .get("required")
            .and_then(Value::as_bool)
            .or_else(|| {
                declaration
                    .get("data")
                    .and_then(|data| data.get("required"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);
        if !required {
            continue;
        }
        let Some(artifact_id) = declaration
            .get("artifact_id")
            .or_else(|| declaration.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(kind) = declaration
            .get("kind")
            .or_else(|| declaration.get("artifact_type"))
            .or_else(|| declaration.get("data").and_then(|data| data.get("kind")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == artifact_id && artifact.kind == kind)
        {
            continue;
        }
        artifacts.push(RequiredWorkflowArtifact {
            artifact_id: artifact_id.to_string(),
            kind: kind.to_string(),
            scope: None,
        });
    }
}

fn execution_contains_artifact(value: &Value, artifact_id: &str, kind: &str) -> bool {
    match value {
        Value::Object(object) => {
            let id_matches = object
                .get("id")
                .or_else(|| object.get("artifact_id"))
                .and_then(Value::as_str)
                == Some(artifact_id);
            let kind_matches = object
                .get("kind")
                .or_else(|| object.get("artifact_type"))
                .and_then(Value::as_str)
                == Some(kind);
            (id_matches && kind_matches)
                || object
                    .values()
                    .any(|value| execution_contains_artifact(value, artifact_id, kind))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| execution_contains_artifact(value, artifact_id, kind)),
        _ => false,
    }
}

fn required_artifact_diagnostics(
    missing: &[RequiredWorkflowArtifact],
) -> Vec<AgentTaskLoopActionDiagnostic> {
    let labels = missing
        .iter()
        .map(|artifact| match &artifact.scope {
            Some(scope) => format!("{}:{} at {scope}", artifact.artifact_id, artifact.kind),
            None => format!("{}:{}", artifact.artifact_id, artifact.kind),
        })
        .collect::<Vec<_>>()
        .join(", ");
    vec![AgentTaskLoopActionDiagnostic {
        code: "required_workflow_artifacts_missing".to_string(),
        message: format!(
            "controller action missing required workflow artifact handoff(s): {labels}"
        ),
        runner: None,
        details: serde_json::json!({ "missing_artifacts": missing }),
    }]
}

fn record_controller_spawn(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    run_id: &str,
    request: &Value,
) -> Result<()> {
    if let Some(dedupe) = record.dedupe_keys.get_mut(dedupe_key) {
        dedupe.run_id = Some(run_id.to_string());
    }
    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            if !entity.run_refs.iter().any(|run| run.run_id == run_id) {
                entity.run_refs.push(AgentTaskLoopRunRef {
                    run_id: run_id.to_string(),
                    task_id: None,
                    role: Some("spawn_task".to_string()),
                });
            }
        }
    }
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == run_id)
    {
        record.task_lineage.push(AgentTaskLoopTaskLineage {
            run_id: run_id.to_string(),
            task_id: None,
            parent_run_id: None,
            parent_task_id: None,
            entity_id: entity_id.map(str::to_string),
            dedupe_key: Some(dedupe_key.to_string()),
            artifact_refs: Vec::new(),
            inputs: request.clone(),
            outputs: Value::Null,
        });
    }
    push_controller_history(
        record,
        "controller.action.spawned_run",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "run_id": run_id,
        }),
    );
    controller::write_controller(record)?;
    Ok(())
}

fn first_pending_action_id(record: &AgentTaskLoopControllerRecord) -> Option<String> {
    if !matches!(record.state, AgentTaskLoopControllerState::Running) {
        return None;
    }
    record
        .next_actions
        .iter()
        .find(|action| action.status == AgentTaskLoopActionStatus::Pending)
        .map(|action| action.action_id.clone())
}

/// Parse an `AgentTaskPlan` out of a controller spawn-task request.
///
/// The request may either wrap the plan under a `"plan"` field or be the plan
/// directly. Exposed for callers that need to validate plans before scheduling.
pub fn plan_from_controller_request(request: &Value) -> Result<AgentTaskPlan> {
    let plan_value = request.get("plan").unwrap_or(request);
    serde_json::from_value(plan_value.clone()).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("controller spawn_task plan".to_string()),
            Some(plan_value.to_string()),
        )
    })
}

fn materialize_fan_out_request(template: &Value, entity_id: &str) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.entry("run_id".to_string()).or_insert_with(|| {
            Value::String(format!(
                "controller-{}",
                entity_id.replace([':', '/', '#'], "_")
            ))
        });
    }
    request
}

fn controller_request_run_id(request: &Value, dedupe_key: &str, action_id: &str) -> String {
    optional_string(request, "run_id").unwrap_or_else(|| {
        format!(
            "controller-{}-{}",
            action_id,
            dedupe_key.replace([':', '/', '#', ' '], "_")
        )
    })
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).ok_or_else(|| {
        Error::validation_invalid_argument(
            key,
            format!("controller action request requires string field '{key}'"),
            None,
            None,
        )
    })
}

/// Extract an optional string field from a controller request JSON value.
pub fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Extract an optional boolean field from a controller request JSON value.
pub fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

/// Extract an optional `u32` field from a controller request JSON value.
pub fn optional_u32(value: &Value, key: &str) -> Result<Option<u32>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a u32"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `usize` field from a controller request JSON value.
pub fn optional_usize(value: &Value, key: &str) -> Result<Option<usize>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a usize"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `Vec<String>` field from a controller request JSON value.
pub fn optional_string_array(value: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = value.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            key,
            format!("controller action request field '{key}' must be an array of strings"),
            Some(value.to_string()),
            None,
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Error::validation_invalid_argument(
                    key,
                    format!("controller action request field '{key}' must contain only strings"),
                    Some(value.to_string()),
                    None,
                )
            })
        })
        .collect()
}

fn push_controller_history(
    record: &mut AgentTaskLoopControllerRecord,
    event_type: &str,
    entity_id: Option<String>,
    payload: Value,
) {
    record.history.push(AgentTaskLoopHistoryEvent {
        event_id: format!("event-{}", record.history.len() + 1),
        event_type: event_type.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entity_id,
        payload,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_loop_controller::{
        AgentTaskGateBundle, AgentTaskGateBundleCheck, AgentTaskGateBundleCheckKind,
        AgentTaskGateBundleStatus, AgentTaskLoopFindingPacket, AgentTaskLoopPolicyAction,
        AgentTaskLoopWait, AgentTaskLoopWaitStatus,
    };
    use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
    use crate::test_support::with_isolated_home;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    #[derive(Clone, Default)]
    struct CapturingDispatchHook {
        observed_requests: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone, Default)]
    struct ArtifactDispatchHook {
        observed_requests: Arc<Mutex<Vec<Value>>>,
    }

    impl ControllerDispatchHook for CapturingDispatchHook {
        fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
            self.observed_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            let entity_id = request
                .get("entity_id")
                .and_then(Value::as_str)
                .unwrap_or("workflow");
            Ok((
                json!({
                    "schema": "homeboy/test-generic-dispatch-result/v1",
                    "run_id": format!("generic-run-{}", entity_id.replace([':', '/', '#', ' '], "_")),
                }),
                0,
            ))
        }
    }

    impl ControllerDispatchHook for ArtifactDispatchHook {
        fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
            self.observed_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            let entity_id = request
                .get("entity_id")
                .and_then(Value::as_str)
                .unwrap_or("workflow");
            Ok((
                json!({
                    "schema": "homeboy/test-generic-dispatch-result/v1",
                    "run_id": format!("generic-run-{}", entity_id.replace([':', '/', '#', ' '], "_")),
                    "artifacts": [{
                        "id": "candidate-patch",
                        "kind": "diff",
                        "metadata": {
                            "payload": {
                                "entity_id": entity_id
                            }
                        }
                    }]
                }),
                0,
            ))
        }
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "controller-service-plan",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "controller-service-task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("fixture".to_string()),
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    fn repo_loop_reconcile_spec(loop_id: &str) -> AgentTaskRepoLoopSpec {
        AgentTaskRepoLoopSpec {
            schema: Some("example/repo-loop/v1".to_string()),
            loop_id: loop_id.to_string(),
            phase: "init".to_string(),
            config_version: "repo-v1".to_string(),
            metadata: Value::Null,
            entities: Vec::new(),
            agents: Vec::new(),
            tools: Vec::new(),
            abilities: vec![
                AgentTaskRepoLoopSpecAbility {
                    ability_id: "github_pull_request_publish".to_string(),
                    description: None,
                    input: Value::Null,
                },
                AgentTaskRepoLoopSpecAbility {
                    ability_id: "static_validation".to_string(),
                    description: None,
                    input: Value::Null,
                },
                AgentTaskRepoLoopSpecAbility {
                    ability_id: "static_publication".to_string(),
                    description: None,
                    input: Value::Null,
                },
            ],
            workflows: vec![
                AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "generation".to_string(),
                    agent_id: None,
                    prompt: Some("Generate a static site candidate.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: Vec::new(),
                    tools: Vec::new(),
                    abilities: vec!["github_pull_request_publish".to_string()],
                    artifacts: vec!["static_site_pull_request".to_string()],
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: Vec::new(),
                    gates: Vec::new(),
                    metrics: Vec::new(),
                    inputs: Value::Null,
                },
                AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "static_validation".to_string(),
                    agent_id: None,
                    prompt: Some("Validate the generated static site.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: Vec::new(),
                    tools: Vec::new(),
                    abilities: vec!["static_validation".to_string()],
                    artifacts: Vec::new(),
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: vec!["static_site_pull_request".to_string()],
                    gates: Vec::new(),
                    metrics: Vec::new(),
                    inputs: Value::Null,
                },
            ],
            artifacts: vec![
                AgentTaskRepoLoopSpecArtifact {
                    artifact_id: "static_site_pull_request".to_string(),
                    kind: "pull_request".to_string(),
                    description: None,
                    required: true,
                },
                AgentTaskRepoLoopSpecArtifact {
                    artifact_id: "static_site_candidate".to_string(),
                    kind: "static_site".to_string(),
                    description: None,
                    required: true,
                },
            ],
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            gate_bundles: Vec::new(),
            policy: None,
            phases: Vec::new(),
            actions: Vec::new(),
            initial_event: None,
        }
    }

    fn reapply_base_then_mutated(
        loop_id: &str,
        mutate: impl FnOnce(&mut AgentTaskRepoLoopSpec),
    ) -> ControllerFromSpecReport {
        let base = repo_loop_reconcile_spec(loop_id);
        init_from_spec(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base spec initialized");
        let reapplied = init_from_spec(ControllerFromSpecRequest { spec: base.clone() })
            .expect("base spec reapplied");
        assert!(reapplied
            .actions
            .iter()
            .any(|action| action.status == AgentTaskLoopActionStatus::AlreadySatisfied));

        let mut changed = base;
        mutate(&mut changed);
        init_from_spec(ControllerFromSpecRequest { spec: changed }).expect("changed spec applied")
    }

    fn workflow_action_context(report: &ControllerFromSpecReport, workflow_id: &str) -> Value {
        let action = report
            .actions
            .iter()
            .find(|action| action.dedupe_key.as_deref() == Some(&format!("workflow:{workflow_id}")))
            .expect("workflow action exists");
        let request = match &action.action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => request,
            AgentTaskLoopPolicyAction::FanOut {
                request_template, ..
            } => request_template,
            other => panic!("expected workflow dispatch action, got {other:?}"),
        };
        serde_json::from_str(
            request["dispatch"]["client_context"]
                .as_str()
                .expect("client_context string"),
        )
        .expect("client_context json")
    }

    fn assert_reconciled_to_pending(report: &ControllerFromSpecReport, expected_actions: usize) {
        assert_eq!(report.actions.len(), expected_actions);
        assert!(report
            .actions
            .iter()
            .all(|action| action.status == AgentTaskLoopActionStatus::Pending));
        assert_eq!(report.controller.next_actions.len(), expected_actions);
        assert!(report
            .controller
            .next_actions
            .iter()
            .all(|action| action.status == AgentTaskLoopActionStatus::Pending));
        let history = report.controller.history.last().expect("history event");
        assert_eq!(history.payload["reconciled_action_count"], json!(4));
        assert_eq!(history.payload["reconciled_dedupe_key_count"], json!(2));
    }

    #[test]
    fn init_and_status_round_trip_controller_record() {
        with_isolated_home(|_| {
            let record = init(ControllerInitRequest {
                loop_id: "loop-service-init".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            assert_eq!(record.loop_id, "loop-service-init");
            assert_eq!(record.phase, "repair");

            let loaded = status("loop-service-init").expect("controller loaded");
            assert_eq!(loaded, record);
        });
    }

    #[test]
    fn list_returns_existing_controllers() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-list-a".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller a initialized");
            init(ControllerInitRequest {
                loop_id: "loop-service-list-b".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller b initialized");

            let report = list().expect("controllers listed");
            assert_eq!(report.schema, LIST_RESULT_SCHEMA);
            assert_eq!(report.controllers.len(), 2);
        });
    }

    #[test]
    fn repo_loop_spec_accepts_controller_id_and_keyed_contract_maps() {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "schema": "homeboy/controller-spec/v1",
            "controller_id": "repo-loop-keyed-spec",
            "agents": {
                "repair-agent": {
                    "role": "repair",
                    "tools": ["repo-inspector"]
                }
            },
            "tools": {
                "repo-inspector": {
                    "description": "inspect repo files"
                }
            },
            "workflows": {
                "repair-findings": {
                    "agent_id": "repair-agent",
                    "prompt": "Repair this finding.",
                    "tools": ["repo-inspector"],
                    "artifacts": ["patch"]
                }
            },
            "artifacts": {
                "patch": {
                    "kind": "diff",
                    "required": true
                }
            }
        }))
        .expect("keyed controller spec deserializes");

        assert_eq!(spec.loop_id, "repo-loop-keyed-spec");
        assert_eq!(spec.agents[0].agent_id, "repair-agent");
        assert_eq!(spec.tools[0].tool_id, "repo-inspector");
        assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
        assert_eq!(spec.artifacts[0].artifact_id, "patch");
    }

    #[test]
    fn repo_loop_spec_preserves_explicit_ids_inside_keyed_contract_maps() {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "loop_id": "repo-loop-explicit-ids",
            "agents": {
                "repair": {
                    "agent_id": "repair-agent"
                }
            },
            "tools": {
                "inspect": {
                    "tool_id": "repo-inspector"
                }
            },
            "workflows": {
                "repair": {
                    "workflow_id": "repair-findings",
                    "prompt": "Repair this finding."
                }
            },
            "artifacts": {
                "patch-output": {
                    "artifact_id": "patch",
                    "kind": "diff"
                }
            }
        }))
        .expect("keyed controller spec deserializes");

        assert_eq!(spec.agents[0].agent_id, "repair-agent");
        assert_eq!(spec.tools[0].tool_id, "repo-inspector");
        assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
        assert_eq!(spec.artifacts[0].artifact_id, "patch");
    }

    #[test]
    fn init_from_spec_compiles_repo_workflows_into_deduped_dispatch_actions() {
        with_isolated_home(|_| {
            let spec = AgentTaskRepoLoopSpec {
                schema: Some("example/repo-loop/v1".to_string()),
                loop_id: "repo-loop-spec".to_string(),
                phase: "init".to_string(),
                config_version: "repo-v1".to_string(),
                metadata: json!({
                    "domain": "example",
                    "dispatch_defaults": {
                        "cwd": "/tmp/repo-loop-spec-checkout",
                        "repo": "repo-loop-spec-checkout"
                    }
                }),
                entities: vec![AgentTaskRepoLoopSpecEntity {
                    entity_type: "finding".to_string(),
                    key: "abc".to_string(),
                    parent_entity_ids: Vec::new(),
                    metadata: json!({ "severity": "high" }),
                }],
                agents: vec![AgentTaskRepoLoopSpecAgent {
                    agent_id: "repair-agent".to_string(),
                    role: Some("repair".to_string()),
                    instructions: Some("repair the routed finding".to_string()),
                    tools: vec!["repo-inspector".to_string()],
                    abilities: vec!["apply_patch".to_string()],
                    metadata: Value::Null,
                }],
                tools: vec![AgentTaskRepoLoopSpecTool {
                    tool_id: "repo-inspector".to_string(),
                    description: Some("inspect repo files".to_string()),
                    input_schema: Value::Null,
                }],
                abilities: vec![AgentTaskRepoLoopSpecAbility {
                    ability_id: "apply_patch".to_string(),
                    description: Some("apply focused patches".to_string()),
                    input: Value::Null,
                }],
                workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "repair-findings".to_string(),
                    agent_id: Some("repair-agent".to_string()),
                    prompt: Some("Repair this finding and report evidence.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: vec!["finding:abc".to_string()],
                    tools: vec!["repo-inspector".to_string()],
                    abilities: vec!["apply_patch".to_string()],
                    artifacts: vec!["patch".to_string()],
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: vec![
                        "source-tree".to_string(),
                        "static_site_pull_request".to_string(),
                    ],
                    gates: vec!["quality".to_string()],
                    metrics: vec!["visual-parity".to_string()],
                    inputs: json!({ "finding_key": "abc" }),
                }],
                artifacts: vec![
                    AgentTaskRepoLoopSpecArtifact {
                        artifact_id: "patch".to_string(),
                        kind: "diff".to_string(),
                        description: Some("candidate patch".to_string()),
                        required: true,
                    },
                    AgentTaskRepoLoopSpecArtifact {
                        artifact_id: "static_site_pull_request".to_string(),
                        kind: "pull_request".to_string(),
                        description: Some("upstream pull request artifact".to_string()),
                        required: true,
                    },
                ],
                dependencies: vec![AgentTaskRepoLoopSpecDependency {
                    dependency_id: "source-tree".to_string(),
                    kind: "repo".to_string(),
                    value: None,
                    required: true,
                }],
                gates: vec![AgentTaskRepoLoopSpecGate {
                    gate_id: "quality".to_string(),
                    description: Some("repo quality gate".to_string()),
                    metrics: vec!["visual-parity".to_string()],
                    input: Value::Null,
                }],
                metrics: vec![AgentTaskRepoLoopSpecMetric {
                    metric_id: "visual-parity".to_string(),
                    description: Some("visual parity threshold".to_string()),
                    target: Some(">=0.98".to_string()),
                    input: Value::Null,
                }],
                gate_bundles: vec![
                    crate::core::agent_task_loop_controller::AgentTaskGateBundle {
                        bundle_id: "quality".to_string(),
                        description: "repo-owned quality gates".to_string(),
                        checks: Vec::new(),
                    },
                ],
                policy: None,
                phases: Vec::new(),
                actions: Vec::new(),
                initial_event: None,
            };

            let report = init_from_spec(ControllerFromSpecRequest { spec: spec.clone() })
                .expect("spec initialized");

            assert_eq!(report.schema, FROM_SPEC_RESULT_SCHEMA);
            assert!(report.initialized);
            assert_eq!(report.controller.config_version, "repo-v1");
            assert!(report.controller.entities.contains_key("finding:abc"));
            assert_eq!(report.controller.gate_bundles[0].bundle_id, "quality");
            assert_eq!(report.actions.len(), 1);
            assert_eq!(report.actions[0].status, AgentTaskLoopActionStatus::Pending);
            match &report.actions[0].action {
                AgentTaskLoopPolicyAction::FanOut {
                    dedupe_key,
                    request_template,
                    ..
                } => {
                    assert_eq!(dedupe_key, "workflow:repair-findings");
                    assert_eq!(request_template["mode"], "dispatch");
                    assert_eq!(
                        request_template["dispatch"]["cwd"],
                        "/tmp/repo-loop-spec-checkout"
                    );
                    assert_eq!(
                        request_template["dispatch"]["repo"],
                        "repo-loop-spec-checkout"
                    );
                    assert!(request_template["dispatch"].get("backend").is_none());
                    assert!(request_template["dispatch"]
                        .get("provider_config")
                        .is_none());
                    assert_eq!(
                        request_template["dispatch"]["required_capabilities"],
                        json!(["tool:repo-inspector", "ability:apply_patch"])
                    );
                    let context: Value = serde_json::from_str(
                        request_template["dispatch"]["client_context"]
                            .as_str()
                            .expect("client context string"),
                    )
                    .expect("client context json");
                    assert_eq!(context["agent"]["agent_id"], "repair-agent");
                    assert_eq!(
                        context["plan"]["inputs"]["schema"],
                        "homeboy/repo-loop-workflow-plan/v1"
                    );
                    assert_eq!(
                        context["plan"]["policy"]["required_capabilities"],
                        json!(["tool:repo-inspector", "ability:apply_patch"])
                    );
                    assert_eq!(context["plan"]["steps"][0]["kind"], "agent_task_dispatch");
                    assert_eq!(
                        context["plan"]["steps"][0]["needs"],
                        json!(["source-tree", "static_site_pull_request"])
                    );
                    assert_eq!(context["plan"]["artifacts"][0]["id"], "patch");
                    assert_eq!(
                        context["artifact_dependencies"][0]["artifact_id"],
                        "static_site_pull_request"
                    );
                    assert_eq!(context["gates"][0]["gate_id"], "quality");
                    assert_eq!(context["metrics"][0]["metric_id"], "visual-parity");
                }
                other => panic!("expected fan_out workflow action, got {other:?}"),
            }

            let resumed =
                init_from_spec(ControllerFromSpecRequest { spec }).expect("spec reapplied");

            assert!(!resumed.initialized);
            assert_eq!(
                resumed.actions[0].status,
                AgentTaskLoopActionStatus::AlreadySatisfied
            );
        });
    }

    #[test]
    fn init_from_spec_reconciles_changed_workflow_dependencies() {
        with_isolated_home(|_| {
            let report = reapply_base_then_mutated("repo-loop-reconcile-dependencies", |spec| {
                spec.workflows
                    .iter_mut()
                    .find(|workflow| workflow.workflow_id == "static_validation")
                    .expect("static validation workflow")
                    .dependencies = vec!["static_site_candidate".to_string()];
            });

            assert_reconciled_to_pending(&report, 2);
            let context = workflow_action_context(&report, "static_validation");
            assert_eq!(
                context["plan"]["steps"][0]["needs"],
                json!(["static_site_candidate"])
            );
            assert_eq!(
                context["artifact_dependencies"][0]["artifact_id"],
                "static_site_candidate"
            );
        });
    }

    #[test]
    fn init_from_spec_maps_workflow_consumes_to_artifact_dependencies() {
        with_isolated_home(|_| {
            let mut spec = repo_loop_reconcile_spec("repo-loop-consumes-artifacts");
            spec.workflows
                .iter_mut()
                .find(|workflow| workflow.workflow_id == "generation")
                .expect("generation workflow")
                .emits = vec!["static_site_pull_request".to_string()];
            spec.workflows
                .iter_mut()
                .find(|workflow| workflow.workflow_id == "static_validation")
                .expect("static validation workflow")
                .consumes = vec!["static_site_pull_request".to_string()];

            let report =
                init_from_spec(ControllerFromSpecRequest { spec }).expect("spec initialized");
            let context = workflow_action_context(&report, "static_validation");

            assert_eq!(
                context["artifact_dependencies"],
                json!([{
                    "artifact_id": "static_site_pull_request",
                    "kind": "pull_request",
                    "required": true,
                    "producer_workflow_ids": ["generation"]
                }])
            );
        });
    }

    #[test]
    fn init_from_spec_reconciles_changed_emitted_artifacts() {
        with_isolated_home(|_| {
            let report = reapply_base_then_mutated("repo-loop-reconcile-artifacts", |spec| {
                spec.workflows
                    .iter_mut()
                    .find(|workflow| workflow.workflow_id == "generation")
                    .expect("generation workflow")
                    .artifacts = vec!["static_site_candidate".to_string()];
            });

            assert_reconciled_to_pending(&report, 2);
            let context = workflow_action_context(&report, "generation");
            assert_eq!(
                context["plan"]["artifacts"][0]["id"],
                "static_site_candidate"
            );
            assert_eq!(
                context["artifacts"][0]["artifact_id"],
                "static_site_candidate"
            );
        });
    }

    #[test]
    fn init_from_spec_reconciles_removed_and_added_workflows() {
        with_isolated_home(|_| {
            let report = reapply_base_then_mutated("repo-loop-reconcile-workflows", |spec| {
                spec.workflows
                    .retain(|workflow| workflow.workflow_id != "static_validation");
                spec.workflows.push(AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "static_publication".to_string(),
                    agent_id: None,
                    prompt: Some("Publish the validated static site.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: Vec::new(),
                    tools: Vec::new(),
                    abilities: vec!["static_publication".to_string()],
                    artifacts: vec!["static_site_pull_request".to_string()],
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: vec!["static_site_candidate".to_string()],
                    gates: Vec::new(),
                    metrics: Vec::new(),
                    inputs: Value::Null,
                });
            });

            assert_reconciled_to_pending(&report, 2);
            let dedupe_keys = report
                .controller
                .next_actions
                .iter()
                .filter_map(|action| action.dedupe_key.as_deref())
                .collect::<Vec<_>>();
            assert!(dedupe_keys.contains(&"workflow:generation"));
            assert!(dedupe_keys.contains(&"workflow:static_publication"));
            assert!(!dedupe_keys.contains(&"workflow:static_validation"));
        });
    }

    #[test]
    fn init_from_spec_reconciles_changed_required_capabilities() {
        with_isolated_home(|_| {
            let report = reapply_base_then_mutated("repo-loop-reconcile-capabilities", |spec| {
                spec.workflows
                    .iter_mut()
                    .find(|workflow| workflow.workflow_id == "generation")
                    .expect("generation workflow")
                    .abilities = vec!["static_publication".to_string()];
            });

            assert_reconciled_to_pending(&report, 2);
            let generation = report
                .actions
                .iter()
                .find(|action| action.dedupe_key.as_deref() == Some("workflow:generation"))
                .expect("generation action");
            let request = match &generation.action {
                AgentTaskLoopPolicyAction::SpawnTask { request, .. } => request,
                AgentTaskLoopPolicyAction::FanOut {
                    request_template, ..
                } => request_template,
                other => panic!("expected workflow dispatch action, got {other:?}"),
            };
            assert_eq!(
                request["dispatch"]["required_capabilities"],
                json!(["ability:static_publication"])
            );
            let context = workflow_action_context(&report, "generation");
            assert_eq!(
                context["plan"]["policy"]["required_capabilities"],
                json!(["ability:static_publication"])
            );
        });
    }

    #[test]
    fn init_from_spec_rejects_undeclared_workflow_requirements() {
        with_isolated_home(|_| {
            let spec = AgentTaskRepoLoopSpec {
                schema: None,
                loop_id: "repo-loop-invalid-reference".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
                metadata: Value::Null,
                entities: Vec::new(),
                agents: Vec::new(),
                tools: Vec::new(),
                abilities: Vec::new(),
                workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "repair".to_string(),
                    agent_id: None,
                    prompt: Some("Repair with a declared tool.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: Vec::new(),
                    tools: vec!["missing-tool".to_string()],
                    abilities: Vec::new(),
                    artifacts: Vec::new(),
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: Vec::new(),
                    gates: Vec::new(),
                    metrics: Vec::new(),
                    inputs: Value::Null,
                }],
                artifacts: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                gate_bundles: Vec::new(),
                policy: None,
                phases: Vec::new(),
                actions: Vec::new(),
                initial_event: None,
            };

            let error = init_from_spec(ControllerFromSpecRequest { spec })
                .expect_err("missing requirement declaration should fail");

            assert_eq!(error.details["field"], "workflows[0].tools");
            assert!(error
                .message
                .contains("references an undeclared contract id"));
        });
    }

    #[test]
    fn init_from_spec_applies_event_gated_policy() {
        with_isolated_home(|_| {
            let spec = AgentTaskRepoLoopSpec {
                schema: None,
                loop_id: "repo-loop-event".to_string(),
                phase: "collect".to_string(),
                config_version: "v1".to_string(),
                metadata: Value::Null,
                entities: Vec::new(),
                agents: Vec::new(),
                tools: Vec::new(),
                abilities: Vec::new(),
                workflows: Vec::new(),
                artifacts: Vec::new(),
                dependencies: Vec::new(),
                gates: Vec::new(),
                metrics: Vec::new(),
                gate_bundles: Vec::new(),
                policy: None,
                phases: vec![AgentTaskRepoLoopSpecPhase {
                    phase: "collect".to_string(),
                    transition_id: None,
                    on_event_type: Some("artifact.ready".to_string()),
                    when_json_path: None,
                    actions: vec![AgentTaskLoopPolicyAction::RunGates {
                        bundle_id: "quality".to_string(),
                        entity_id: None,
                    }],
                }],
                actions: Vec::new(),
                initial_event: Some(AgentTaskRepoLoopSpecEvent {
                    event_type: "artifact.ready".to_string(),
                    event_id: Some("artifact-ready-1".to_string()),
                    event_key: None,
                    entity_id: None,
                    payload: Value::Null,
                }),
            };

            let report = init_from_spec(ControllerFromSpecRequest { spec })
                .expect("event-gated spec applied");

            assert_eq!(report.actions.len(), 1);
            assert!(report
                .controller
                .history
                .iter()
                .any(|event| event.event_id == "artifact-ready-1"));
        });
    }

    #[test]
    fn run_next_returns_unclaimed_when_no_pending_actions() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-noop".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let result = run_next(
                "loop-service-noop",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("controller polled");

            assert_eq!(result.exit_code, 0);
            assert!(!result.value.claimed);
            assert_eq!(result.value.schema, ACTION_RESULT_SCHEMA);
            assert!(result.value.action_id.is_none());
            assert!(result.value.execution.is_none());
        });
    }

    #[test]
    fn run_next_executes_spawn_task_action_and_records_lineage() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-spawn".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let plan = test_plan();
            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "finding:abc:repair".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "run_plan",
                        "run_id": "controller-service-spawn-a",
                        "plan": plan,
                    }),
                },
                "finding emitted",
            );
            controller::write_controller(&record).expect("controller written");

            let executor = CapturingExecutor::default();
            let result = run_next("loop-service-spawn", executor.clone(), &NoopDispatchHook)
                .expect("controller action executed");

            assert_eq!(result.exit_code, 0);
            assert!(result.value.claimed);
            assert_eq!(result.value.status.as_deref(), Some("completed"));

            let loaded = controller::load_controller("loop-service-spawn").expect("controller");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Completed
            );
            assert_eq!(
                loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
                Some("controller-service-spawn-a")
            );
            assert_eq!(loaded.task_lineage[0].run_id, "controller-service-spawn-a");
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.claimed"));
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.completed"));

            let observed = executor
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("provider saw request");
            assert_eq!(observed.task_id, "controller-service-task");
        });
    }

    #[test]
    fn resume_fails_required_workflow_artifact_handoff_before_downstream_action() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-required-handoff".to_string(),
                phase: "collect".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let workflow_context = json!({
                "schema": "homeboy/repo-loop-workflow-context/v1",
                "workflow_id": "finding-packets",
                "plan": {
                    "schema": "homeboy/repo-loop-workflow-plan/v1",
                    "artifacts": [{
                        "id": "finding_groups",
                        "artifact_type": "finding-packets",
                        "data": {
                            "kind": "finding-packets",
                            "required": true
                        }
                    }]
                }
            });
            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "workflow:finding-packets".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "run_id": "controller-service-required-handoff-a",
                        "dispatch": {
                            "client_context": workflow_context.to_string()
                        }
                    }),
                },
                "produce finding packets",
            );
            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "workflow:iterator".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "run_id": "controller-service-required-handoff-b",
                    }),
                },
                "consume finding packets",
            );
            controller::write_controller(&record).expect("controller written");

            let result = resume(
                "loop-service-required-handoff",
                CapturingExecutor::default(),
                &CapturingDispatchHook::default(),
            )
            .expect("controller resumed");

            assert_eq!(result.exit_code, 1);
            assert_eq!(result.value.results.len(), 1);
            let loaded = controller::load_controller("loop-service-required-handoff")
                .expect("controller loaded");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Failed
            );
            assert_eq!(
                loaded.next_actions[1].status,
                AgentTaskLoopActionStatus::Pending
            );
            assert_eq!(
                loaded.next_actions[0].diagnostics[0].code,
                "required_workflow_artifacts_missing"
            );
            assert_eq!(
                loaded.next_actions[0].diagnostics[0].details["missing_artifacts"][0]
                    ["artifact_id"],
                json!("finding_groups")
            );
            assert_eq!(
                loaded.next_actions[0].diagnostics[0].details["missing_artifacts"][0]["kind"],
                json!("finding-packets")
            );
            assert!(loaded.history.iter().any(|event| {
                event.event_type == "controller.action.failed"
                    && event.payload["diagnostics"][0]["code"]
                        == json!("required_workflow_artifacts_missing")
            }));
        });
    }

    #[test]
    fn resume_recovers_running_action_with_stale_child_run() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-stale-child".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let plan = test_plan();
            crate::core::agent_task_lifecycle::submit_plan(
                &plan,
                Some("controller-service-stale-child-a"),
            )
            .expect("child submitted");
            crate::core::agent_task_lifecycle::mark_running("controller-service-stale-child-a")
                .expect("child marked running");
            crate::core::agent_task_lifecycle::rewrite_record_for_test(
                "controller-service-stale-child-a",
                |record| {
                    record.metadata["runner_pid"] = json!(999999u32);
                },
            )
            .expect("stale child status written");

            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "finding:abc:repair".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "run_plan",
                        "run_id": "controller-service-stale-child-a",
                        "plan": plan,
                    }),
                },
                "finding emitted",
            );
            record.next_actions[0].status = AgentTaskLoopActionStatus::Running;
            record
                .dedupe_keys
                .get_mut("finding:abc:repair")
                .expect("dedupe record")
                .run_id = Some("controller-service-stale-child-a".to_string());
            controller::write_controller(&record).expect("controller written");

            let result = resume(
                "loop-service-stale-child",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("controller resumed");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.results.len(), 1);
            let loaded =
                controller::load_controller("loop-service-stale-child").expect("controller");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Completed
            );
            assert_eq!(
                loaded.next_actions[0].diagnostics[0].code,
                "stale_child_run_recovery"
            );
            assert!(loaded.history.iter().any(|event| {
                event.event_type == "controller.action.stale_child_recovery"
                    && event.payload["run_id"] == json!("controller-service-stale-child-a")
            }));
            let child =
                crate::core::agent_task_lifecycle::status("controller-service-stale-child-a")
                    .expect("child status");
            assert_eq!(child.metadata["reclaimed_stale_running"], json!(true));
        });
    }

    #[test]
    fn run_action_executes_only_requested_action_id() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-action".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
                    wait_key: "wait-a".to_string(),
                    event_type: "task.completed".to_string(),
                    entity_id: None,
                    external_ref: None,
                    timeout_at: None,
                    escalation_policy: None,
                    status: AgentTaskLoopWaitStatus::Open,
                    satisfied_by_event_id: None,
                }),
                "wait first",
            );
            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("done".to_string()),
                },
                "complete second",
            );
            controller::write_controller(&record).expect("controller written");

            let result = run_action(
                "loop-service-action",
                "action-2",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("action executed");

            assert_eq!(result.exit_code, 0);
            assert!(result.value.claimed);
            let loaded = controller::load_controller("loop-service-action").expect("controller");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Pending
            );
            assert_eq!(
                loaded.next_actions[1].status,
                AgentTaskLoopActionStatus::Completed
            );
        });
    }

    #[test]
    fn complete_action_transitions_only_when_executed() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-complete".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("done".to_string()),
                },
                "complete",
            );
            assert_eq!(record.state, AgentTaskLoopControllerState::Running);
            controller::write_controller(&record).expect("controller written");

            let result = run_next(
                "loop-service-complete",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("complete action executed");

            assert_eq!(result.exit_code, 0);
            assert_eq!(
                result.value.controller.state,
                AgentTaskLoopControllerState::Completed
            );
            assert_eq!(result.value.status.as_deref(), Some("completed"));
        });
    }

    #[test]
    fn resume_stops_when_wait_action_blocks_controller() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-wait-stop".to_string(),
                phase: "delegate".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");
            record.record_action(
                AgentTaskLoopPolicyAction::WaitForController {
                    loop_id: "missing-child".to_string(),
                    entity_id: None,
                    wait_key: None,
                    terminal_states: Vec::new(),
                },
                "wait for child",
            );
            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("must not run yet".to_string()),
                },
                "complete after wait",
            );
            record.state = AgentTaskLoopControllerState::Running;
            controller::write_controller(&record).expect("controller written");

            let result = resume(
                "loop-service-wait-stop",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("controller resumed");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.results.len(), 1);
            assert_eq!(
                result.value.controller.state,
                AgentTaskLoopControllerState::Waiting
            );
            assert_eq!(
                result.value.controller.next_actions[1].status,
                AgentTaskLoopActionStatus::Pending
            );
        });
    }

    #[test]
    fn wait_for_controller_resumes_after_child_terminal_state() {
        with_isolated_home(|_| {
            let mut parent = init(ControllerInitRequest {
                loop_id: "loop-service-parent".to_string(),
                phase: "delegate".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("parent initialized");
            parent.record_action(
                AgentTaskLoopPolicyAction::WaitForController {
                    loop_id: "loop-service-child".to_string(),
                    entity_id: None,
                    wait_key: None,
                    terminal_states: Vec::new(),
                },
                "wait for child",
            );
            parent.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("child done".to_string()),
                },
                "complete after child",
            );
            parent.state = AgentTaskLoopControllerState::Running;
            controller::write_controller(&parent).expect("parent written");

            let mut child = init(ControllerInitRequest {
                loop_id: "loop-service-child".to_string(),
                phase: "work".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("child initialized");
            child.state = AgentTaskLoopControllerState::Completed;
            controller::write_controller(&child).expect("child written");

            let result = resume(
                "loop-service-parent",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("controller resumed");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.results.len(), 2);
            assert_eq!(
                result.value.controller.state,
                AgentTaskLoopControllerState::Completed
            );
        });
    }

    #[test]
    fn run_gates_executes_command_bundle_and_records_result() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-gates".to_string(),
                phase: "validate".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");
            record.gate_bundles.push(AgentTaskGateBundle {
                bundle_id: "quality".to_string(),
                description: "quality gates".to_string(),
                checks: vec![AgentTaskGateBundleCheck {
                    check_id: "true-command".to_string(),
                    kind: AgentTaskGateBundleCheckKind::Command,
                    input: json!({ "command": "true" }),
                    retryable: false,
                }],
            });
            record.record_action(
                AgentTaskLoopPolicyAction::RunGates {
                    bundle_id: "quality".to_string(),
                    entity_id: None,
                },
                "run quality gates",
            );
            controller::write_controller(&record).expect("controller written");

            let result = run_next(
                "loop-service-gates",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("gates executed");

            assert_eq!(result.exit_code, 0);
            let loaded = controller::load_controller("loop-service-gates").expect("controller");
            assert_eq!(loaded.gate_results.len(), 1);
            assert_eq!(
                loaded.gate_results[0].status,
                AgentTaskGateBundleStatus::Passed
            );
        });
    }

    #[test]
    fn from_spec_resume_drives_generic_workflow_gates_completion_and_lineage() {
        with_isolated_home(|_| {
            let spec = AgentTaskRepoLoopSpec {
                schema: Some("example/repo-loop/v1".to_string()),
                loop_id: "repo-loop-generic-execution".to_string(),
                phase: "repair".to_string(),
                config_version: "repo-v1".to_string(),
                metadata: json!({ "domain": "example" }),
                entities: vec![
                    AgentTaskRepoLoopSpecEntity {
                        entity_type: "finding".to_string(),
                        key: "alpha".to_string(),
                        parent_entity_ids: Vec::new(),
                        metadata: json!({ "severity": "high" }),
                    },
                    AgentTaskRepoLoopSpecEntity {
                        entity_type: "finding".to_string(),
                        key: "beta".to_string(),
                        parent_entity_ids: Vec::new(),
                        metadata: json!({ "severity": "medium" }),
                    },
                ],
                agents: vec![AgentTaskRepoLoopSpecAgent {
                    agent_id: "repair-agent".to_string(),
                    role: Some("repair".to_string()),
                    instructions: Some("repair findings and return artifacts".to_string()),
                    tools: vec!["repo-inspector".to_string()],
                    abilities: vec!["patch-writer".to_string()],
                    metadata: Value::Null,
                }],
                tools: vec![AgentTaskRepoLoopSpecTool {
                    tool_id: "repo-inspector".to_string(),
                    description: Some("inspect repository state".to_string()),
                    input_schema: Value::Null,
                }],
                abilities: vec![AgentTaskRepoLoopSpecAbility {
                    ability_id: "patch-writer".to_string(),
                    description: Some("write candidate patches".to_string()),
                    input: Value::Null,
                }],
                workflows: vec![AgentTaskRepoLoopSpecWorkflow {
                    workflow_id: "repair-findings".to_string(),
                    agent_id: Some("repair-agent".to_string()),
                    prompt: Some("Repair each routed finding and report evidence.".to_string()),
                    tasks: Vec::new(),
                    entity_ids: vec!["finding:alpha".to_string(), "finding:beta".to_string()],
                    tools: vec!["repo-inspector".to_string()],
                    abilities: vec!["patch-writer".to_string()],
                    artifacts: vec!["candidate-patch".to_string()],
                    consumes: Vec::new(),
                    emits: Vec::new(),
                    dependencies: vec!["source-tree".to_string()],
                    gates: vec!["quality".to_string()],
                    metrics: vec!["coverage".to_string()],
                    inputs: json!({ "scope": "changed findings" }),
                }],
                artifacts: vec![AgentTaskRepoLoopSpecArtifact {
                    artifact_id: "candidate-patch".to_string(),
                    kind: "diff".to_string(),
                    description: Some("candidate patch".to_string()),
                    required: true,
                }],
                dependencies: vec![AgentTaskRepoLoopSpecDependency {
                    dependency_id: "source-tree".to_string(),
                    kind: "repo".to_string(),
                    value: None,
                    required: true,
                }],
                gates: vec![AgentTaskRepoLoopSpecGate {
                    gate_id: "quality".to_string(),
                    description: Some("repo quality gate".to_string()),
                    metrics: vec!["coverage".to_string()],
                    input: Value::Null,
                }],
                metrics: vec![AgentTaskRepoLoopSpecMetric {
                    metric_id: "coverage".to_string(),
                    description: Some("coverage should not regress".to_string()),
                    target: Some("maintained".to_string()),
                    input: Value::Null,
                }],
                gate_bundles: vec![AgentTaskGateBundle {
                    bundle_id: "quality".to_string(),
                    description: "repo quality gate bundle".to_string(),
                    checks: vec![AgentTaskGateBundleCheck {
                        check_id: "external-quality-signal".to_string(),
                        kind: AgentTaskGateBundleCheckKind::Manual,
                        input: json!({ "metric": "coverage" }),
                        retryable: false,
                    }],
                }],
                policy: None,
                phases: Vec::new(),
                actions: vec![
                    AgentTaskLoopPolicyAction::RunGates {
                        bundle_id: "quality".to_string(),
                        entity_id: Some("finding:alpha".to_string()),
                    },
                    AgentTaskLoopPolicyAction::Complete {
                        reason: Some("repo loop contract executed".to_string()),
                    },
                ],
                initial_event: None,
            };

            let initialized = init_from_spec(ControllerFromSpecRequest { spec })
                .expect("repo loop spec initialized");
            assert!(initialized.initialized);
            assert_eq!(initialized.actions.len(), 3);
            assert_eq!(
                initialized.actions[0].status,
                AgentTaskLoopActionStatus::Pending
            );
            match &initialized.actions[0].action {
                AgentTaskLoopPolicyAction::FanOut {
                    request_template, ..
                } => {
                    assert_eq!(request_template["mode"], "dispatch");
                    let dispatch = request_template["dispatch"]
                        .as_object()
                        .expect("compiled dispatch request");
                    assert!(dispatch.get("backend").is_none());
                    assert!(dispatch.get("provider_config").is_none());
                    assert!(dispatch.get("executor").is_none());
                }
                other => panic!("expected compiled workflow fan-out, got {other:?}"),
            }

            let dispatch = ArtifactDispatchHook::default();
            let result = resume(
                "repo-loop-generic-execution",
                CapturingExecutor::default(),
                &dispatch,
            )
            .expect("controller resumed");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.results.len(), 3);
            assert_eq!(
                result.value.controller.state,
                AgentTaskLoopControllerState::Completed
            );
            assert!(result
                .value
                .controller
                .next_actions
                .iter()
                .all(|action| action.status == AgentTaskLoopActionStatus::Completed));
            assert_eq!(result.value.controller.gate_results.len(), 1);
            assert_eq!(
                result.value.controller.gate_results[0].status,
                AgentTaskGateBundleStatus::Warn
            );
            assert_eq!(result.value.controller.task_lineage.len(), 2);
            assert!(result.value.controller.task_lineage.iter().any(|lineage| {
                lineage.run_id == "generic-run-finding_alpha"
                    && lineage.entity_id.as_deref() == Some("finding:alpha")
                    && lineage.dedupe_key.as_deref()
                        == Some("workflow:repair-findings:finding:alpha")
                    && lineage.inputs["dispatch"]["client_context"]
                        .as_str()
                        .is_some_and(|context| context.contains("repair-findings"))
            }));
            assert!(result.value.controller.task_lineage.iter().any(|lineage| {
                lineage.run_id == "generic-run-finding_beta"
                    && lineage.entity_id.as_deref() == Some("finding:beta")
                    && lineage.dedupe_key.as_deref()
                        == Some("workflow:repair-findings:finding:beta")
            }));

            let observed = dispatch
                .observed_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            assert_eq!(observed.len(), 2);
            assert!(observed.iter().all(|request| {
                let dispatch = request["dispatch"].as_object().expect("dispatch object");
                dispatch.get("backend").is_none()
                    && dispatch.get("provider_config").is_none()
                    && dispatch.get("executor").is_none()
            }));
        });
    }

    #[test]
    fn route_finding_action_spawns_materialized_plan() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-route-finding".to_string(),
                phase: "triage".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");
            let plan = test_plan();
            record.record_action(
                AgentTaskLoopPolicyAction::RouteFinding {
                    finding: AgentTaskLoopFindingPacket {
                        finding_id: "finding-1".to_string(),
                        severity: "high".to_string(),
                        summary: "broken".to_string(),
                        owner: None,
                        source_transformer: None,
                        reproduction_key: Some("repo-loop-gap".to_string()),
                        lineage: Vec::new(),
                        payload: Value::Null,
                    },
                    dedupe_key: "finding:repo-loop-gap".to_string(),
                    entity_id: None,
                    request_template: json!({
                        "mode": "run_plan",
                        "run_id": "controller-service-route-finding-a",
                        "plan": plan,
                    }),
                },
                "route finding",
            );
            controller::write_controller(&record).expect("controller written");

            let result = run_next(
                "loop-service-route-finding",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("route finding executed");

            assert_eq!(result.exit_code, 0);
            let loaded =
                controller::load_controller("loop-service-route-finding").expect("controller");
            assert!(loaded.entities.contains_key("finding:repo-loop-gap"));
            assert_eq!(
                loaded.task_lineage[0].run_id,
                "controller-service-route-finding-a"
            );
        });
    }

    #[test]
    fn apply_event_persists_actions_and_keeps_event_envelope() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-event".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let report = apply_event(ControllerApplyEventRequest {
                loop_id: "loop-service-event".to_string(),
                event_type: "task.completed".to_string(),
                event_id: None,
                event_key: None,
                entity_id: None,
                payload: Value::Null,
            })
            .expect("event applied");

            assert_eq!(report.schema, APPLY_EVENT_RESULT_SCHEMA);
            assert!(!report
                .controller
                .history
                .iter()
                .all(|event| event.event_type == "controller.action.claimed"));
        });
    }
}
