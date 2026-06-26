//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

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
    /// Stale-state resolution evidence for `from-spec --resume`. `None` for the
    /// non-resume path, where stale-state guarding does not apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_state: Option<ControllerResumeStateReport>,
}

/// Operator-facing record of how an existing controller's persisted state was
/// reconciled with the supplied spec during `from-spec --resume`.
///
/// Spells out whether Homeboy is creating, resuming, replacing, or forking
/// controller state so proof reruns never have to rely on loop-id folklore
/// (#6123).
#[derive(Debug, Clone, Serialize)]
pub struct ControllerResumeStateReport {
    /// One of `creating`, `resuming`, `replacing`, or `forking`.
    pub action: &'static str,
    /// Operator-selected resolution keyword (`guard`, `replace`, `fork`, `resume-existing`).
    pub resolution: &'static str,
    /// Loop id the controller state was actually applied to (differs for forks).
    pub loop_id: String,
    /// Loop id supplied by the spec before any fork derivation.
    pub requested_loop_id: String,
    /// Persisted controller record path that was inspected for stale state.
    pub controller_path: String,
    /// Spec fingerprint of the run being applied.
    pub spec_fingerprint: String,
    /// Persisted spec fingerprint found on the existing controller, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_spec_fingerprint: Option<String>,
    /// True when an existing controller was found for the requested loop id.
    pub existing_controller: bool,
    /// True when the persisted fingerprint matched the supplied spec.
    pub fingerprint_match: bool,
}

/// Typed report returned by `controller plan`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerPlanReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub spec_fingerprint: String,
    pub plan: HomeboyPlan,
    pub actions: Vec<AgentTaskLoopPolicyActionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
}

/// Typed report returned by `compile_plan_from_spec`.
///
/// Carries the executable agent-task plan derived from a loop controller spec
/// (#5101): stages and inter-stage dependencies are compiled from the spec's
/// workflows and `artifact_flow` edges, and Homeboy-owned runtime artifacts are
/// surfaced as synthetic runtime stages so downstream callers do not hard-code
/// Homeboy internals.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerExecutablePlanReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub spec_fingerprint: String,
    /// Homeboy-owned runtime artifact ids the plan synthesizes (e.g. `static_validation_run`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_artifacts: Vec<String>,
    pub plan: HomeboyPlan,
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
    pub failure_summary: Option<ControllerActionFailureSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_evidence: Option<ControllerActionRuntimeEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<Value>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Generic runtime evidence surfaced from runtime-backed controller action
/// execution results. The controller preserves provider-neutral ids and refs
/// already present in dispatch/runtime payloads without interpreting them.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerActionRuntimeEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_invocation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript_refs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result_refs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics_refs: Vec<Value>,
}

/// Concise failed-action context for controller resume operators.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerActionFailureSummary {
    pub action_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_phase: Option<String>,
    pub diagnostic: String,
}

/// Typed report returned by `resume`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerResumeReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub claimed: bool,
    pub stopped_reason: String,
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
/// The CLI adapter implements this to bridge controller-driven work into the
/// internal dispatch service. Callers that do not need dispatch mode can pass
/// [`NoopDispatchHook`].
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
