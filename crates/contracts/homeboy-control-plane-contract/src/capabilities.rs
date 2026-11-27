//! `homeboy/control-plane-capabilities/v1` — which resources this build serves.

use serde::{Deserialize, Serialize};

pub const CONTROL_PLANE_CAPABILITIES_SCHEMA: &str = "homeboy/control-plane-capabilities/v1";

/// Pure serializable declaration of control-plane resources, operations, and
/// compatibility. `operations` is the truthful surface: it lists what this
/// build/transport actually serves, never mutations that are not wired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneCapabilities {
    pub schema: String,
    pub resources: Vec<ControlPlaneResource>,
    #[serde(default)]
    pub operations: Vec<ControlPlaneOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility_windows: Vec<ControlPlaneCompatibilityWindow>,
}

/// A superseded serialized projection retained until one declared release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneCompatibilityWindow {
    pub projection: String,
    pub replacement_schema: String,
    pub remove_in: String,
}

/// An operation this build/transport actually serves.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneOperation {
    GetCapabilities,
    ListMissions,
    GetMission,
    SubmitRun,
    ListRuns,
    GetRun,
    ListRunTasks,
    GetRunTask,
    ListTaskAttempts,
    GetTaskAttempt,
    ListAttemptExecutions,
    GetAttemptExecution,
    ListRunArtifacts,
    GetRunArtifact,
    RegisterRunArtifact,
    ListRunEvidence,
    GetRunEvidence,
    RegisterRunEvidence,
    ListRunExternalReferences,
    GetRunExternalReference,
    RegisterRunExternalReference,
    GetRunReview,
    GetRunEvents,
    GetRunEventRetention,
    AppendRunEvent,
    ExecuteRunAction,
    #[serde(other)]
    Unknown,
}

/// A resource identity this build serves.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneResource {
    Mission,
    Run,
    Review,
    Task,
    Attempt,
    Execution,
    Artifact,
    Evidence,
    ExternalReference,
    ProviderSession,
    Event,
}

impl ControlPlaneCapabilities {
    /// Describe the operations wired by a runtime. The contract does not infer
    /// build capabilities itself.
    pub fn new(
        resources: Vec<ControlPlaneResource>,
        operations: Vec<ControlPlaneOperation>,
    ) -> Self {
        Self {
            schema: CONTROL_PLANE_CAPABILITIES_SCHEMA.to_string(),
            resources,
            operations,
            compatibility_windows: Vec::new(),
        }
    }

    pub fn with_compatibility_windows(
        mut self,
        compatibility_windows: Vec<ControlPlaneCompatibilityWindow>,
    ) -> Self {
        self.compatibility_windows = compatibility_windows;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlPlaneCapabilities, ControlPlaneCompatibilityWindow, ControlPlaneOperation,
        ControlPlaneResource, CONTROL_PLANE_CAPABILITIES_SCHEMA,
    };

    #[test]
    fn capabilities_wire_shape_omits_empty_compatibility_windows() {
        let document = ControlPlaneCapabilities::new(
            vec![ControlPlaneResource::Run],
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun,
            ],
        );
        assert_eq!(
            serde_json::to_value(&document).expect("serialize"),
            serde_json::json!({
                "schema": CONTROL_PLANE_CAPABILITIES_SCHEMA,
                "resources": ["run"],
                "operations": ["get_capabilities", "get_run"],
            })
        );

        let windowed = document.with_compatibility_windows(vec![ControlPlaneCompatibilityWindow {
            projection: "homeboy/legacy-run/v1".to_string(),
            replacement_schema: "homeboy/control-plane-run/v1".to_string(),
            remove_in: "0.370.0".to_string(),
        }]);
        let value = serde_json::to_value(&windowed).expect("serialize");
        assert_eq!(value["compatibility_windows"][0]["remove_in"], "0.370.0");
    }

    #[test]
    fn capabilities_tolerate_future_operation_names() {
        let document: ControlPlaneCapabilities = serde_json::from_value(serde_json::json!({
            "schema": CONTROL_PLANE_CAPABILITIES_SCHEMA,
            "resources": ["run"],
            "operations": ["get_run", "future_operation"],
            "future_negotiation": { "major": 2 }
        }))
        .expect("forward-compatible capabilities");
        assert_eq!(
            document.operations,
            vec![
                ControlPlaneOperation::GetRun,
                ControlPlaneOperation::Unknown
            ]
        );
    }
}
