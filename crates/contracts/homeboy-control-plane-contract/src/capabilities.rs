//! `homeboy/control-plane-capabilities/v1` — which resources this build serves.

use serde::{Deserialize, Serialize};

pub const CONTROL_PLANE_CAPABILITIES_SCHEMA: &str = "homeboy/control-plane-capabilities/v1";
pub const LEGACY_COMPATIBILITY_MINOR_VERSIONS: u32 = 1;

/// Pure serializable declaration of control-plane resources, operations, and
/// compatibility. `operations` is the truthful surface: it lists what this
/// build/transport actually serves, never mutations that are not wired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneCapabilities {
    pub schema: String,
    pub resources: Vec<ControlPlaneResource>,
    #[serde(default)]
    pub operations: Vec<ControlPlaneOperation>,
    pub compatibility: CompatibilityWindow,
}

/// An operation this build/transport actually serves.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneOperation {
    GetCapabilities,
    GetRun,
}

/// Declared legacy-compatibility window of one minor version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityWindow {
    pub legacy_minor_versions: u32,
}

/// A resource identity this build serves.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneResource {
    Mission,
    Run,
    Task,
    Attempt,
    Execution,
    ProviderSession,
}

impl ControlPlaneCapabilities {
    pub fn this_build() -> Self {
        Self {
            schema: CONTROL_PLANE_CAPABILITIES_SCHEMA.to_string(),
            resources: vec![ControlPlaneResource::Run],
            operations: vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun,
            ],
            compatibility: CompatibilityWindow {
                legacy_minor_versions: LEGACY_COMPATIBILITY_MINOR_VERSIONS,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlPlaneCapabilities, ControlPlaneOperation, ControlPlaneResource,
        CONTROL_PLANE_CAPABILITIES_SCHEMA, LEGACY_COMPATIBILITY_MINOR_VERSIONS,
    };

    #[test]
    fn capabilities_document_serializes_schema_and_compatibility_window() {
        let document = ControlPlaneCapabilities::this_build();
        let value = serde_json::to_value(&document).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_CAPABILITIES_SCHEMA);
        assert_eq!(value["compatibility"]["legacy_minor_versions"], 1);
        assert_eq!(
            document.compatibility.legacy_minor_versions,
            LEGACY_COMPATIBILITY_MINOR_VERSIONS
        );
        assert_eq!(value["resources"], serde_json::json!(["run"]));
        assert_eq!(
            value["operations"],
            serde_json::json!(["get_capabilities", "get_run"])
        );
        assert!(
            !document.operations.iter().any(|operation| !matches!(
                operation,
                ControlPlaneOperation::GetCapabilities | ControlPlaneOperation::GetRun
            )),
            "capabilities must not advertise unwired mutations"
        );
        let decoded: ControlPlaneCapabilities = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, document);
        assert_eq!(decoded.resources, vec![ControlPlaneResource::Run]);
        assert_eq!(
            decoded.operations,
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun
            ]
        );
    }
}
