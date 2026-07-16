//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

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

/// Operator-chosen strategy for handling an existing controller whose persisted
/// spec fingerprint differs from the supplied spec on `from-spec --resume`.
///
/// This protects proof reruns from silently inheriting stale controller/loop
/// state (see #6123). When the supplied spec matches the persisted fingerprint
/// the resolution is irrelevant and the run resumes normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControllerResumeStateResolution {
    /// No explicit override. Refuse to resume when persisted state is stale.
    #[default]
    Guard,
    /// Discard the persisted controller record entirely and re-create it from the spec.
    Replace,
    /// Resume a copy under a derived `loop_id`, leaving the original untouched.
    Fork,
    /// Resume the persisted controller as-is, accepting the stale/mismatched state.
    ResumeExisting,
    /// One-flag safe proof-run mode: reset stale persisted state automatically,
    /// re-deriving isolated run-scoped controller state from the spec without
    /// any manual state cleanup. Behaves like [`Replace`] for stale state but is
    /// surfaced as its own keyword so proof-run evidence reads cleanly (#6221).
    ///
    /// [`Replace`]: ControllerResumeStateResolution::Replace
    ReconcileStale,
}

impl ControllerResumeStateResolution {
    /// CLI-facing keyword for operator-readable run evidence.
    pub fn keyword(self) -> &'static str {
        match self {
            ControllerResumeStateResolution::Guard => "guard",
            ControllerResumeStateResolution::Replace => "replace",
            ControllerResumeStateResolution::Fork => "fork",
            ControllerResumeStateResolution::ResumeExisting => "resume-existing",
            ControllerResumeStateResolution::ReconcileStale => "reconcile-stale",
        }
    }
}

/// Request to compile a declarative controller spec without mutating controller state.
#[derive(Debug, Clone)]
pub struct ControllerPlanRequest {
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
        deserialize_with = "deserialize_artifact_graph_edges",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub artifact_graph: Vec<AgentTaskRepoLoopSpecArtifactGraphEdge>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_out: Option<AgentTaskRepoLoopSpecFanOut>,
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
    #[serde(default, alias = "execution", skip_serializing_if = "Value::is_null")]
    pub runtime_execution: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub inputs: Value,
}

/// Generic fan-out declaration for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecFanOut {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_non_empty: bool,
    #[serde(default, alias = "items", skip_serializing_if = "Vec::is_empty")]
    pub entity_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_fast: Option<bool>,
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

/// Directed artifact edge declared by a repo loop spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRepoLoopSpecArtifactGraphEdge {
    pub artifact_id: String,
    #[serde(alias = "producer", alias = "producer_workflow_id")]
    pub from_workflow_id: String,
    #[serde(alias = "consumer", alias = "consumer_workflow_id")]
    pub to_workflow_id: String,
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

pub(super) fn deserialize_agents<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecAgent>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "agent_id")
}

pub(super) fn deserialize_tools<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecTool>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "tool_id")
}

pub(super) fn deserialize_abilities<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecAbility>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "ability_id")
}

pub(super) fn deserialize_workflows<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecWorkflow>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "workflow_id")
}

pub(super) fn deserialize_artifacts<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecArtifact>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "artifact_id")
}

pub(super) fn deserialize_artifact_graph_edges<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecArtifactGraphEdge>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let edges = match value {
        Value::Array(items) => Value::Array(items),
        Value::Object(mut object) => object.remove("edges").ok_or_else(|| {
            de::Error::custom("artifact_graph object must contain an edges array")
        })?,
        other => {
            return Err(de::Error::custom(format!(
                "expected array or object with edges for artifact_graph, got {}",
                json_type_name(&other)
            )))
        }
    };
    serde_json::from_value(edges).map_err(de::Error::custom)
}

pub(super) fn deserialize_dependencies<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecDependency>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "dependency_id")
}

pub(super) fn deserialize_gates<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecGate>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "gate_id")
}

pub(super) fn deserialize_metrics<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentTaskRepoLoopSpecMetric>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_vec_or_keyed_map(deserializer, "metric_id")
}

pub(super) fn deserialize_vec_or_keyed_map<'de, D, T>(
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

pub(super) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn default_loop_spec_phase() -> String {
    "init".to_string()
}

pub(super) fn default_loop_spec_config_version() -> String {
    "v1".to_string()
}

pub(super) fn default_loop_spec_event_type() -> String {
    "loop_spec.applied".to_string()
}
