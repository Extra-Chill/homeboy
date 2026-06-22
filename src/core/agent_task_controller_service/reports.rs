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
    pub execution: Option<Value>,
    pub controller: AgentTaskLoopControllerRecord,
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
